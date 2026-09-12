package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"testing"
)

func TestOffRoundTrip(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	seedHook(t, f.home, "post-result", "guard.js", "return {}")

	code, body := f.do(t, "POST", "/api/off", `{"id":"hook:post-result/guard.js","off":true}`)
	if code != http.StatusOK || body["ok"] != true {
		t.Fatalf("POST /api/off = %d %v", code, body)
	}

	// The switch is in the file the TUI and the loop read, and the
	// next read reflects it.
	code, body = f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/hooks = %d %v", code, body)
	}
	hooks, _ := body["hooks"].([]any)
	if len(hooks) != 1 {
		t.Fatalf("hooks = %v", body["hooks"])
	}
	row, _ := hooks[0].(map[string]any)
	if row["id"] != "post-result/guard.js" || row["off"] != true {
		t.Fatalf("row = %v, want it off", row)
	}

	if _, err := os.Stat(filepath.Join(f.home, ".bough", "off.yml")); err != nil {
		t.Fatalf("off.yml: %v", err)
	}

	// And back on again.
	if code, body = f.do(t, "POST", "/api/off", `{"id":"hook:post-result/guard.js","off":false}`); code != http.StatusOK {
		t.Fatalf("un-off = %d %v", code, body)
	}
	_, body = f.do(t, "GET", "/api/hooks", "")
	hooks, _ = body["hooks"].([]any)
	if row, _ := hooks[0].(map[string]any); row["off"] != false {
		t.Errorf("row = %v, want it back on", row)
	}
}

// An off rule still appears, so the switch can be thrown back.
func TestOffRuleStaysListed(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	path := filepath.Join(f.home, ".claude", "rules", "house.md")
	write(t, path, "be terse\n")
	work := filepath.Join(f.home, "work")
	write(t, filepath.Join(work, "keep.md"), "x")
	seedAt(t, f, "s1", work)

	if code, body := f.do(t, "POST", "/api/off", `{"id":"rule:`+path+`","off":true}`); code != http.StatusOK {
		t.Fatalf("POST /api/off = %d %v", code, body)
	}
	_, body := f.do(t, "GET", "/api/sessions/s1/context", "")
	rules, _ := body["rules"].([]any)
	if len(rules) != 1 {
		t.Fatalf("rules = %v, want the off rule still listed", body["rules"])
	}
	row, _ := rules[0].(map[string]any)
	if row["id"] != path || row["off"] != true || row["scope"] != "home" {
		t.Errorf("rule = %v", row)
	}
	if globs, ok := row["globs"].([]any); !ok || len(globs) != 0 {
		t.Errorf("globs = %v, want an empty list", row["globs"])
	}
}

func TestOffUnknownKind(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	for _, id := range []string{`"mcp:everything"`, `"circleci"`, `""`} {
		code, body := f.do(t, "POST", "/api/off", `{"id":`+id+`,"off":true}`)
		if code != http.StatusBadRequest {
			t.Fatalf("POST /api/off %s = %d %v, want 400", id, code, body)
		}
		if body["error"] == "" {
			t.Errorf("%s: no error message", id)
		}
	}
}

func TestHooksListsRulesPluginsAndWatchers(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	code, body := f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/hooks = %d %v", code, body)
	}
	// Empty is [] everywhere, never null: the UI never branches on it.
	for _, key := range []string{"hooks", "watchers", "fires", "rules", "plugins"} {
		if _, ok := body[key].([]any); !ok {
			t.Errorf("%s = %v, want a list", key, body[key])
		}
	}
	if len(body["plugins"].([]any)) != 0 {
		t.Errorf("plugins = %v, want none in a seeded home", body["plugins"])
	}
}
