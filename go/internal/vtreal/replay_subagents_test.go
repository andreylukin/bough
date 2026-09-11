package vtreal

// Subagent rendering from a recorded session. Replay never spawns, so
// the sub:* entries reach the UI the way they do after a restart: the
// history row resumes the tape itself and ui replays it into cards on
// boot. The checks are the card life cycle a user sees — collapsed
// heads, focus, enter folds the card open, ctrl+o opens the child's
// transcript in the overlay, esc returns to the spawner — at a narrow
// and a wide pane.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// subagentsConfig is replayConfig plus a history row resuming the same
// tape, so the recorded sub:* entries render on boot.
func subagentsConfig(tape string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: codemode
  plugin: replay
  config: {file: %q, provide: codemode}
- id: commands
  plugin: commands
- id: history
  plugin: history
  config: {file: %q}
- id: loop
  plugin: loop
- id: ui
  plugin: ui
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
`, tape, tape, tape)
}

// subagentsBoot copies the fixture into the test's own directory (the
// history row appends to the file it resumes) and boots on it.
func subagentsBoot(t *testing.T, cols, rows int) *app {
	t.Helper()
	src, err := filepath.Abs("testdata/replay/subagents.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(src)
	if err != nil {
		t.Fatal(err)
	}
	tape := filepath.Join(t.TempDir(), "subagents.jsonl")
	if err := os.WriteFile(tape, data, 0o644); err != nil {
		t.Fatal(err)
	}
	a := startCfg(t, cols, rows, subagentsConfig(tape))
	a.waitFor("subagent 2")
	return a
}

// subagentsEsc presses esc. A bare ESC byte sits in the app's input
// parser until the next keypress disambiguates it (alt+<key> vs esc),
// so a harmless right-arrow follows to flush it; on an empty composer
// that moves nothing. The pause between them matters: sent back to
// back, the two arrive in one read as "\x1b\x1b[C" (alt+right), not esc.
func subagentsEsc(a *app) {
	a.key(uv.KeyEscape, 0)
	a.settled()
	a.key(uv.KeyRight, 0)
}

// subagentsFocusCard tabs until the given card is focused, proved by
// ctrl+o opening that child's transcript; it leaves the overlay shut.
// tab starts at the newest block and walks older, so the number of
// presses is not fixed by the tape alone.
func subagentsFocusCard(a *app, want string) {
	a.t.Helper()
	for range 8 {
		a.key(uv.KeyTab, 0)
		a.settled()
		a.key('o', uv.ModCtrl)
		s := a.settled()
		open := strings.Contains(s, "esc to close")
		if open {
			subagentsEsc(a)
		} else {
			a.key('o', uv.ModCtrl) // the history inspector: its own key closes it
		}
		a.settled()
		if open && strings.Contains(s, want) {
			return
		}
	}
	a.t.Fatalf("never focused the card for %q (tab + ctrl+o never showed its transcript)\nscreen:\n%s", want, a.text())
}

func TestSubagentsCards(t *testing.T) {
	t.Parallel()
	for _, cols := range []int{60, 120} {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			a := subagentsBoot(t, cols, 30)
			a.check("boot")
			s := a.settled()
			for _, want := range []string{"subagent 1", "subagent 2"} {
				if !strings.Contains(s, want) {
					t.Fatalf("no card for %s on screen:\n%s", want, s)
				}
			}
			// Collapsed: one head row each, no body bar and no report.
			if strings.Contains(s, "┃") {
				t.Errorf("cards are not collapsed (body bar on screen):\n%s", s)
			}
			if strings.Contains(s, "ParseFile") {
				t.Errorf("collapsed card leaked the child's transcript:\n%s", s)
			}
			for _, l := range strings.Split(s, "\n") {
				if strings.Contains(l, "subagent 1") && !strings.Contains(l, "▸") {
					t.Errorf("card head has no collapsed glyph: %q\nscreen:\n%s", l, s)
				}
			}
		})
	}
}

func TestSubagentsFoldOpen(t *testing.T) {
	t.Parallel()
	for _, cols := range []int{60, 120} {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			a := subagentsBoot(t, cols, 30)
			subagentsFocusCard(a, "subagent 2")
			a.key(uv.KeyEnter, 0)
			s := a.settled()
			if !strings.Contains(s, "┃") {
				t.Fatalf("enter did not fold the card open (no body bar):\n%s", s)
			}
			if !strings.Contains(s, "17 test files") {
				t.Errorf("open card does not show the child's report:\n%s", s)
			}
			a.check("card open")
			// enter again closes it.
			a.key(uv.KeyEnter, 0)
			s = a.settled()
			if strings.Contains(s, "17 test files") {
				t.Errorf("enter did not fold the card shut again:\n%s", s)
			}
			a.check("card closed")
		})
	}
}

func TestSubagentsTranscriptOverlay(t *testing.T) {
	t.Parallel()
	for _, cols := range []int{60, 120} {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			a := subagentsBoot(t, cols, 30)
			subagentsFocusCard(a, "subagent 1")
			a.key('o', uv.ModCtrl)
			s := a.settled()
			if !strings.Contains(s, "subagent 1") {
				t.Fatalf("overlay does not name the subagent:\n%s", s)
			}
			// The child's own transcript: its call and that call's output,
			// which the collapsed card never showed.
			for _, want := range []string{"parser.go", "ParseFile"} {
				if !strings.Contains(s, want) {
					t.Errorf("overlay transcript missing %q:\n%s", want, s)
				}
			}
			if strings.Contains(s, "17 test files") {
				t.Errorf("overlay leaked the other subagent's transcript:\n%s", s)
			}
			a.check("overlay open")

			subagentsEsc(a)
			s = a.settled()
			if strings.Contains(s, "esc to close") {
				t.Fatalf("esc did not close the subagent transcript:\n%s", s)
			}
			if !strings.Contains(s, "subagent 2") {
				t.Errorf("esc did not return to the spawner's transcript:\n%s", s)
			}
			a.check("back from overlay")
		})
	}
}
