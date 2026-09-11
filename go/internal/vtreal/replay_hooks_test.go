package vtreal

// Hooks (plugins/hooks) under replay codemode. The loop fires
// pre-code-exec before a block and post-result after it. A denied
// block is noted as "[hook denied: …]" and never reaches Runtime.Run,
// so its recorded result is NOT consumed; the next block finds its own
// result by the replay cursor's exact-code lookahead, which skips the
// denied one. A rewritten block's code matches nothing on tape, so it
// takes the next recorded result in order.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

// hooksWrite installs <home>/.bough/hooks/<event>/<name>. bough runs
// with cwd = $HOME, so this is both the global and the project dir.
func hooksWrite(t *testing.T, home, event, name, body string) {
	t.Helper()
	dir := filepath.Join(home, ".bough", "hooks", event)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

// hooksEntries returns the trimmed texts of the session's entries of kind.
func hooksEntries(a *app, kind string) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out []string
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			if e.Kind == kind {
				s, _ := e.Data["text"].(string)
				out = append(out, strings.TrimSpace(s))
			}
		}
	}
	return out
}

func TestHooksReplay(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/hooks.jsonl")
	a := startCfg(t, 100, 30, replayConfig(tape))
	// Hook files are re-read on every fire, so installing them after
	// boot is enough.
	hooksWrite(t, a.home, "pre-code-exec", "a_deny.js",
		`if (event.code.includes("DENYME")) return {deny: "no DENYME here"}`)
	hooksWrite(t, a.home, "pre-code-exec", "b_rewrite.js",
		`if (event.code.includes("REWRITE_ME")) return {code: "console.log('HOOK_REWROTE')\n"}`)
	hooksWrite(t, a.home, "post-result", "rewrite.js",
		`if (event.result.includes("POSTME")) return {result: "POST_HOOK_OUTPUT"}`)

	turn := func(t *testing.T, n int, in string) string {
		t.Helper()
		a.typeText(in)
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(n, 30*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", n, a.text())
		}
		a.check(in)
		return a.settled()
	}
	must := func(t *testing.T, s, want, where string) {
		t.Helper()
		if !strings.Contains(s, want) {
			t.Errorf("%s: %q not on screen:\n%s", where, want, s)
		}
	}
	mustNot := func(t *testing.T, s, bad, where string) {
		t.Helper()
		if strings.Contains(s, bad) {
			t.Errorf("%s: %q on screen:\n%s", where, bad, s)
		}
	}

	t.Run("TestHooksDenyShowsReasonAndSkipsRecordedResult", func(t *testing.T) {
		s := turn(t, 1, "try the denied block")
		must(t, s, "[hook denied: no DENYME here]", "deny")
		mustNot(t, s, "RECORDED_DENIED_OUTPUT", "deny")
		must(t, s, "ALIGNED_B_OUTPUT", "block after deny")
		must(t, s, "Turn one over.", "deny turn reply")
	})
	t.Run("TestHooksPreCodeRewriteRunsRewrittenCode", func(t *testing.T) {
		s := turn(t, 2, "try the rewritten block")
		must(t, s, "HOOK_REWROTE", "rewritten code block")
		// The rewritten code matches no recorded block, so the replay
		// cursor hands it the next result in order: this one.
		must(t, s, "RECORDED_REWRITE_OUTPUT", "rewritten block result")
		must(t, s, "Turn two over.", "rewrite turn reply")
	})
	t.Run("TestHooksPostResultRewritesOutput", func(t *testing.T) {
		s := turn(t, 3, "try the post result hook")
		must(t, s, "POST_HOOK_OUTPUT", "post-result")
		mustNot(t, s, "POSTME raw output", "post-result")
		must(t, s, "Turn three over.", "post-result turn reply")
	})
	t.Run("TestHooksTapeStaysAligned", func(t *testing.T) {
		s := turn(t, 4, "thanks")
		must(t, s, "Tape still aligned.", "last turn")
		mustNot(t, s, "end of tape", "last turn")
	})
	t.Run("TestHooksHistoryRecordsTruth", func(t *testing.T) {
		// The denied block has a result entry but no code entry.
		wantCode := []string{"console.log('b')", "console.log('HOOK_REWROTE')", "console.log('p')"}
		wantRes := []string{"[hook denied: no DENYME here]", "ALIGNED_B_OUTPUT", "RECORDED_REWRITE_OUTPUT", "POST_HOOK_OUTPUT"}
		if got := hooksEntries(a, "code"); strings.Join(got, "|") != strings.Join(wantCode, "|") {
			t.Errorf("code entries = %q, want %q\n%s", got, wantCode, a.text())
		}
		if got := hooksEntries(a, "result"); strings.Join(got, "|") != strings.Join(wantRes, "|") {
			t.Errorf("result entries = %q, want %q\n%s", got, wantRes, a.text())
		}
	})
}
