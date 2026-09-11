package vtreal

// The terminal tab title (OSC 0/2, recorded by the title filter) in
// every state tabtitle.go names: "bough" on boot, "● <prompt>" while
// a turn runs, "✓ <prompt>" after it, "■ <prompt>" after a cancel,
// "? <prompt>" while tools.ask waits, and the session title in place
// of the prompt once one is set.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func tabTitleTape(t *testing.T, name string) string {
	t.Helper()
	p, err := filepath.Abs(filepath.Join("testdata", "replay", name))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// tabTitleSlow streams the tape a word every 150 ms, so a turn stays
// running long enough to observe (and cancel).
func tabTitleSlow(tape string) string {
	return strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}\n", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 150}\n", tape), 1)
}

// tabTitleWait polls the recorded title until it equals want.
func tabTitleWait(a *app, want string) {
	a.t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		if a.term.Snapshot().Title == want {
			return
		}
		time.Sleep(25 * time.Millisecond)
	}
	a.t.Fatalf("tab title %q, want %q:\n%s", a.term.Snapshot().Title, want, a.text())
}

func tabTitleSend(a *app, in string) {
	a.typeText(in)
	a.key(uv.KeyEnter, 0)
}

func TestTabTitleBootRunDone(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, tabTitleSlow(tabTitleTape(t, "tab-title-slow.jsonl")))
	tabTitleWait(a, "bough")
	tabTitleSend(a, "fix the flaky test")
	tabTitleWait(a, "● fix the flaky test")
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	tabTitleWait(a, "✓ fix the flaky test")
}

func TestTabTitleCancel(t *testing.T) {
	t.Parallel()
	a := startCfg(t, 100, 30, tabTitleSlow(tabTitleTape(t, "tab-title-slow.jsonl")))
	tabTitleSend(a, "fix the flaky test")
	tabTitleWait(a, "● fix the flaky test")
	a.key('c', uv.ModCtrl)
	if !a.waitDone(1, 20*time.Second) {
		t.Fatalf("cancel never landed:\n%s", a.text())
	}
	tabTitleWait(a, "■ fix the flaky test")
}

// The ask needs a real runtime: the replay codemode answers blocks
// from the tape and would never call tools.ask.
func TestTabTitleAsk(t *testing.T) {
	t.Parallel()
	cfg := replayConfig(tabTitleTape(t, "tab-title-ask.jsonl"))
	i := strings.Index(cfg, "- id: codemode\n")
	j := strings.Index(cfg, "- id: commands\n")
	cfg = cfg[:i] + "- id: codemode\n  plugin: codemode\n" + cfg[j:]
	a := startCfg(t, 100, 30, cfg)
	tabTitleSend(a, "pick a color")
	tabTitleWait(a, "? pick a color")
	a.waitFor("which color?")
}

// A resumed session carries a title entry: the tab shows it, not the
// opening prompt, and keeps it through the next turn.
func TestTabTitleResumedSessionTitle(t *testing.T) {
	t.Parallel()
	src := tabTitleTape(t, "tab-title-resume.jsonl")
	data, err := os.ReadFile(src)
	if err != nil {
		t.Fatal(err)
	}
	hist := filepath.Join(t.TempDir(), "resumed.jsonl") // history appends: never the fixture
	if err := os.WriteFile(hist, data, 0o644); err != nil {
		t.Fatal(err)
	}
	// The model side plays the slow tape so the new turn is observable.
	cfg := strings.Replace(tabTitleSlow(tabTitleTape(t, "tab-title-slow.jsonl")),
		"- id: history\n  plugin: history\n",
		fmt.Sprintf("- id: history\n  plugin: history\n  config: {file: %q}\n", hist), 1)
	a := startCfg(t, 100, 30, cfg)
	tabTitleWait(a, "✓ Flaky test hunt")
	tabTitleSend(a, "one more thing")
	tabTitleWait(a, "● Flaky test hunt")
	tabTitleWait(a, "✓ Flaky test hunt")
}
