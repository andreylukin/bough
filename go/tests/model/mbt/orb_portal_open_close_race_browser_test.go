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

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
)

// TestOrbPortalOpenCloseRaceBrowserServe is the backend of the browser
// walk of specs/orb_portal_open_close_race.fizz
// (go/tests/web/specs/model/orb_portal_open_close_race.spec.ts), not a
// test of its own: it skips unless MODEL_PORTAL_SERVE names a file.
//
// None of the spec's four actions has a page control — opening or
// closing a portal is a tool call the orb's agent makes over its own
// Host, never a button the browser can click, and a restart or an
// external stop is the reaper's or another client's doing — so every
// action here is the same portalAdapter the Go MBT test drives
// (internal/orb's real state and portal code). What the browser walk
// adds is the page: portalAdapter.Init's session is relabelled a
// project session (a fake project.yml, its first history entry
// rewritten to mode "project") so the Portal button and pane in
// go/internal/serve/web render for it, exactly as a real project
// session's would, and every node is read off the DOM through that
// pane, including the portal frame's own body (the fake backend's
// "ip1"/"ip2" label), which is the real dial a browser walking the
// product would see.
//
// portalAdapter.Init() mints a fresh session per walk on the one serve
// this starts (servetest.Start's real `bough serve --run`, never
// restarted: no action here restarts serve, only the fake orb inside
// it), so its URL is the page's home for the whole run.
//
// Control routes, one walk at a time:
//
//	POST /model/init            a fresh session, relabelled project
//	POST /model/act/{action}    play one of the spec's four actions
//	POST /model/cleanup         close a portal a walk left open
//
// Each answers the role's state as portalAdapter.GetState reports it
// (bare field names), or an error when the action is not enabled.
func TestOrbPortalOpenCloseRaceBrowserServe(t *testing.T) {
	out := os.Getenv("MODEL_PORTAL_SERVE")
	if out == "" {
		t.Skip("the browser walk's backend; orb_portal_open_close_race.spec.ts sets MODEL_PORTAL_SERVE")
	}
	a := newPortalAdapter(t)
	defer a.closeFakes()

	const slug = "portalp"
	if err := projectdef.WriteFile(a.s.Home, slug, projectdef.FileYAML, "name: "+slug+"\nrepos:\n  - path: "+a.s.Home+"\n"); err != nil {
		t.Fatal(err)
	}

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
			err = markProjectSession(a.s.Home, a.id, slug)
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
		f, ok := portalActions["Portal"][name]
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

	b, _ := json.Marshal(map[string]string{"ui": a.s.URL, "model": ctl.URL, "home": a.s.Home})
	if err := writeFileAtomic(out, b); err != nil {
		t.Fatal(err)
	}
	io.Copy(io.Discard, os.Stdin)
}

// markProjectSession rewrites session id's first ("meta") history entry
// so sessionMode (go/internal/serve/orbs.go) reads it as a project
// session of slug: a page's row.mode gates the Portal button and Stop
// orb on it (RuntimeStrip in web/src/app.tsx), and only a project
// session's row carries an orb view at all.
func markProjectSession(home, id, slug string) error {
	path := filepath.Join(home, ".bough", "history", id+".jsonl")
	entries, err := history.Read(path)
	if err != nil {
		return err
	}
	for i := range entries {
		if entries[i].Kind != "meta" {
			continue
		}
		if entries[i].Data == nil {
			entries[i].Data = map[string]any{}
		}
		entries[i].Data["mode"] = "project"
		entries[i].Data["project"] = slug
		break
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_TRUNC|os.O_CREATE, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	for _, e := range entries {
		b, err := json.Marshal(e)
		if err != nil {
			return err
		}
		if _, err := f.Write(append(b, '\n')); err != nil {
			return err
		}
	}
	return nil
}
