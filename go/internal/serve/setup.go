package serve

// First-run setup for the web UI: which providers have a key, whether a
// folder is a checkout a session could write in, and a way to record a
// key without a terminal. Someone arriving from a README link lands on
// an empty page; without this they had to find ~/.bough/env on their own
// and guess why their first session could not edit anything.

import (
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/plugins/connect"
)

type setupProvider struct {
	Name string `json:"name"`
	Env  string `json:"env"`
	Set  bool   `json:"set"`
}

type setupFolder struct {
	Path   string `json:"path"`
	Exists bool   `json:"exists"`
	// Checkout is the git checkout a local session started here may
	// write; "" means the session would be read-only.
	Checkout string `json:"checkout,omitempty"`
}

func (a *API) envFile() string { return filepath.Join(a.home, ".bough", "env") }

// setup answers for ?cwd=, else the directory serve was started in:
// that is usually the repo the person ran `bough serve` from.
func (a *API) setup(w http.ResponseWriter, r *http.Request) {
	dir := strings.TrimSpace(r.URL.Query().Get("cwd"))
	if dir == "" {
		dir = a.start
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"providers": a.setupProviders(),
		"envFile":   a.envFile(),
		"home":      a.home,
		"folder":    a.setupFolder(dir),
	})
}

// writableRoot is what the launcher grants a local session started in
// cwd (applyDefaultWriteRoot), so the composer badge can say "edits
// <repo>" instead of calling every local session read-only.
func (a *API) writableRoot(mode, cwd string) string {
	if mode == "project" || cwd == "" {
		return ""
	}
	return iorb.CheckoutRoot(cwd, a.home)
}

func (a *API) setupFolder(dir string) setupFolder {
	if rest, ok := strings.CutPrefix(dir, "~"); ok && (rest == "" || rest[0] == '/') {
		dir = a.home + rest
	}
	f := setupFolder{Path: dir}
	if dir == "" {
		return f
	}
	if st, err := os.Stat(dir); err == nil && st.IsDir() {
		f.Exists = true
		f.Checkout = iorb.CheckoutRoot(dir, a.home)
	}
	return f
}

// setupProviders reports a key as set when this process has it or the
// env file does: a session started from here reads both.
func (a *API) setupProviders() []setupProvider {
	file, _ := os.ReadFile(a.envFile())
	var out []setupProvider
	for _, p := range connect.Providers() {
		out = append(out, setupProvider{Name: p.Name, Env: p.Env, Set: a.getenv(p.Env) != "" || envFileSets(string(file), p.Env)})
	}
	return out
}

func envFileSets(file, env string) bool {
	for line := range strings.SplitSeq(file, "\n") {
		k, v, ok := strings.Cut(strings.TrimPrefix(strings.TrimSpace(line), "export "), "=")
		if ok && strings.TrimSpace(k) == env && strings.Trim(strings.TrimSpace(v), `"'`) != "" {
			return true
		}
	}
	return false
}

// setKey records a key the way /connect does, and sets it in this
// process so sessions serve starts from now on inherit it.
func (a *API) setKey(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Provider string `json:"provider"`
		Key      string `json:"key"`
	}
	if !decode(w, r, &body) {
		return
	}
	key := strings.TrimSpace(body.Key)
	if key == "" {
		writeErr(w, http.StatusBadRequest, errors.New("serve: api: key is empty"))
		return
	}
	for _, p := range connect.Providers() {
		if p.Name != body.Provider {
			continue
		}
		if err := connect.WriteKey(a.envFile(), p.Env, key); err != nil {
			writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: save key: %w", err))
			return
		}
		if err := a.setenv(p.Env, key); err != nil {
			writeErr(w, http.StatusInternalServerError, fmt.Errorf("serve: api: save key: %w", err))
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{"providers": a.setupProviders()})
		return
	}
	writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown provider %q", body.Provider))
}
