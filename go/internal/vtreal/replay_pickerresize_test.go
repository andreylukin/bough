package vtreal

// Pickers under a real tmux resize: the action palette, the "@" file
// picker, the /model picker and the /sessions picker are each opened,
// moved off their first row, then resized to 30x10 and to 160x50. At
// every size the overlay must fit the pane (no row wider than it) and
// keep the highlighted row on screen, still the same item; enter must
// then dispatch that item.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// pickerResizeSel returns the highlighted row with its whitespace
// collapsed and a truncation "…" dropped, "" when no row is
// highlighted. marker is "> " (palette, @) or "▸ " (model, sessions);
// the composer row, which also starts "> ", is skipped.
func pickerResizeSel(screen, marker string) string {
	ls := strings.Split(screen, "\n")
	c := composerRow(ls)
	for i, l := range ls {
		if i == c || !strings.HasPrefix(l, marker) {
			continue
		}
		return strings.TrimSpace(strings.TrimRight(strings.Join(strings.Fields(l[len(marker):]), " "), "…"))
	}
	return ""
}

// pickerResizeSame: the same row at two widths — one is a truncation
// (or a narrower layout) of the other.
func pickerResizeSame(a, b string) bool {
	return a != "" && b != "" && (strings.HasPrefix(a, b) || strings.HasPrefix(b, a))
}

// pickerResizeSettled waits for a frame at most rows tall with a
// highlighted row, then for it to stop moving. The model and sessions
// pickers take the whole screen (no status bar), so resizeTmuxSettled
// cannot be used.
func pickerResizeSettled(tm *tmuxApp, rows int, marker string) string {
	tm.waitUntil(func(string) bool {
		s := resizeTmuxScreen(tm)
		return len(strings.Split(s, "\n")) <= rows && pickerResizeSel(s, marker) != ""
	}, fmt.Sprintf("a %d-row frame with a highlighted row", rows))
	tm.settled()
	return resizeTmuxScreen(tm)
}

// pickerResizeCheck resizes, waits for the redrawn frame, and asserts
// the overlay fits and the highlight is still the same row.
func pickerResizeCheck(t *testing.T, tm *tmuxApp, cols, rows int, marker, want string) {
	t.Helper()
	tm.resize(cols, rows)
	s := pickerResizeSettled(tm, rows, marker)
	for i, l := range strings.Split(s, "\n") {
		if w := len([]rune(l)); w > cols {
			t.Errorf("%dx%d: row %d is %d cells wide:\n%s", cols, rows, i, w, s)
		}
	}
	if got := pickerResizeSel(s, marker); !pickerResizeSame(got, want) {
		t.Errorf("%dx%d: highlighted row %q, want %q still visible:\n%s", cols, rows, got, want, s)
	}
}

// pickerResizeRun opens a picker (open), moves the highlight down
// once, sweeps both sizes, presses enter and hands the highlighted
// row to dispatched.
func pickerResizeRun(t *testing.T, tm *tmuxApp, marker string, open func(), dispatched func(sel string)) {
	t.Helper()
	open()
	first := pickerResizeSel(pickerResizeSettled(tm, 30, marker), marker)
	tm.keys("Down")
	tm.waitUntil(func(string) bool {
		s := pickerResizeSel(resizeTmuxScreen(tm), marker)
		return s != "" && s != first
	}, "the highlight to move off "+first)
	sel := pickerResizeSel(pickerResizeSettled(tm, 30, marker), marker)
	pickerResizeCheck(t, tm, 30, 10, marker, sel)
	pickerResizeCheck(t, tm, 160, 50, marker, sel)
	if t.Failed() {
		return
	}
	tm.keys("Enter")
	dispatched(sel)
}

func pickerResizeStart(t *testing.T) (*tmuxApp, string) {
	t.Helper()
	return resizeTmuxStart(t, 100, 30, replayConfig(resizeTmuxTape(t)))
}

func TestPickerResize(t *testing.T) {
	t.Parallel()

	t.Run("ActionPalette", func(t *testing.T) {
		t.Parallel()
		tm, _ := pickerResizeStart(t)
		pickerResizeRun(t, tm, "> ", func() {
			tm.keys("C-x")
			tm.waitFor("ctrl+x …")
			tm.keys("p")
			tm.waitFor("collapse_all")
			tm.keys("_all")
			tm.waitUntil(func(s string) bool { return !strings.Contains(s, "clear_input") }, "filter to _all")
		}, func(sel string) {
			verb := strings.TrimSuffix(strings.Fields(sel)[0], "_all") // expand / collapse
			tm.waitUntil(func(s string) bool {
				return !strings.Contains(s, "action · ") && strings.Contains(s, verb)
			}, sel+" to run")
		})
	})

	t.Run("AtPicker", func(t *testing.T) {
		t.Parallel()
		tm, home := pickerResizeStart(t)
		for _, name := range []string{"alpha.md", "beta.md", "gamma.md"} {
			if err := os.WriteFile(filepath.Join(home, name), []byte("x\n"), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		pickerResizeRun(t, tm, "> ", func() {
			tm.keys("@")
			tm.waitFor("@alpha.md")
		}, func(sel string) {
			name := strings.Fields(sel)[0]
			tm.waitUntil(func(s string) bool {
				ls := strings.Split(s, "\n")
				r := composerRow(ls)
				return r >= 0 && strings.Contains(ls[r], name) && pickerResizeSel(s, "> ") == ""
			}, name+" completed into the composer")
		})
	})

	t.Run("ModelPicker", func(t *testing.T) {
		t.Parallel()
		tm, _ := pickerResizeStart(t)
		pickerResizeRun(t, tm, "▸ ", func() {
			resizeTmuxSend(tm, "/model")
			tm.waitFor("pick a model")
		}, func(sel string) {
			f := strings.Fields(sel)
			model := f[len(f)-1]
			tm.waitUntil(func(s string) bool {
				return !strings.Contains(s, "pick a model") && strings.Contains(s, model)
			}, model+" picked")
		})
	})

	t.Run("SessionsPicker", func(t *testing.T) {
		t.Parallel()
		tm, home := pickerResizeStart(t)
		dir := filepath.Join(home, ".bough", "history")
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		at := time.Now().UTC().Format(time.RFC3339)
		for _, id := range []string{"alpha", "beta", "gamma"} {
			var sb strings.Builder
			for i, e := range []struct {
				kind string
				data map[string]any
			}{
				{"meta", map[string]any{"cwd": "/x/" + id}},
				{"input", map[string]any{"text": id + "prompt"}},
				{"assistant", map[string]any{"text": "reply to " + id}},
				{"done", map[string]any{}},
			} {
				b, _ := json.Marshal(map[string]any{"seq": i + 1, "at": at, "kind": e.kind, "data": e.data})
				sb.Write(append(b, '\n'))
			}
			if err := os.WriteFile(filepath.Join(dir, "seed-"+id+".jsonl"), []byte(sb.String()), 0o644); err != nil {
				t.Fatal(err)
			}
		}
		pickerResizeRun(t, tm, "▸ ", func() {
			resizeTmuxSend(tm, "/sessions")
			tm.waitFor("resume a session")
		}, func(sel string) {
			id := ""
			for _, f := range strings.Fields(sel) {
				if strings.HasSuffix(f, "prompt") {
					id = strings.TrimSuffix(f, "prompt")
				}
			}
			if id == "" {
				t.Fatalf("highlighted session row %q names no seeded session", sel)
			}
			tm.waitFor("resumed seed-" + id)
			tm.waitFor("reply to " + id)
		})
	})
}
