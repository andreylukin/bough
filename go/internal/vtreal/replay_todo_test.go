package vtreal

// The pinned todo panel on a real PTY: a resumed session whose history
// carries todo/add and todo/done entries, the /todo mutation that puts
// the list on screen, and ctrl+t as the way to put it away and bring
// it back — at a narrow and a wide pane.
//
// The tape (testdata/replay/todo.jsonl) is used twice: the replay rows
// read it as the recording, and the history row RESUMES it, so the
// todo plugin derives the recorded items instead of a fresh list.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// todoHeader is the panel's first row; it names the key that hides it.
const todoHeader = "todo · ctrl+t hides"

// todoStart boots bough on the todo tape with history resuming a
// writable copy of it, so the session already has three items, one done.
func todoStart(t *testing.T, cols, rows int) *app {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/todo.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	src, err := os.ReadFile(tape)
	if err != nil {
		t.Fatal(err)
	}
	resume := filepath.Join(t.TempDir(), "todo.jsonl")
	if err := os.WriteFile(resume, src, 0o644); err != nil {
		t.Fatal(err)
	}
	yml := strings.Replace(replayConfig(tape),
		"- id: history\n  plugin: history",
		"- id: history\n  plugin: history\n  config: {file: "+resume+"}", 1)
	if !strings.Contains(yml, "config: {file: "+resume) {
		t.Fatalf("could not point the history row at %s:\n%s", resume, yml)
	}
	return startCfg(t, cols, rows, yml)
}

// todoOpen types a /todo mutation and waits for the panel: the panel
// tracks todo events, and nothing emits one until the list changes.
func todoOpen(a *app) {
	a.t.Helper()
	a.typeText("/todo add ship it")
	a.key(uv.KeyEnter, 0)
	a.waitFor(todoHeader)
}

// todoRow is the index of the panel header on screen, -1 when the
// panel is not drawn.
func todoRow(lines []string) int {
	for i, l := range lines {
		if strings.Contains(l, todoHeader) {
			return i
		}
	}
	return -1
}

func TestTodoPanel(t *testing.T) {
	t.Parallel()
	for _, cols := range []int{40, 100} {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			a := todoStart(t, cols, 30)
			todoOpen(a)
			s := a.settled()
			ls := strings.Split(s, "\n")

			// The recorded items, the recorded checkmark, and the one
			// this turn added.
			for _, want := range []string{
				"[ ] 1 cut the tag",
				"[x] 2 review the diff",
				"[ ] 3 write the notes",
				"[ ] 4 ship it",
			} {
				if !strings.Contains(s, want) {
					t.Fatalf("%d cols: todo panel is missing %q:\n%s", cols, want, s)
				}
			}

			// The panel sits directly above the composer, which stays
			// visible and typeable with it open.
			head, comp := todoRow(ls), composerRow(ls)
			if head < 0 || comp < 0 || head >= comp {
				t.Fatalf("%d cols: panel (row %d) must sit above the composer (row %d):\n%s", cols, head, comp, s)
			}
			a.check("todo panel open")
		})
	}
}

func TestTodoToggleHidesAndRestores(t *testing.T) {
	t.Parallel()
	a := todoStart(t, 100, 30)
	todoOpen(a)

	a.key('t', uv.ModCtrl)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, todoHeader) }, "the todo panel to hide")
	a.check("todo panel hidden")
	if s := a.settled(); strings.Contains(s, "[x] 2 review the diff") {
		t.Fatalf("ctrl+t hid the header but left the items:\n%s", s)
	}

	a.key('t', uv.ModCtrl)
	a.waitFor(todoHeader)
	if s := a.settled(); !strings.Contains(s, "[x] 2 review the diff") {
		t.Fatalf("ctrl+t should bring the whole panel back:\n%s", s)
	}
	a.check("todo panel back")
}

// Esc is not a way out of the todo panel: it is pinned until ctrl+t
// unpins it, and esc belongs to the overlays and the running turn.
func TestTodoEscKeepsPanel(t *testing.T) {
	t.Parallel()
	a := todoStart(t, 100, 30)
	todoOpen(a)
	a.key(uv.KeyEscape, 0)
	a.key(uv.KeyEscape, 0)
	s := a.settled()
	if !strings.Contains(s, todoHeader) || !strings.Contains(s, "[ ] 1 cut the tag") {
		t.Fatalf("esc should leave the pinned todo panel alone (only ctrl+t hides it):\n%s", s)
	}
	a.check("esc with the todo panel open")
}

// Until something mutates the list, a resumed session has no panel:
// the panel is fed by todo events, and boot emits none.
func TestTodoNoPanelBeforeMutation(t *testing.T) {
	t.Parallel()
	a := todoStart(t, 100, 30)
	if s := a.settled(); strings.Contains(s, todoHeader) {
		t.Fatalf("the todo panel should not be pinned before a todo event:\n%s", s)
	}
	a.key('t', uv.ModCtrl)
	a.waitFor("no todo list yet")
	a.check("ctrl+t with no todo list")
}
