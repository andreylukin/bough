//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// TestOrbRemoveDataSafetyBrowserServe is the backend of the browser walk
// of specs/orb_remove_data_safety.fizz
// (go/tests/web/specs/model/orb_remove_data_safety.spec.ts), not a test
// of its own: it skips unless MODEL_ORDS_SERVE names a file.
//
// Like TestOrbLifecycleBrowserServe: the walk needs serve on a fake
// runtime (a `bough serve` process opens the host's), the owner's
// state.json writes, real git and the CLI's library calls, all of which
// the MBT adapter already plays. So this runs that adapter's serve, which
// serves the page, plus a control server, and writes both URLs to
// $MODEL_ORDS_SERVE. It serves until its stdin closes.
//
// Control routes, one walk at a time:
//
//	POST /model/init                 a fresh walk (the spec's Init)
//	POST /model/act/{action}         play one action the page has no control for
//	POST /model/begin/{action}       an action the page takes: gate it on the require
//	POST /model/record?code={status} the page's request answered status: judge it
//	POST /model/cleanup              end the walk
//
// Each answers the role's state as the adapter observes it, or an error.
func TestOrbRemoveDataSafetyBrowserServe(t *testing.T) {
	out := os.Getenv("MODEL_ORDS_SERVE")
	if out == "" {
		t.Skip("the browser walk's backend; orb_remove_data_safety.spec.ts sets MODEL_ORDS_SERVE")
	}
	a := newOrdsAdapter(t)
	// The page asks serve about models, skills, hooks and the wiki, and
	// those read $HOME: never the real one.
	t.Setenv("HOME", a.home)

	var mu sync.Mutex
	// pending finishes the page action begin opened, given the status its
	// request got.
	var pending func(code int) error
	reply := func(w http.ResponseWriter, st map[string]any, err error) {
		w.Header().Set("Content-Type", "application/json")
		if err != nil {
			w.WriteHeader(http.StatusConflict)
			o, _ := a.observe()
			json.NewEncoder(w).Encode(map[string]any{"error": err.Error(), "state": o.state()})
			return
		}
		json.NewEncoder(w).Encode(map[string]any{"state": st})
	}
	// journal is ordsRecorded's second half: the state after the step,
	// into the walk's trace.
	journal := func(name string) (map[string]any, error) {
		st, err := a.GetState()
		if err != nil {
			return nil, err
		}
		a.steps = append(a.steps, tracecheck.Step{Action: "Orb#0." + name, State: ordsQualify(st)})
		return st, nil
	}

	// last is the state the last step journaled, by bare field name.
	last := func() map[string]any {
		out := map[string]any{}
		for k, v := range a.steps[len(a.steps)-1].State {
			out[strings.TrimPrefix(k, "Orb#0.")] = v
		}
		return out
	}

	mux := http.NewServeMux()
	mux.HandleFunc("POST /model/init", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		pending = nil
		if err := a.Init(); err != nil {
			reply(w, nil, err)
			return
		}
		st, err := a.GetState()
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]any{"id": a.id, "slug": a.slug, "state": st, "error": errString(err)})
	})
	mux.HandleFunc("POST /model/act/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		f, ok := ordsActions["Orb"][name]
		if !ok {
			http.Error(w, "no action "+name, http.StatusNotFound)
			return
		}
		if _, err := f(a, nil); err != nil {
			reply(w, nil, fmt.Errorf("%s: %w", name, err))
			return
		}
		if a.gate.off {
			reply(w, nil, fmt.Errorf("%s is not enabled", name))
			return
		}
		reply(w, last(), nil)
	})
	mux.HandleFunc("POST /model/begin/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		var ok bool
		var err error
		switch name {
		case "OpenRemovePlan":
			ok, err = a.openBegin()
			pending = a.openEnd
		case "Cancel":
			ok, err = a.cancelBegin()
			pending = func(int) error { a.flow = "idle"; return nil }
		case "Confirm", "ConfirmRuntimeRemoveFails":
			var pre ordsFields
			pre, ok, err = a.confirmBegin(name == "ConfirmRuntimeRemoveFails")
			pending = func(code int) error {
				a.rt.failRemove.Store(false)
				return a.confirmEnd(pre, code)
			}
		default:
			http.Error(w, "the page has no control for "+name, http.StatusNotFound)
			return
		}
		if err == nil && !ok {
			err = fmt.Errorf("%s is not enabled", name)
		}
		if err != nil {
			pending = nil
			reply(w, nil, err)
			return
		}
		pending = func(f func(int) error) func(int) error {
			return func(code int) error {
				if err := f(code); err != nil {
					return err
				}
				_, err := journal(name)
				return err
			}
		}(pending)
		reply(w, nil, nil)
	})
	mux.HandleFunc("POST /model/record", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		if pending == nil {
			reply(w, nil, fmt.Errorf("record with no page action begun"))
			return
		}
		f := pending
		pending = nil
		code, _ := strconv.Atoi(r.URL.Query().Get("code"))
		if err := f(code); err != nil {
			reply(w, nil, err)
			return
		}
		reply(w, last(), nil)
	})
	mux.HandleFunc("POST /model/cleanup", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		pending = nil
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
