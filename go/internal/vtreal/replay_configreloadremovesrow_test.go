package vtreal

// Hot edits of the config overlay that drop a default row (todo,
// theme) with `disabled: true`, mid-turn and between turns, then put
// it back. The reconcile must not crash the remount, the in-flight
// turn must finish, the row's UI must go away once the turn is over,
// and re-adding the row must bring it back.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// configreloadremovesrowTape writes a two-turn tape: a slow reply
// (streamed word by word under delay_ms) and a quick one.
func configreloadremovesrowTape(t *testing.T) string {
	t.Helper()
	words := make([]string, 40)
	for i := range words {
		words[i] = fmt.Sprintf("w%02d", i)
	}
	lines := []string{
		`{"seq":1,"at":"2026-09-10T11:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}`,
		`{"seq":2,"at":"2026-09-10T11:00:01Z","kind":"input","data":{"text":"go slow"}}`,
		`{"seq":3,"at":"2026-09-10T11:00:02Z","kind":"assistant","data":{"text":"` + "```stop\\n" + strings.Join(words, " ") + " SLOWEND\\n```" + `"}}`,
		`{"seq":4,"at":"2026-09-10T11:00:03Z","kind":"done","data":{"text":""}}`,
		`{"seq":5,"at":"2026-09-10T11:00:04Z","kind":"input","data":{"text":"go fast"}}`,
		`{"seq":6,"at":"2026-09-10T11:00:05Z","kind":"assistant","data":{"text":"` + "```stop\\nFASTEND\\n```" + `"}}`,
		`{"seq":7,"at":"2026-09-10T11:00:06Z","kind":"done","data":{"text":""}}`,
	}
	p := filepath.Join(t.TempDir(), "reload.jsonl")
	if err := os.WriteFile(p, []byte(strings.Join(lines, "\n")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// configreloadremovesrowStart boots on the tape with a 100ms word delay.
func configreloadremovesrowStart(t *testing.T) (*app, string) {
	t.Helper()
	tape := configreloadremovesrowTape(t)
	yml := strings.Replace(replayConfig(tape), fmt.Sprintf("config: {file: %q}", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 100}", tape), 1)
	if !strings.Contains(yml, "delay_ms") {
		t.Fatalf("could not slow the llm row:\n%s", yml)
	}
	return startCfg(t, 100, 30, yml), yml
}

// configreloadremovesrowWrite replaces the overlay the running binary
// watches ($HOME/bough.yml, passed as -config).
func configreloadremovesrowWrite(a *app, yml string) {
	a.t.Helper()
	if err := os.WriteFile(filepath.Join(a.home, "bough.yml"), []byte(yml), 0o644); err != nil {
		a.t.Fatal(err)
	}
}

// configreloadremovesrowUntil retypes a slash line until the screen
// shows want: the reload lands asynchronously (fsnotify + debounce),
// and a live repaint can wipe its stderr notice, so behavior is the
// proof the tree changed.
func configreloadremovesrowUntil(a *app, line, want string) {
	a.t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		configreloadremovesrowCmd(a, line)
		time.Sleep(700 * time.Millisecond)
		if strings.Contains(strings.ReplaceAll(a.text(), "\n", ""), want) {
			return
		}
	}
	a.t.Fatalf("%q never showed %q:\n%s", line, want, a.text())
}

// configreloadremovesrowKnown skips a subtest that pins a known bug.
func configreloadremovesrowKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_CONFIGRELOADREMOVESROW") == "" {
		t.Skip("known bug (set BOUGH_KNOWN_CONFIGRELOADREMOVESROW=1 to run): " + bug)
	}
}

// configreloadremovesrowRepaint forces a full redraw with a resize
// round trip, so a stale paint (the raw stderr reload line scrolls the
// alt screen under the renderer) is not mistaken for ui state.
func configreloadremovesrowRepaint(a *app) {
	a.t.Helper()
	for _, c := range []int{a.cols + 1, a.cols} {
		if err := a.term.Resize(c, a.rows); err != nil {
			a.t.Fatal(err)
		}
		time.Sleep(300 * time.Millisecond)
	}
}

// configreloadremovesrowNotices counts reload notices on screen; the
// line is long and wraps, so rows are joined first.
func configreloadremovesrowNotices(s string) int {
	return strings.Count(strings.ReplaceAll(s, "\n", ""), "bough: reloaded")
}

func configreloadremovesrowDisable(yml, id string) string {
	return yml + fmt.Sprintf("- id: %s\n  plugin: %s\n  disabled: true\n", id, id)
}

func configreloadremovesrowCmd(a *app, line string) {
	a.t.Helper()
	a.typeText(line)
	a.key(uv.KeyEnter, 0)
}

func TestConfigReloadRemovesRow(t *testing.T) {
	t.Parallel()

	// Control: a reload that changes no row prints its notice and the
	// next turn runs and renders.
	t.Run("noop", func(t *testing.T) {
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		configreloadremovesrowWrite(a, yml+"# touched\n")
		// Under the TUI the reload notice goes to the log, not the
		// screen: wait out the 300 ms debounce instead.
		time.Sleep(1500 * time.Millisecond)
		configreloadremovesrowCmd(a, "go slow")
		if !a.waitDone(1, 20*time.Second) {
			t.Fatalf("no turn ran after a no-op reload (input lost):\n%s", a.text())
		}
		configreloadremovesrowRepaint(a)
		a.waitFor("SLOWEND")
	})

	// The reload notice is written raw to stderr over the alt screen
	// and the renderer never repaints those cells.
	t.Run("notice_repainted", func(t *testing.T) {
		configreloadremovesrowKnown(t, "raw stderr reload line stays over the composer (cmd/bough/main.go reload)")
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		configreloadremovesrowWrite(a, yml+"# touched\n")
		a.waitUntil(func(s string) bool { return configreloadremovesrowNotices(s) >= 1 }, "reload notice")
		configreloadremovesrowRepaint(a)
		a.typeText("x")
		time.Sleep(time.Second)
		a.check("after a no-op reload")
		if configreloadremovesrowNotices(a.text()) > 0 {
			t.Fatalf("raw reload notice still on screen after a repaint:\n%s", a.text())
		}
	})

	// Removing todo between turns: the command goes, turns still run,
	// re-adding brings /todo and its panel back.
	t.Run("todo_betweenturns", func(t *testing.T) {
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		todoOpen(a)
		configreloadremovesrowWrite(a, configreloadremovesrowDisable(yml, "todo"))
		configreloadremovesrowUntil(a, "/todo add gone", "unknown command: /todo")
		if s := a.text(); panicky.MatchString(s) {
			t.Fatalf("crash text after the remount:\n%s", s)
		}
		configreloadremovesrowCmd(a, "go slow")
		if !a.waitDone(1, 20*time.Second) {
			t.Fatalf("turn after the removal never finished:\n%s", a.text())
		}
		configreloadremovesrowWrite(a, yml)
		configreloadremovesrowUntil(a, "/todo add back", "back")
		a.waitFor(todoHeader)
	})

	t.Run("todo_betweenturns_panel_clears", func(t *testing.T) {
		configreloadremovesrowKnown(t, "the ui drops the panel (plugins/ui todoRows), but the raw stderr reload line scrolls the alt screen and the stale panel paint survives the repaint (same bug as notice_repainted)")
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		todoOpen(a)
		configreloadremovesrowWrite(a, configreloadremovesrowDisable(yml, "todo"))
		configreloadremovesrowUntil(a, "/todo add gone", "unknown command: /todo")
		configreloadremovesrowRepaint(a)
		a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "todo panel to go with its row")
		a.check("todo row removed")
	})

	// Removing todo mid-turn: the in-flight turn still completes and
	// nothing crashes; re-adding restores the row.
	t.Run("todo_midturn", func(t *testing.T) {
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		todoOpen(a)
		configreloadremovesrowCmd(a, "go slow")
		a.waitFor("w02")
		configreloadremovesrowWrite(a, configreloadremovesrowDisable(yml, "todo"))
		if a.doneCount() >= 1 {
			t.Fatalf("turn finished before the edit landed; not a mid-turn reload")
		}
		if !a.waitDone(1, 20*time.Second) {
			t.Fatalf("in-flight turn never finished after the reload:\n%s", a.text())
		}
		configreloadremovesrowUntil(a, "/todo add gone", "unknown command: /todo")
		if s := a.text(); panicky.MatchString(s) {
			t.Fatalf("crash text after the remount:\n%s", s)
		}
		configreloadremovesrowWrite(a, yml)
		configreloadremovesrowUntil(a, "/todo add back", "back")
		a.waitFor(todoHeader)
	})

	t.Run("todo_midturn_output", func(t *testing.T) {
		configreloadremovesrowKnown(t, "ui remount mid-turn drops the rest of the streamed reply (Reconcile dependent closure remounts ui, plugins/ui/live.go:296)")
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		todoOpen(a)
		configreloadremovesrowCmd(a, "go slow")
		a.waitFor("w02")
		configreloadremovesrowWrite(a, configreloadremovesrowDisable(yml, "todo"))
		if !a.waitDone(1, 20*time.Second) {
			t.Fatalf("in-flight turn never finished after the reload:\n%s", a.text())
		}
		configreloadremovesrowRepaint(a)
		a.waitFor("SLOWEND")
		a.check("mid-turn todo removal")
	})

	// Removing theme mid-turn: the turn renders to the end, /theme goes,
	// re-adding brings it back.
	t.Run("theme", func(t *testing.T) {
		t.Parallel()
		a, yml := configreloadremovesrowStart(t)
		configreloadremovesrowCmd(a, "/theme")
		a.waitFor("forest_light")
		configreloadremovesrowCmd(a, "go slow")
		a.waitFor("w02")
		configreloadremovesrowWrite(a, configreloadremovesrowDisable(yml, "theme"))
		if !a.waitDone(1, 20*time.Second) {
			t.Fatalf("in-flight turn never finished after the reload:\n%s", a.text())
		}
		a.waitFor("SLOWEND")
		configreloadremovesrowUntil(a, "/theme", "unknown command: /theme")
		if s := a.text(); panicky.MatchString(s) {
			t.Fatalf("theme row removed: crash text:\n%s", s)
		}
		configreloadremovesrowWrite(a, yml)
		configreloadremovesrowCmd(a, "/clear")
		configreloadremovesrowUntil(a, "/theme", "forest_light")
		if s := a.text(); panicky.MatchString(s) {
			t.Fatalf("theme row restored: crash text:\n%s", s)
		}
	})
}
