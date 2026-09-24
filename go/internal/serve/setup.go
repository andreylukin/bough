package serve

// First-run setup for the web UI: which providers have a key, whether a
// folder is a checkout a session could write in, and a way to record a
// key without a terminal. Someone arriving from a README link lands on
// an empty page; without this they had to find ~/.bough/env on their own
// and guess why their first session could not edit anything.

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

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

// dirs completes a folder path typed into the palette: the folders
// whose path starts with ?path=, each with the checkout it would edit.
// A pasted path used to become a first message in home, because
// nothing on the page could tell a folder from a prompt.
func (a *API) dirs(w http.ResponseWriter, r *http.Request) {
	typed := strings.TrimSpace(r.URL.Query().Get("path"))
	full := a.setupFolder(typed).Path
	parent, prefix := full, ""
	if !strings.HasSuffix(typed, "/") {
		parent, prefix = filepath.Dir(full), filepath.Base(full)
	}
	out := []setupFolder{}
	entries, _ := os.ReadDir(parent)
	for _, e := range entries {
		name := e.Name()
		// Dot folders only when asked for: ~/ is otherwise all caches.
		if !strings.HasPrefix(name, prefix) || name == prefix || strings.HasPrefix(name, ".") && !strings.HasPrefix(prefix, ".") {
			continue
		}
		if st, err := os.Stat(filepath.Join(parent, name)); err != nil || !st.IsDir() {
			continue
		}
		out = append(out, a.setupFolder(filepath.Join(parent, name)))
		if len(out) == 20 {
			break
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"folder": a.setupFolder(typed), "dirs": out})
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

func envFileSets(file, env string) bool { return envFileValue(file, env) != "" }

func envFileValue(file, env string) string {
	val := ""
	for line := range strings.SplitSeq(file, "\n") {
		k, v, ok := strings.Cut(strings.TrimPrefix(strings.TrimSpace(line), "export "), "=")
		if ok && strings.TrimSpace(k) == env {
			val = strings.Trim(strings.TrimSpace(v), `"'`)
		}
	}
	return val
}

// checkURL is an authenticated read per provider that costs no tokens.
var checkURL = map[string]string{
	"anthropic":  "https://api.anthropic.com/v1/models",
	"openai":     "https://api.openai.com/v1/models",
	"openrouter": "https://openrouter.ai/api/v1/key",
	"cerebras":   "https://api.cerebras.ai/v1/models",
}

// keyChecker is how serve asks providers about keys. A non-empty base
// (BOUGH_SETUP_CHECK_URL) is asked instead of every provider's own
// endpoint: a test driving a real serve process has no other way to
// have a key accepted, rejected or unanswered without the network.
func keyChecker(base string) func(context.Context, string, string) (int, error) {
	if base == "" {
		return checkProviderKey
	}
	return func(ctx context.Context, provider, key string) (int, error) {
		return askProvider(ctx, base, provider, key)
	}
}

func checkProviderKey(ctx context.Context, provider, key string) (int, error) {
	u, ok := checkURL[provider]
	if !ok {
		return 0, fmt.Errorf("no check for %s", provider)
	}
	return askProvider(ctx, u, provider, key)
}

func askProvider(ctx context.Context, u, provider, key string) (int, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return 0, err
	}
	if provider == "anthropic" {
		req.Header.Set("x-api-key", key)
		req.Header.Set("anthropic-version", "2023-06-01")
	} else {
		req.Header.Set("Authorization", "Bearer "+key)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	resp.Body.Close()
	return resp.StatusCode, nil
}

// checkSetupKey says whether a provider's key works: "ok", "rejected"
// (401/403), "unset", or "unknown" when the provider could not be asked.
// A key that is merely present used to read as "Key found" while every
// session on it failed with a 401.
func (a *API) checkSetupKey(w http.ResponseWriter, r *http.Request) {
	name := r.URL.Query().Get("provider")
	for _, p := range connect.Providers() {
		if p.Name != name {
			continue
		}
		key := a.getenv(p.Env)
		if key == "" {
			file, _ := os.ReadFile(a.envFile())
			key = envFileValue(string(file), p.Env)
		}
		if key == "" {
			writeJSON(w, http.StatusOK, map[string]any{"state": "unset"})
			return
		}
		ctx, cancel := context.WithTimeout(r.Context(), 8*time.Second)
		defer cancel()
		code, err := a.checkKey(ctx, p.Name, key)
		switch {
		case err != nil:
			writeJSON(w, http.StatusOK, map[string]any{"state": "unknown", "detail": err.Error()})
		case code == http.StatusUnauthorized || code == http.StatusForbidden:
			writeJSON(w, http.StatusOK, map[string]any{"state": "rejected", "detail": fmt.Sprintf("%s answered %d", p.Name, code)})
		case code >= 200 && code < 300:
			writeJSON(w, http.StatusOK, map[string]any{"state": "ok"})
		default:
			writeJSON(w, http.StatusOK, map[string]any{"state": "unknown", "detail": fmt.Sprintf("%s answered %d", p.Name, code)})
		}
		return
	}
	writeErr(w, http.StatusBadRequest, fmt.Errorf("serve: api: unknown provider %q", name))
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
