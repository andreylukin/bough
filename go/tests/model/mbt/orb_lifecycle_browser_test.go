//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
)

// TestOrbLifecycleBrowserServe is the backend of the browser walk of
// specs/orb_lifecycle.fizz (go/tests/web/specs/model/orb_lifecycle.spec.ts),
// not a test of its own: it skips unless MODEL_ORB_SERVE names a file.
//
// The walk needs serve on container.Fake, which only an in-process serve
// has (a `bough serve` process opens the host's runtime), and it needs
// the child's state.json writes played, which the MBT adapter already
// does. So this runs that adapter's serve — the page is served by the
// same API — plus a small control server the spec drives the child's
// actions through, and writes both URLs to $MODEL_ORB_SERVE. It serves
// until its stdin closes (the spec's worker ends).
//
// Control routes, one walk at a time:
//
//	POST /model/init            a fresh walk: new session, no orb
//	POST /model/act/{action}    play one child or clock action, record it
//	POST /model/begin/{action}  an action the page takes: check its require
//	POST /model/record          after the page acted: record the step
//	POST /model/cleanup         end the walk's owner
//
// Each answers the role's state as the adapter observes it (bare field
// names), or an error when the action is not enabled or serve's API
// disagrees with the spec's views of that state (checkViews).
func TestOrbLifecycleBrowserServe(t *testing.T) {
	out := os.Getenv("MODEL_ORB_SERVE")
	if out == "" {
		t.Skip("the browser walk's backend; orb_lifecycle.spec.ts sets MODEL_ORB_SERVE")
	}
	a := newOrbAdapter(t)
	// The page asks serve about models, skills, hooks and the wiki, and
	// those read $HOME: never the real one.
	t.Setenv("HOME", a.home)
	// Only the walk's Refresh re-reads the runtime (see SetRunningTTL).
	a.api.SetRunningTTL(time.Hour)

	var mu sync.Mutex
	reply := func(w http.ResponseWriter, st map[string]any, err error) {
		w.Header().Set("Content-Type", "application/json")
		if err != nil {
			w.WriteHeader(http.StatusConflict)
			json.NewEncoder(w).Encode(map[string]any{"error": err.Error(), "state": st})
			return
		}
		json.NewEncoder(w).Encode(map[string]any{"state": st})
	}
	// record is GetState: the step just taken goes into the walk's trace
	// and serve's views are checked against it.
	record := func() (map[string]any, error) {
		// The portal's listener stands in for the owner's: a Remove the
		// page sent killed serve's child, and the listener goes with it.
		if o, err := a.observe(); err == nil && o.owner == "none" {
			a.closePortal()
		}
		st, err := a.GetState()
		if err != nil {
			o, _ := a.observe()
			return o.state(), err
		}
		return st, nil
	}
	enabled := func(name string) (orbFields, bool, error) {
		o, err := a.observe()
		if err != nil {
			return o, false, err
		}
		a.probe, a.enabled = &o, nil
		orbActions["Orb"][name](a, nil)
		a.probe = nil
		return o, slices.Contains(a.enabled, name), nil
	}
	known := func(w http.ResponseWriter, name string) bool {
		if _, ok := orbActions["Orb"][name]; !ok {
			http.Error(w, "no action "+name, http.StatusNotFound)
			return false
		}
		return true
	}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /model/init", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if err := a.Init(); err != nil {
			reply(w, nil, err)
			return
		}
		// The project page is where Remove orb lives, and it lists only
		// projects with a definition.
		if err := writeFileAtomic(filepath.Join(projectdef.Root(a.home), a.slug, projectdef.FileYAML),
			[]byte("name: "+a.slug+"\n")); err != nil {
			reply(w, nil, err)
			return
		}
		st, err := record()
		w.Header().Set("Content-Type", "application/json")
		if err != nil {
			w.WriteHeader(http.StatusConflict)
		}
		json.NewEncoder(w).Encode(map[string]any{"id": a.id, "slug": a.slug, "state": st, "error": errString(err)})
	})
	mux.HandleFunc("POST /model/act/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		if !known(w, name) {
			return
		}
		if _, err := orbActions["Orb"][name](a, nil); err != nil {
			reply(w, nil, fmt.Errorf("%s: %w", name, err))
			return
		}
		if a.action == "" {
			o, _ := a.observe()
			reply(w, o.state(), fmt.Errorf("%s is not enabled", name))
			return
		}
		st, err := record()
		reply(w, st, err)
	})
	mux.HandleFunc("POST /model/begin/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		if !known(w, name) {
			return
		}
		o, ok, err := enabled(name)
		if err == nil && !ok {
			err = fmt.Errorf("%s is not enabled", name)
		}
		if err == nil {
			a.action = name
		}
		reply(w, o.state(), err)
	})
	mux.HandleFunc("POST /model/record", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		st, err := record()
		reply(w, st, err)
	})
	mux.HandleFunc("POST /model/cleanup", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		reply(w, nil, a.Cleanup())
	})
	ctl := httptest.NewServer(mux)
	defer ctl.Close()

	b, _ := json.Marshal(map[string]string{"ui": a.srv.URL, "model": ctl.URL, "home": a.home})
	if err := writeFileAtomic(out, b); err != nil {
		t.Fatal(err)
	}
	io.Copy(io.Discard, os.Stdin)
}

func errString(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
