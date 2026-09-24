package serve

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// setupHome gives the fixture its own HOME and an empty process env, so
// a key on the developer's machine cannot make a provider look set.
func setupHome(t *testing.T, f *apiFixture) string {
	t.Helper()
	home := t.TempDir()
	f.api.home = home
	f.api.getenv = func(string) string { return "" }
	return home
}

func providerSet(t *testing.T, body map[string]any, name string) bool {
	t.Helper()
	list, _ := body["providers"].([]any)
	for _, p := range list {
		m, _ := p.(map[string]any)
		if m["name"] == name {
			set, _ := m["set"].(bool)
			return set
		}
	}
	t.Fatalf("no provider %q in %v", name, body)
	return false
}

func TestSetupReportsKeysAndCheckout(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	home := setupHome(t, f)
	repo := filepath.Join(home, "app")
	for _, d := range []string{filepath.Join(repo, ".git"), filepath.Join(repo, "sub"), filepath.Join(home, "plain"), filepath.Join(home, ".bough")} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "env"), []byte("export OPENROUTER_API_KEY='sk-or-x'\nOPENAI_API_KEY=\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	code, body := f.do(t, "GET", "/api/setup?cwd="+url.QueryEscape(filepath.Join(repo, "sub")), "")
	if code != 200 {
		t.Fatalf("GET /api/setup = %d %v", code, body)
	}
	if !providerSet(t, body, "openrouter") || providerSet(t, body, "openai") || providerSet(t, body, "anthropic") {
		t.Errorf("provider state wrong: %v", body["providers"])
	}
	if folder, _ := body["folder"].(map[string]any); folder["checkout"] != repo || folder["exists"] != true {
		t.Errorf("folder in a checkout = %v, want checkout %s", folder, repo)
	}

	_, body = f.do(t, "GET", "/api/setup?cwd="+url.QueryEscape("~/plain"), "")
	if folder, _ := body["folder"].(map[string]any); folder["path"] != filepath.Join(home, "plain") || folder["exists"] != true || folder["checkout"] != nil {
		t.Errorf("plain folder = %v, want an existing read-only folder", folder)
	}
	_, body = f.do(t, "GET", "/api/setup?cwd="+url.QueryEscape(filepath.Join(home, "missing")), "")
	if folder, _ := body["folder"].(map[string]any); folder["exists"] != false {
		t.Errorf("missing folder = %v, want exists false", folder)
	}
}

// The badge reads Row.Writable: a local session in a checkout edits it,
// one outside is read-only, and a project session never gets a root.
func TestWritableRoot(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	home := setupHome(t, f)
	repo := filepath.Join(home, "app")
	if err := os.MkdirAll(filepath.Join(repo, ".git"), 0o755); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct{ mode, cwd, want string }{
		{"local", filepath.Join(repo), repo},
		{"", repo, repo},
		{"local", home, ""},
		{"project", repo, ""},
	} {
		if got := f.api.writableRoot(tc.mode, tc.cwd); got != tc.want {
			t.Errorf("writableRoot(%q, %q) = %q, want %q", tc.mode, tc.cwd, got, tc.want)
		}
	}
}

// A typed path completes to the folders under it, hidden ones only when
// the prefix asks, and each says whether a session there could edit.
func TestDirsCompletesFolders(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	home := setupHome(t, f)
	for _, d := range []string{filepath.Join(home, "repos", "app", ".git"), filepath.Join(home, "repos", "api"), filepath.Join(home, "repos", ".cache"), filepath.Join(home, "other")} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(home, "repos", "apple.txt"), nil, 0o600); err != nil {
		t.Fatal(err)
	}
	paths := func(body map[string]any) map[string]any {
		got := map[string]any{}
		list, _ := body["dirs"].([]any)
		for _, x := range list {
			m, _ := x.(map[string]any)
			got[m["path"].(string)] = m["checkout"]
		}
		return got
	}
	code, body := f.do(t, "GET", "/api/dirs?path="+url.QueryEscape("~/repos/ap"), "")
	if code != 200 {
		t.Fatalf("GET /api/dirs = %d %v", code, body)
	}
	app, api := filepath.Join(home, "repos", "app"), filepath.Join(home, "repos", "api")
	if got := paths(body); len(got) != 2 || got[app] != app || got[api] != nil {
		t.Errorf("~/repos/ap = %v, want app (a checkout) and api", got)
	}
	_, body = f.do(t, "GET", "/api/dirs?path="+url.QueryEscape("~/repos/"), "")
	if got := paths(body); len(got) != 2 {
		t.Errorf("~/repos/ = %v, want app and api without .cache", got)
	}
	if folder, _ := body["folder"].(map[string]any); folder["exists"] != true {
		t.Errorf("folder = %v, want the typed folder to exist", folder)
	}
	_, body = f.do(t, "GET", "/api/dirs?path="+url.QueryEscape("~/repos/.c"), "")
	if got := paths(body); len(got) != 1 {
		t.Errorf("~/repos/.c = %v, want .cache", got)
	}
}

func TestSetupSavesKey(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	home := setupHome(t, f)
	var gotEnv, gotKey string
	f.api.setenv = func(k, v string) error { gotEnv, gotKey = k, v; return nil }

	code, body := f.do(t, "POST", "/api/setup/key", `{"provider":"anthropic","key":"  sk-ant-1  "}`)
	if code != 200 {
		t.Fatalf("POST /api/setup/key = %d %v", code, body)
	}
	b, err := os.ReadFile(filepath.Join(home, ".bough", "env"))
	if err != nil || !strings.Contains(string(b), "ANTHROPIC_API_KEY=sk-ant-1\n") {
		t.Errorf("env file = %q (%v), want the trimmed key", b, err)
	}
	if gotEnv != "ANTHROPIC_API_KEY" || gotKey != "sk-ant-1" {
		t.Errorf("setenv(%q, %q), want the key in this process", gotEnv, gotKey)
	}
	if !providerSet(t, body, "anthropic") {
		t.Errorf("reply does not show anthropic set: %v", body)
	}

	for _, bad := range []string{`{"provider":"nope","key":"x"}`, `{"provider":"openai","key":"  "}`} {
		if code, _ := f.do(t, "POST", "/api/setup/key", bad); code != 400 {
			t.Errorf("POST %s = %d, want 400", bad, code)
		}
	}
}

func TestSetupChecksKey(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	home := setupHome(t, f)
	if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "env"), []byte("export ANTHROPIC_API_KEY='sk-ant-bad'\nOPENAI_API_KEY=sk-good\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	f.api.checkKey = func(_ context.Context, provider, key string) (int, error) {
		switch key {
		case "sk-good":
			return 200, nil
		case "sk-ant-bad":
			return 401, nil
		}
		return 0, errors.New("offline")
	}
	for provider, want := range map[string]string{"anthropic": "rejected", "openai": "ok", "cerebras": "unset"} {
		code, body := f.do(t, "GET", "/api/setup/check?provider="+provider, "")
		if code != 200 || body["state"] != want {
			t.Errorf("check %s = %d %v, want state %q", provider, code, body, want)
		}
	}
	if code, _ := f.do(t, "GET", "/api/setup/check?provider=nope", ""); code != 400 {
		t.Errorf("unknown provider = %d, want 400", code)
	}
}

// BOUGH_SETUP_CHECK_URL points every provider's key check at one
// endpoint, so a test of a real serve process can have a key accepted,
// rejected or unanswerable without asking a real provider.
func TestSetupCheckURLOverride(t *testing.T) {
	t.Parallel()
	var got []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		key := r.Header.Get("x-api-key")
		if key == "" {
			key = strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
		}
		got = append(got, key)
		if key == "good" {
			w.WriteHeader(200)
			return
		}
		w.WriteHeader(401)
	}))
	defer srv.Close()
	check := keyChecker(srv.URL)
	for _, c := range []struct {
		provider, key string
		want          int
	}{{"anthropic", "good", 200}, {"anthropic", "bad", 401}, {"openai", "good", 200}} {
		code, err := check(context.Background(), c.provider, c.key)
		if err != nil || code != c.want {
			t.Errorf("check %s %s = %d, %v; want %d", c.provider, c.key, code, err, c.want)
		}
	}
	if len(got) != 3 {
		t.Errorf("the override saw keys %v, want all three checks", got)
	}
}
