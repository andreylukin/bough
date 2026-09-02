package hooks

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/loop"
)

// Service must satisfy the loop seam it is resolved through.
var _ loop.Hooks = (*Service)(nil)

// fixture points HOME at a temp dir, chdirs into a temp project dir,
// and returns a Service backed by a real codemode VM.
func fixture(t *testing.T) *Service {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	t.Chdir(t.TempDir())
	return &Service{code: codemode.New(time.Second)}
}

// writeHook writes body to <root>/.bough/hooks/<event>/<name>.
func writeHook(t *testing.T, root, event, name, body string) {
	t.Helper()
	dir := filepath.Join(root, ".bough", "hooks", event)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestFireMissingDirs(t *testing.T) {
	s := fixture(t)
	res, err := s.Fire(context.Background(), "session-start", map[string]any{})
	if err != nil {
		t.Fatal(err)
	}
	if res != nil {
		t.Fatalf("want nil result, got %v", res)
	}
}

func TestFireOrderingAndShadowing(t *testing.T) {
	s := fixture(t)
	home, _ := os.UserHomeDir()
	cwd, _ := os.Getwd()
	// a.js (global) runs first, b.js exists in both — project shadows.
	writeHook(t, home, "user-prompt-submit", "a.js", `return {who: "global-a", ga: true}`)
	writeHook(t, home, "user-prompt-submit", "b.js", `return {who: "global-b", gb: true}`)
	writeHook(t, cwd, "user-prompt-submit", "b.js", `return {who: "project-b", pb: true}`)

	res, err := s.Fire(context.Background(), "user-prompt-submit", map[string]any{"input": "hi"})
	if err != nil {
		t.Fatal(err)
	}
	if res["who"] != "project-b" {
		t.Fatalf("want who=project-b (b after a, project shadows global), got %v", res)
	}
	if res["ga"] != true || res["pb"] != true {
		t.Fatalf("want keys from both files merged, got %v", res)
	}
	if _, ok := res["gb"]; ok {
		t.Fatalf("shadowed global b.js should not have run: %v", res)
	}
}

func TestFireDenyShortCircuits(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "pre-code-exec", "a.js", `return {deny: "nope"}`)
	writeHook(t, cwd, "pre-code-exec", "b.js", `return {ran: true}`)

	res, err := s.Fire(context.Background(), "pre-code-exec", map[string]any{"code": "1"})
	if err != nil {
		t.Fatal(err)
	}
	if res["deny"] != "nope" {
		t.Fatalf("want deny=nope, got %v", res)
	}
	if _, ok := res["ran"]; ok {
		t.Fatalf("b.js should not have run after deny: %v", res)
	}
}

func TestFireBrokenFileSkipped(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "stop", "a.js", `this is not javascript ((`)
	writeHook(t, cwd, "stop", "b.js", `return {ok: true}`)

	res, err := s.Fire(context.Background(), "stop", map[string]any{})
	if err != nil {
		t.Fatal(err)
	}
	if res["ok"] != true {
		t.Fatalf("want b.js result despite broken a.js, got %v", res)
	}
}
