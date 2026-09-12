package serve

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// seedHook writes a hook file into the fixture's home pool.
func seedHook(t *testing.T, home, event, name, body string) string {
	t.Helper()
	dir := filepath.Join(home, ".bough", "hooks", event)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

func newHooksAPI(t *testing.T) *apiFixture {
	t.Helper()
	f := newAPI(t)
	f.api.home = f.home // the fixture's pools, not the developer's
	return f
}

func TestHooksList(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	seedHook(t, f.home, "post-result", "guard.js", "return {}")

	code, body := f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/hooks = %d %v", code, body)
	}
	hooks, _ := body["hooks"].([]any)
	if len(hooks) != 1 {
		t.Fatalf("hooks = %v, want one", body["hooks"])
	}
	row, _ := hooks[0].(map[string]any)
	if row["name"] != "guard.js" || row["event"] != "post-result" || row["scope"] != "home" {
		t.Errorf("row = %v", row)
	}
	if row["shadowed"] != false || row["failing"] != false || row["error"] != "" {
		t.Errorf("row = %v", row)
	}
	// Every field is always present, so the UI never branches on undefined.
	if _, ok := row["lastFired"]; !ok {
		t.Errorf("lastFired missing from %v", row)
	}
	if row["lastDecision"] != "" {
		t.Errorf("lastDecision = %v", row["lastDecision"])
	}
}

// The three lists are lists even when nothing is installed: the UI maps
// over them.
func TestHooksEmptyLists(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	code, body := f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/hooks = %d", code)
	}
	for _, key := range []string{"hooks", "watchers", "fires"} {
		if _, ok := body[key].([]any); !ok {
			t.Errorf("%s is not a list: %#v", key, body[key])
		}
	}
}

func TestHookFileReadWrite(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	path := filepath.Join(f.home, ".bough", "watchers", "ci.js")

	code, body := f.do(t, "PUT", "/api/hooks/file",
		`{"path":`+quote(path)+`,"body":"return {}"}`)
	if code != http.StatusOK || body["ok"] != true {
		t.Fatalf("PUT = %d %v", code, body)
	}
	code, body = f.do(t, "GET", "/api/hooks/file?path="+path, "")
	if code != http.StatusOK {
		t.Fatalf("GET = %d %v", code, body)
	}
	if body["body"] != "return {}" {
		t.Errorf("body = %v", body["body"])
	}
}

// The guard: serve has no auth, so a path outside the pools must never
// be readable or writable through it.
func TestHookFileOutsidePools(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	elsewhere := filepath.Join(f.home, "secrets.js")
	if err := os.WriteFile(elsewhere, []byte("secret"), 0o644); err != nil {
		t.Fatal(err)
	}
	traversal := filepath.Join(f.home, ".bough", "hooks", "..", "..", "secrets.js")

	for _, path := range []string{elsewhere, traversal} {
		if code, _ := f.do(t, "GET", "/api/hooks/file?path="+path, ""); code != http.StatusBadRequest {
			t.Errorf("GET %s = %d, want 400", path, code)
		}
		code, _ := f.do(t, "PUT", "/api/hooks/file", `{"path":`+quote(path)+`,"body":"owned"}`)
		if code != http.StatusBadRequest {
			t.Errorf("PUT %s = %d, want 400", path, code)
		}
	}
	if b, _ := os.ReadFile(elsewhere); string(b) != "secret" {
		t.Fatalf("file outside the pools was overwritten: %q", b)
	}
}

func TestHookDryrun(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	path := seedHook(t, f.home, "pre-code-exec", "deny.js",
		`if (event.event === "pre-code-exec") return {deny: "not here"}; return {}`)

	code, body := f.do(t, "POST", "/api/hooks/dryrun",
		`{"path":`+quote(path)+`,"event":"pre-code-exec"}`)
	if code != http.StatusOK {
		t.Fatalf("dryrun = %d %v", code, body)
	}
	res, ok := body["result"].(map[string]any)
	if !ok {
		t.Fatalf("result is not an object: %#v", body["result"])
	}
	if res["deny"] != "not here" {
		t.Errorf("result = %v", res)
	}
	if body["error"] != "" {
		t.Errorf("error = %v", body["error"])
	}
	if _, ok := body["ms"].(float64); !ok {
		t.Errorf("ms missing from %v", body)
	}
}

// A hook that throws is a report, not a 500: the point of a dry run is
// to see the failure.
func TestHookDryrunError(t *testing.T) {
	t.Parallel()
	f := newHooksAPI(t)
	path := seedHook(t, f.home, "post-result", "boom.js", `throw new Error("boom")`)

	code, body := f.do(t, "POST", "/api/hooks/dryrun",
		`{"path":`+quote(path)+`,"event":"post-result"}`)
	if code != http.StatusOK {
		t.Fatalf("dryrun = %d %v", code, body)
	}
	if s, _ := body["error"].(string); s == "" {
		t.Fatalf("no error reported: %v", body)
	}
	if _, ok := body["result"].(map[string]any); !ok {
		t.Errorf("result is not an object: %#v", body["result"])
	}
}

// quote is a JSON string literal for a path, so a temp dir with an odd
// character in it does not break a hand-written request body.
func quote(s string) string {
	b, err := json.Marshal(s)
	if err != nil {
		return `""`
	}
	return string(b)
}

// The ledger crosses a process boundary: the child records a fire into
// history, and serve — which can only read history — must find it. This
// is the seam that was stubbed out and returned [] regardless.
func TestFiresFromHistory(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	now := time.Now()
	f.seed(t, "01a00000-0000-7000-8000-00000000fire",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "go"}},
		history.Entry{Seq: 3, At: now, Kind: "hook", Data: map[string]any{
			"event": "post-result", "name": "rules", "ms": float64(3), "decision": "denied", "error": "",
		}},
		history.Entry{Seq: 4, At: now, Kind: "done", Data: nil},
	)
	code, body := f.do(t, "GET", "/api/hooks", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/hooks = %d", code)
	}
	fires, _ := body["fires"].([]any)
	if len(fires) != 1 {
		t.Fatalf("fires = %d, want 1 (%v)", len(fires), body["fires"])
	}
	got, _ := fires[0].(map[string]any)
	if got["name"] != "rules" || got["event"] != "post-result" || got["decision"] != "denied" {
		t.Errorf("fire = %v, want the rules/post-result/denied record", got)
	}
	if ms, _ := got["ms"].(float64); ms != 3 {
		t.Errorf("ms = %v, want 3", got["ms"])
	}
}
