package vtreal

// Fold clicks across a narrowing resize, on a real tmux server: a
// click opens a block, the pane shrinks so the block's long body
// re-wraps (shifting every row below it), and a click on the same
// logical row — found again by a screen scan — must toggle that block
// back, not a neighbour. Clicks are raw SGR mouse reports written to
// the pane's input, so the click lands exactly where the scan says.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// clickFoldNarrowTape writes a one-turn tape whose code line is long
// enough to wrap at 40 columns, with the result block right below it.
func clickFoldNarrowTape(t *testing.T) string {
	t.Helper()
	long := "console.log('" + strings.Repeat("wrap me ", 14) + "')"
	lines := []string{
		`{"seq":1,"at":"2026-09-10T10:00:00Z","kind":"meta","data":{"cwd":"/tmp/demo"}}`,
		`{"seq":2,"at":"2026-09-10T10:00:01Z","kind":"input","data":{"text":"say it long"}}`,
		`{"seq":3,"at":"2026-09-10T10:00:02Z","kind":"assistant","data":{"text":"` + "```js\\n" + long + "\\n```" + `"}}`,
		`{"seq":4,"at":"2026-09-10T10:00:02Z","kind":"code","data":{"text":"` + long + `\n"}}`,
		`{"seq":5,"at":"2026-09-10T10:00:02Z","kind":"result","data":{"code":"` + long + `\n","text":"one\ntwo\n"}}`,
		`{"seq":6,"at":"2026-09-10T10:00:03Z","kind":"assistant","data":{"text":"` + "```stop\\nSaid.\\n```" + `"}}`,
		`{"seq":7,"at":"2026-09-10T10:00:03Z","kind":"done","data":{"text":""}}`,
	}
	p := filepath.Join(t.TempDir(), "clickfold.jsonl")
	if err := os.WriteFile(p, []byte(strings.Join(lines, "\n")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// clickFoldNarrowRow is the screen row of a header with this glyph and
// tag, -1 when none is on screen.
func clickFoldNarrowRow(s, glyph, tag string) int {
	for i, l := range strings.Split(s, "\n") {
		if strings.Contains(l, glyph) && strings.Contains(l, tag) {
			return i
		}
	}
	return -1
}

// clickFoldNarrowClick presses and releases the left button at the
// 0-based cell (x, y).
func clickFoldNarrowClick(tm *tmuxApp, x, y int) {
	tm.keys("-l", fmt.Sprintf("\x1b[<0;%d;%dM", x+1, y+1))
	tm.keys("-l", fmt.Sprintf("\x1b[<0;%d;%dm", x+1, y+1))
}

// clickFoldNarrowToggle finds the header glyph+tag on a settled screen,
// clicks it, and waits for it to show want instead.
func clickFoldNarrowToggle(t *testing.T, tm *tmuxApp, cols int, glyph, tag, want, where string) string {
	t.Helper()
	s := resizeTmuxSettled(tm, cols)
	row := clickFoldNarrowRow(s, glyph, tag)
	if row < 0 {
		t.Fatalf("%s: no %s %q header on screen:\n%s", where, glyph, tag, s)
	}
	clickFoldNarrowClick(tm, 2, row)
	tm.waitUntil(func(string) bool {
		return clickFoldNarrowRow(resizeTmuxScreen(tm), want, tag) >= 0
	}, fmt.Sprintf("%s: %q toggled to %s", where, tag, want))
	return resizeTmuxSettled(tm, cols)
}

func TestClickFoldNarrowResize(t *testing.T) {
	t.Parallel()
	tm, home := resizeTmuxStart(t, 100, 30, replayConfig(clickFoldNarrowTape(t)))
	resizeTmuxSend(tm, "say it long")
	resizeTmuxWaitDone(t, tm, home, 1)

	// Open the code block at full width.
	s := clickFoldNarrowToggle(t, tm, 100, "▸", "code js", "▾", "open code @100")
	if clickFoldNarrowRow(s, "▾", "result (") >= 0 {
		t.Fatalf("opening code also opened result:\n%s", s)
	}
	wide := clickFoldNarrowRow(s, "▸", "result (")

	// Narrow: the long code line re-wraps, pushing result down.
	tm.resize(40, 30)
	resizeTmuxCheck(t, tm, "narrow to 40", 40)
	s = resizeTmuxSettled(tm, 40)
	if r := clickFoldNarrowRow(s, "▸", "result ("); r <= wide {
		t.Fatalf("result header did not move down on re-wrap (%d -> %d), the resize proves nothing:\n%s", wide, r, s)
	}

	// Same logical row: the open code header closes, result untouched.
	s = clickFoldNarrowToggle(t, tm, 40, "▾", "code js", "▸", "close code @40")
	if clickFoldNarrowRow(s, "▾", "result (") >= 0 {
		t.Fatalf("click on the code header opened result instead:\n%s", s)
	}

	// Result opens at its re-flowed row, code stays shut.
	s = clickFoldNarrowToggle(t, tm, 40, "▸", "result (", "▾", "open result @40")
	if clickFoldNarrowRow(s, "▾", "code js") >= 0 {
		t.Fatalf("click on the result header opened code:\n%s", s)
	}

	// Narrower still, then close result from its new row.
	tm.resize(30, 30)
	resizeTmuxCheck(t, tm, "narrow to 30", 30)
	s = clickFoldNarrowToggle(t, tm, 30, "▾", "result (", "▸", "close result @30")
	if clickFoldNarrowRow(s, "▾", "code js") >= 0 {
		t.Fatalf("closing result opened code:\n%s", s)
	}
}
