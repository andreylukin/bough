//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"sync"
	"testing"
)

// lendPage is what the browser walk does through the page itself (the
// project's orb page: the project.yml editor, a session row's Stop orb
// and Remove…), each with the spec's require.
var lendPage = map[string]func(lendFields) bool{
	"AddDir":    func(f lendFields) bool { return f.d == "none" && f.host },
	"AddHazard": func(f lendFields) bool { return f.d == "none" && f.host },
	"AddDenied": func(f lendFields) bool { return f.d == "none" },
	"RemoveDir": func(f lendFields) bool { return f.d != "none" },
	"StopOrb":   func(f lendFields) bool { return f.live && f.ctr == "running" },
	"RemoveOrb": func(f lendFields) bool { return !f.live && f.ctr == "stopped" },
}

// TestIdentityLendingBrowserServe is the backend of the browser walk of
// specs/identity_lending.fizz (go/tests/web/specs/model/identity_lending.spec.ts),
// not a test of its own: it skips unless MODEL_LEND_SERVE names a file.
//
// As for orb_lifecycle, the page needs serve on a fake runtime, which
// only an in-process serve has, and the session's process, the host's
// $HOME and the guest's writes played, which the MBT adapter does. So
// this runs that adapter's serve (the page is served by the same API)
// and a control server, and writes both URLs to $MODEL_LEND_SERVE. It
// serves until its stdin closes.
//
// Control routes, one walk at a time:
//
//	POST /model/init            a fresh walk: a new project and session
//	POST /model/act/{action}    play one action the page has no control for
//	POST /model/begin/{action}  an action the page takes: check its require
//	POST /model/record          after the page acted: its drift, the step
//	POST /model/cleanup         end the walk's session
//
// Each answers the role's state as the adapter observes it.
func TestIdentityLendingBrowserServe(t *testing.T) {
	out := os.Getenv("MODEL_LEND_SERVE")
	if out == "" {
		t.Skip("the browser walk's backend; identity_lending.spec.ts sets MODEL_LEND_SERVE")
	}
	a := newLendAdapter(t)
	// The page asks serve about models, skills, hooks and the wiki, and
	// those read $HOME: never the real one.
	t.Setenv("HOME", a.s.home)

	var mu sync.Mutex
	var before lendFields
	reply := func(w http.ResponseWriter, st map[string]any, err error, extra ...any) {
		w.Header().Set("Content-Type", "application/json")
		body := map[string]any{"state": st}
		for i := 0; i+1 < len(extra); i += 2 {
			body[extra[i].(string)] = extra[i+1]
		}
		if err != nil {
			w.WriteHeader(http.StatusConflict)
			body["error"] = err.Error()
		}
		json.NewEncoder(w).Encode(body)
	}
	observed := func() map[string]any {
		f, _ := a.observe()
		return f.state()
	}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /model/init", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if err := a.Init(); err != nil {
			reply(w, nil, err)
			return
		}
		st, err := a.GetState()
		reply(w, st, err, "id", a.id, "slug", a.slug)
	})
	mux.HandleFunc("POST /model/act/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		fn, ok := lendActions["Lend"][name]
		if !ok || lendPage[name] != nil {
			http.Error(w, "no backend action "+name, http.StatusNotFound)
			return
		}
		if _, err := fn(a, nil); err != nil {
			reply(w, observed(), fmt.Errorf("%s: %w", name, err))
			return
		}
		if a.gate.off {
			a.gate.reset()
			reply(w, observed(), fmt.Errorf("%s is not enabled", name))
			return
		}
		st, err := a.GetState()
		reply(w, st, err)
	})
	mux.HandleFunc("POST /model/begin/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		req, ok := lendPage[name]
		if !ok {
			http.Error(w, "no page action "+name, http.StatusNotFound)
			return
		}
		f, err := a.observe()
		if err == nil && !req(f) {
			err = fmt.Errorf("%s is not enabled", name)
		}
		if err == nil {
			before, a.action = f, name
			a.did[name]++
		}
		reply(w, f.state(), err)
	})
	// record is the adapter's own bookkeeping for the action the page
	// took (drift from a definition edit, none after a remove), then
	// GetState, which puts the step in the walk's journal.
	mux.HandleFunc("POST /model/record", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		var err error
		switch a.action {
		case "AddDir", "AddHazard", "RemoveDir":
			err = a.changed(before.want())
		case "RemoveOrb":
			a.drift = false
		case "AddDenied":
			// The refused save must change nothing.
			if f, ferr := a.observe(); ferr != nil || f != before {
				err = fmt.Errorf("AddDenied changed the state: %v, was %v (%v)", f.state(), before.state(), ferr)
			}
		}
		if err != nil {
			reply(w, observed(), err)
			return
		}
		st, err := a.GetState()
		reply(w, st, err)
	})
	mux.HandleFunc("POST /model/cleanup", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		reply(w, nil, a.Cleanup())
	})
	ctl := httptest.NewServer(mux)
	defer ctl.Close()

	b, _ := json.Marshal(map[string]string{"ui": a.s.srv.URL, "model": ctl.URL, "home": a.s.home})
	if err := writeFileAtomic(out, b); err != nil {
		t.Fatal(err)
	}
	io.Copy(io.Discard, os.Stdin)
}
