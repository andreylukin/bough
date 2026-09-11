package vtreal

// The "!" shell passthrough on a real PTY: a bang line runs in the
// composer without a model turn. Replay-backed so a consumed tape
// reply would show: the tape's first (and only) reply must still
// answer the first real turn after the bang lines.

import (
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

func bangShellStart(t *testing.T) *app {
	t.Helper()
	tape, _ := filepath.Abs("testdata/replay/bang-shell.jsonl")
	a := startCfg(t, 100, 30, replayConfig(tape))
	a.check("boot")
	return a
}

// bangShellKinds lists the history entry kinds this run recorded.
func bangShellKinds(a *app) []string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var kinds []string
	for _, p := range paths {
		entries, _ := history.Read(p)
		for _, e := range entries {
			kinds = append(kinds, e.Kind)
		}
	}
	return kinds
}

// bangShellRun submits a composer line and waits for its output.
func bangShellRun(a *app, line, want string) {
	a.t.Helper()
	a.typeText(line)
	a.key(uv.KeyEnter, 0)
	a.waitFor(want)
}

// bangShellNoTurn asserts no model turn started and the tape is
// intact: the next real turn gets the tape's first reply.
func bangShellNoTurn(a *app) {
	a.t.Helper()
	time.Sleep(300 * time.Millisecond) // a wrongly started turn would have logged by now
	for _, k := range bangShellKinds(a) {
		if k == "input" || k == "assistant" || k == "done" {
			a.t.Fatalf("a ! line recorded %q: it reached the model\nkinds=%v\n%s", k, bangShellKinds(a), a.text())
		}
	}
	a.typeText("hello")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 30*time.Second) {
		a.t.Fatalf("real turn after ! never finished:\n%s", a.text())
	}
	s := a.settled()
	if !strings.Contains(s, "first-tape-reply") || strings.Contains(s, "end of tape") {
		a.t.Fatalf("a ! line consumed a tape reply:\n%s", s)
	}
}

func TestBangShell(t *testing.T) {
	t.Parallel()

	t.Run("TestBangShellEcho", func(t *testing.T) {
		t.Parallel()
		a := bangShellStart(t)
		bangShellRun(a, "!echo hi-bang-$((6*7))", "hi-bang-42")
		s := a.settled()
		if !strings.Contains(s, "! echo hi-bang-$((6*7))") {
			t.Fatalf("bang result block label missing:\n%s", s)
		}
		a.check("after !echo")
		if kinds := strings.Join(bangShellKinds(a), ","); !strings.Contains(kinds, "command,system") {
			t.Fatalf("history kinds %s, want command then system:\n%s", kinds, s)
		}
		bangShellNoTurn(a)
	})

	t.Run("TestBangShellFailureShowsExitCode", func(t *testing.T) {
		t.Parallel()
		a := bangShellStart(t)
		bangShellRun(a, "!echo before-fail; exit 3", "exit status 3")
		s := a.settled()
		if !strings.Contains(s, "before-fail") || !strings.Contains(s, "! exit status 3") {
			t.Fatalf("failing ! must show its output and a loud exit line:\n%s", s)
		}
		a.check("after failing !")
		bangShellNoTurn(a)
	})

	t.Run("TestBangShellLongOutputCollapses", func(t *testing.T) {
		t.Parallel()
		a := bangShellStart(t)
		bangShellRun(a, "!seq -f 'row-%g' 1 400", "row-400")
		a.check("after long !")
		// Starts expanded (the user asked for it); scroll up to the
		// label and click it: the 400 rows must fold away.
		for range 200 {
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
		}
		a.waitFor("! seq")
		for y, l := range a.lines() {
			if i := strings.Index(l, "! seq"); i >= 0 {
				a.click(i+1, y)
				break
			}
		}
		a.waitUntil(func(s string) bool {
			return !strings.Contains(s, "row-200") && !strings.Contains(s, "row-400")
		}, "long ! block to collapse")
		a.check("after collapsing long !")
		bangShellNoTurn(a)
	})

	t.Run("TestBangShellDraftTyped", func(t *testing.T) {
		t.Parallel()
		a := bangShellStart(t)
		// A draft is already typed; jumping home and prefixing "!"
		// turns the whole draft into the shell command.
		a.typeText("echo from-draft")
		a.waitFor("echo from-draft")
		a.key(uv.KeyHome, 0)
		a.typeText("!")
		a.waitFor("!echo from-draft")
		a.key(uv.KeyEnter, 0)
		a.waitFor("! echo from-draft")
		a.check("after draft !")
		bangShellNoTurn(a)
	})
}
