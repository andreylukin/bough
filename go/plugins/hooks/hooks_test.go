package hooks

import (
	"context"
	"os"
	"path/filepath"
	"strings"
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
	// Skipped, not fatal — but REPORTED. This used to go to os.Stderr,
	// which under the TUI writes inside the alt-screen and corrupts the
	// frame; the loop turns a returned error into a visible error event
	// instead. Silence here is what let that bug live.
	if err == nil {
		t.Fatal("a broken hook file must be reported, not silently skipped")
	}
	if !strings.Contains(err.Error(), "a.js") {
		t.Errorf("the error should name the file that failed: %v", err)
	}
	if res["ok"] != true {
		t.Fatalf("want b.js result despite broken a.js, got %v", res)
	}
}

func TestLedgerRecordsDecisions(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	s.SetSession("sess-1")
	writeHook(t, cwd, "post-result", "noop.js", `return null`)
	writeHook(t, cwd, "post-result", "rewrite.js", `return {output: "clean"}`)
	writeHook(t, cwd, "post-result", "zblock.js", `return {block: "no"}`)
	if _, err := s.Fire(context.Background(), "post-result", map[string]any{"output": "dirty"}); err != nil {
		t.Fatal(err)
	}
	fires := s.Fires(0)
	if len(fires) != 3 {
		t.Fatalf("want 3 fires, got %d: %v", len(fires), fires)
	}
	// newest first: zblock, rewrite, noop.
	want := []struct{ name, decision string }{
		{"zblock.js", "blocked"}, {"rewrite.js", "rewrote"}, {"noop.js", ""},
	}
	for i, w := range want {
		f := fires[i]
		if f.Name != w.name || f.Decision != w.decision {
			t.Errorf("fire %d = %q/%q, want %q/%q", i, f.Name, f.Decision, w.name, w.decision)
		}
		if f.Event != "post-result" || f.Session != "sess-1" || f.At.IsZero() {
			t.Errorf("fire %d = %+v", i, f)
		}
	}
}

func TestLedgerRecordsDeny(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "pre-code-exec", "guard.js", `return {deny: "nope"}`)
	if _, err := s.Fire(context.Background(), "pre-code-exec", map[string]any{"code": "rm"}); err != nil {
		t.Fatal(err)
	}
	fires := s.Fires(0)
	if len(fires) != 1 || fires[0].Decision != "denied" {
		t.Fatalf("want one denied fire, got %+v", fires)
	}
}

// A rules row rides the same seam, so its decisions land in the ledger
// without the rules package knowing the ledger exists.
func TestLedgerRecordsGoHook(t *testing.T) {
	s := fixture(t)
	s.Add("post-result", "rules", func(map[string]any) map[string]any {
		return map[string]any{"deny": "rule"}
	})
	if _, err := s.Fire(context.Background(), "post-result", map[string]any{}); err != nil {
		t.Fatal(err)
	}
	fires := s.Fires(0)
	if len(fires) != 1 || fires[0].Name != "rules" || fires[0].Decision != "denied" {
		t.Fatalf("want one denied rules fire, got %+v", fires)
	}
}

func TestLedgerRecordsFailure(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "post-result", "boom.js", `throw new Error("boom")`)
	if _, err := s.Fire(context.Background(), "post-result", map[string]any{}); err == nil {
		t.Fatal("want an error")
	}
	fires := s.Fires(0)
	if len(fires) != 1 || fires[0].Error == "" {
		t.Fatalf("want one failing fire, got %+v", fires)
	}
}

func TestLedgerRingIsBounded(t *testing.T) {
	s := fixture(t)
	s.Add("tick", "counter", func(map[string]any) map[string]any { return nil })
	for range fireRing + 50 {
		if _, err := s.Fire(context.Background(), "tick", map[string]any{}); err != nil {
			t.Fatal(err)
		}
	}
	if got := len(s.Fires(0)); got != fireRing {
		t.Fatalf("ring holds %d, want %d", got, fireRing)
	}
	if got := len(s.Fires(5)); got != 5 {
		t.Fatalf("Fires(5) returned %d", got)
	}
}

func TestNoticeIsRecordedAndDecidesNothing(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "post-result", "audit.js", `return {notice: "3 secrets scanned"}`)
	res, err := s.Fire(context.Background(), "post-result", map[string]any{"result": "out"})
	if err != nil {
		t.Fatal(err)
	}
	if got, _ := res["notice"].(string); got != "3 secrets scanned" {
		t.Fatalf("notice not returned: %v", res)
	}
	if _, ok := res["result"]; ok {
		t.Fatalf("a notice-only hook touched the payload: %v", res)
	}
	fires := s.Fires(0)
	if len(fires) != 1 || fires[0].Notice != "3 secrets scanned" {
		t.Fatalf("notice not in the ledger: %+v", fires)
	}
	if fires[0].Decision != "" {
		t.Fatalf("a notice decided something: %q", fires[0].Decision)
	}
}

func TestNoticesFromSeveralHooksBothSurvive(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "post-result", "a.js", `return {notice: "first"}`)
	writeHook(t, cwd, "post-result", "b.js", `return {notice: "second"}`)
	res, err := s.Fire(context.Background(), "post-result", map[string]any{})
	if err != nil {
		t.Fatal(err)
	}
	if got, _ := res["notice"].(string); got != "first\nsecond" {
		t.Fatalf("notices lost: %q", got)
	}
}

func TestResultIsCappedAndTheCutIsRecorded(t *testing.T) {
	s := fixture(t)
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "post-result", "big.js", `return {result: "x".repeat(20000)}`)
	res, err := s.Fire(context.Background(), "post-result", map[string]any{"result": "out"})
	if err != nil {
		t.Fatal(err)
	}
	out, _ := res["result"].(string)
	if len(out) <= maxHookOutput || len(out) > maxHookOutput+100 {
		t.Fatalf("result not capped: %d chars", len(out))
	}
	if !strings.Contains(out, "truncated at 10000 characters") {
		t.Fatalf("no truncation note: %q", out[len(out)-80:])
	}
	fires := s.Fires(0)
	if len(fires) != 1 || len(fires[0].Truncated) != 1 || fires[0].Truncated[0] != "result" {
		t.Fatalf("truncation not in the ledger: %+v", fires)
	}
}

func TestOffHookDoesNotFire(t *testing.T) {
	s := fixture(t)
	home, _ := os.UserHomeDir()
	cwd, _ := os.Getwd()
	writeHook(t, cwd, "post-result", "audit.js", `return {result: "REWRITTEN"}`)
	if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "off.yml"),
		[]byte("off:\n  - hook:post-result/audit.js\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	res, err := s.Fire(context.Background(), "post-result", map[string]any{"result": "out"})
	if err != nil {
		t.Fatal(err)
	}
	if res != nil {
		t.Fatalf("an off hook ran: %v", res)
	}
	if fires := s.Fires(0); len(fires) != 0 {
		t.Fatalf("an off hook was recorded: %+v", fires)
	}
}

func TestOffAppliesToGoHooks(t *testing.T) {
	s := fixture(t)
	home, _ := os.UserHomeDir()
	if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "off.yml"),
		[]byte("off:\n  - hook:post-result/rules\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	s.Add("post-result", "rules", func(map[string]any) map[string]any {
		return map[string]any{"deny": "rule"}
	})
	res, err := s.Fire(context.Background(), "post-result", map[string]any{})
	if err != nil {
		t.Fatal(err)
	}
	if res != nil {
		t.Fatalf("an off go hook ran: %v", res)
	}
}
