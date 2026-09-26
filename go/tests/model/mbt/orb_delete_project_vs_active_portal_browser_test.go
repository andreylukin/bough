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
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// odpAppendMeta writes the one "meta" entry a real project session's
// history file starts with (mode/project), the same shape
// orb_lifecycle_test.go's appendHistory writes for its own fake
// sessions: sessionMode (internal/serve/orbs.go) reads it off exactly
// this, whatever process wrote it.
func odpAppendMeta(home, id, slug string) error {
	path := filepath.Join(home, ".bough", "history", id+".jsonl")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	b, err := json.Marshal(history.Entry{Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{
		"cwd": home, "mode": "project", "project": slug,
	}})
	if err != nil {
		return err
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.Write(append(b, '\n'))
	return err
}

// TestOrbDeleteProjectVsActivePortalBrowserServe is the backend of the
// browser walk of specs/orb_delete_project_vs_active_portal.fizz
// (go/tests/web/specs/model/orb_delete_project_vs_active_portal.spec.ts),
// not a test of its own: it skips unless MODEL_ODP_SERVE names a file.
//
// None of the spec's actions has a page control — a project directory
// vanishing and the reaper's sweep are never a button — so every action
// here is the same odpAdapter the Go MBT test drives (real project dirs,
// the real portal split, and the real reapVanishedProjectPortals via
// serve.API, reached in-process rather than by waiting for the reaper's
// own multi-minute tick). What the browser walk adds is the page:
// odpAdapter.Init's session is given a real "meta" history entry with
// mode "project" (the same shape a real project session's first entry
// has), so the control room's Portal button renders for it off
// state.json's Portals list — the same button orb_portal_open_close_race
// reads — and every node is read off that button, never off a value the
// adapter remembers setting.
//
// Control routes, one walk at a time:
//
//	POST /model/init            a fresh session, given a project meta entry
//	POST /model/act/{action}    play one of the spec's actions
//	POST /model/cleanup         end the walk
//
// Each answers the role's state as odpAdapter.GetState reports it (bare
// field names), or an error when the action is not enabled.
func TestOrbDeleteProjectVsActivePortalBrowserServe(t *testing.T) {
	out := os.Getenv("MODEL_ODP_SERVE")
	if out == "" {
		t.Skip("the browser walk's backend; orb_delete_project_vs_active_portal.spec.ts sets MODEL_ODP_SERVE")
	}
	a := newOdpAdapter(t)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		a.api.ServeHTTP(w, r)
	}))
	defer srv.Close()

	var mu sync.Mutex
	reply := func(w http.ResponseWriter, body map[string]any, err error) {
		w.Header().Set("Content-Type", "application/json")
		if err != nil {
			w.WriteHeader(http.StatusConflict)
			body["error"] = err.Error()
		}
		json.NewEncoder(w).Encode(body)
	}
	record := func() (map[string]any, error) { return a.GetState() }

	mux := http.NewServeMux()
	mux.HandleFunc("POST /model/init", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		err := a.Init()
		if err == nil {
			// The same shape a real project session's first entry has
			// (orb_lifecycle_test.go's appendHistory), so sessionMode
			// (internal/serve/orbs.go) reads this session as a.slug's
			// project session and the page's Portal button renders for
			// it off row.orb.
			err = odpAppendMeta(a.home, a.id, a.slug)
		}
		body := map[string]any{"id": a.id}
		if err == nil {
			body["state"], err = record()
		}
		reply(w, body, err)
	})
	mux.HandleFunc("POST /model/act/{action}", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		name := r.PathValue("action")
		f, ok := odpActions["Portal"][name]
		if !ok {
			http.Error(w, "no backend action "+name, http.StatusNotFound)
			return
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
	mux.HandleFunc("POST /model/cleanup", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		reply(w, map[string]any{}, a.Cleanup())
	})
	ctl := httptest.NewServer(mux)
	defer ctl.Close()

	b, _ := json.Marshal(map[string]string{"ui": srv.URL, "model": ctl.URL, "home": a.home})
	if err := writeFileAtomic(out, b); err != nil {
		t.Fatal(err)
	}
	io.Copy(io.Discard, os.Stdin)
}
