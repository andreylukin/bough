package serve

import (
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
