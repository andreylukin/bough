//go:build !windows

package mbt

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
)

// TestServeCloseChildrenOrbsBrowserServe is the backend of the browser
// walk of specs/serve_close_children_orbs.fizz
// (go/tests/web/specs/model/serve_close_children_orbs.spec.ts), not a
// test of its own: it skips unless MODEL_SCCO_SERVE names a file.
//
// The walk needs what the Go walk needs (container.Fake, stub children,
// "closing" inside sup.Close), so it runs that adapter. The page is
// served by the adapter's current API behind one front address that
// stays put across restarts, as launchd's serve keeps its port: while
// serve is down or closing the front is not listening at all, so the
// page meets a refused connection, exactly as it does when the real
// serve exits. It serves until its stdin closes.
//
// Control routes, one walk at a time:
//
//	POST /model/init             a fresh room on a fresh HOME
//	POST /model/act/{action}     play one action the page has no control for
//	POST /model/begin/ResumeA    check ResumeA's require before the page sends
//	POST /model/after/ResumeA    the page sent: wait for A's new turn, record
//	POST /model/cleanup          end the walk's serve and children
//
// Each answers the room's state as the adapter observes it (bare field
// names), or an error when the action is not enabled or serve's API
// disagrees with the spec's views (checkViews).
func TestServeCloseChildrenOrbsBrowserServe(t *testing.T) {
	out := os.Getenv("MODEL_SCCO_SERVE")
	if out == "" {
		t.Skip("the browser walk's backend; serve_close_children_orbs.spec.ts sets MODEL_SCCO_SERVE")
	}
	a := newSccoAdapter(t)
	// The page asks serve about models, skills, hooks and the wiki, and
	// those read $HOME: never the real one.
	t.Setenv("HOME", a.root)

	front, err := newSccoFront()
	if err != nil {
		t.Fatal(err)
	}
	defer front.down()
	// sync points the front at the serve the room has: listening on the
	// API while serve is up, refusing while it is not.
	syncFront := func() error {
		if a.serve == "up" && a.api != nil {
			return front.up(a.api)
		}
		front.down()
		return nil
	}

	var mu sync.Mutex
	var resumeTurns int
	reply := func(w http.ResponseWriter, body map[string]any, err error) {
		w.Header().Set("Content-Type", "application/json")
		if err != nil {
			w.WriteHeader(http.StatusConflict)
			body["error"] = err.Error()
		}
		json.NewEncoder(w).Encode(body)
	}
	record := func() (map[string]any, error) {
		if err := syncFront(); err != nil {
			return nil, err
		}
		return a.GetState()
	}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /model/init", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		err := a.Init()
		if err == nil {
			// The project's orb page lists only projects with a definition.
			err = writeFileAtomic(filepath.Join(projectdef.Root(a.home), sccoSlug, projectdef.FileYAML), []byte("name: "+sccoSlug+"\n"))
		}
		body := map[string]any{"p": a.p, "a": a.a, "b": a.b, "c": a.c, "home": a.home}
		if err == nil {
			body["state"], err = record()
		}
		reply(w, body, err)
	})
	mux.HandleFunc("POST /model/act/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		f, ok := sccoActions["Room"][name]
		if !ok || name == "ResumeA" {
			http.Error(w, "no backend action "+name, http.StatusNotFound)
			return
		}
		// srv.Shutdown runs first: nothing answers from here on.
		if name == "Close" || name == "Crash" {
			front.down()
		}
		body := map[string]any{}
		_, err := f(a, nil)
		if err == nil && a.gate.off {
			err = fmt.Errorf("%s is not enabled", name)
		}
		if err == nil {
			body["state"], err = record()
		}
		reply(w, body, err)
	})
	mux.HandleFunc("POST /model/begin/ResumeA", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		o, err := a.observe()
		if err == nil && !a.gate.pass(a.serve == "up" && o.a == "t") {
			err = errors.New("ResumeA is not enabled")
		}
		resumeTurns = a.turns(a.a)
		reply(w, map[string]any{}, err)
	})
	mux.HandleFunc("POST /model/after/ResumeA", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		body := map[string]any{}
		err := a.resumed(resumeTurns)
		if err == nil {
			body["state"], err = record()
		}
		reply(w, body, err)
	})
	mux.HandleFunc("POST /model/cleanup", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		front.down()
		reply(w, map[string]any{}, a.Cleanup())
	})
	ctl := httptest.NewServer(mux)
	defer ctl.Close()

	b, _ := json.Marshal(map[string]string{"ui": "http://" + front.addr, "model": ctl.URL, "home": a.root})
	if err := writeFileAtomic(out, b); err != nil {
		t.Fatal(err)
	}
	io.Copy(io.Discard, os.Stdin)
}

// sccoFront is serve's listening address, held across restarts.
type sccoFront struct {
	addr string
	h    http.Handler
	srv  *http.Server
}

func newSccoFront() (*sccoFront, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	addr := ln.Addr().String()
	ln.Close()
	return &sccoFront{addr: addr}, nil
}

// up serves h on the address, unless it already does. The address was
// just released by down, so a moment's retry covers a slow close.
func (f *sccoFront) up(h http.Handler) error {
	if f.srv != nil && f.h == h {
		return nil
	}
	f.down()
	var ln net.Listener
	var err error
	for deadline := time.Now().Add(5 * time.Second); ; time.Sleep(20 * time.Millisecond) {
		if ln, err = net.Listen("tcp", f.addr); err == nil || time.Now().After(deadline) {
			break
		}
	}
	if err != nil {
		return fmt.Errorf("serve's address %s: %w", f.addr, err)
	}
	f.h, f.srv = h, &http.Server{Handler: h}
	go f.srv.Serve(ln)
	return nil
}

// down stops listening and drops every open connection (the page's
// event streams too), as the process exiting does.
func (f *sccoFront) down() {
	if f.srv != nil {
		f.srv.Close()
		f.srv, f.h = nil, nil
	}
}
