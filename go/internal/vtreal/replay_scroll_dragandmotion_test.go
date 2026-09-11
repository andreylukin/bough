package vtreal

// Drag selection and motion under scrolling, on a real PTY: presses,
// drags past every edge, releases outside the pane, wheel in all four
// directions mid-drag, coordinates beyond the pane, and raw SGR mouse
// bytes (oversized, truncated, split across writes). After each burst
// bough must still be alive, draw its frame, and keep mouse bytes out
// of the composer.

import (
	"fmt"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// dragAndMotionAlive checks the process has not exited and the frame
// holds, with no SGR residue ("[<") typed into the composer.
func dragAndMotionAlive(t *testing.T, a *app, where string) {
	t.Helper()
	if a.cmd.ProcessState != nil {
		t.Fatalf("%s: bough exited:\n%s", where, a.text())
	}
	a.check(where)
	ls := a.lines()
	if r := composerRow(ls); r >= 0 && strings.Contains(ls[r], "[<") {
		t.Errorf("%s: SGR mouse bytes leaked into the composer: %q", where, ls[r])
	}
}

func TestScrollDragAndMotion(t *testing.T) {
	const cols, rows = 80, 24
	a := scrollingApp(t, cols, rows, 6)
	scrollingWheel(a, 5, true)
	a.settled()

	// Press, drag past the top and bottom edges with wheel mid-drag,
	// release below the pane.
	a.term.SendMouse(uv.MouseClickEvent{X: 10, Y: 5, Button: uv.MouseLeft})
	for y := 5; y >= -3; y-- {
		a.term.SendMouse(uv.MouseMotionEvent{X: 0, Y: y, Button: uv.MouseLeft})
	}
	for _, b := range []uv.MouseButton{uv.MouseWheelUp, uv.MouseWheelDown, uv.MouseWheelLeft, uv.MouseWheelRight} {
		a.term.SendMouse(uv.MouseWheelEvent{X: 3, Y: 3, Button: b})
	}
	for y := 0; y < rows+5; y++ {
		a.term.SendMouse(uv.MouseMotionEvent{X: cols - 1, Y: y, Button: uv.MouseLeft})
	}
	a.term.SendMouse(uv.MouseReleaseEvent{X: cols + 10, Y: rows + 10, Button: uv.MouseLeft})
	dragAndMotionAlive(t, a, "drag past edges")

	// Motion without a press, clicks beyond the pane, column 0 and the
	// last column, then a drag while the view jumps to the bottom.
	for x := -2; x < cols+3; x += 7 {
		a.term.SendMouse(uv.MouseMotionEvent{X: x, Y: rows / 2})
		a.click(x, rows+2)
	}
	a.term.SendMouse(uv.MouseClickEvent{X: 0, Y: 2, Button: uv.MouseLeft})
	a.term.SendMouse(uv.MouseMotionEvent{X: cols - 1, Y: 8, Button: uv.MouseLeft})
	a.key(uv.KeyEnd, 0)
	a.term.SendMouse(uv.MouseMotionEvent{X: 5, Y: 1, Button: uv.MouseLeft})
	a.term.SendMouse(uv.MouseReleaseEvent{X: 5, Y: 1, Button: uv.MouseLeft})
	dragAndMotionAlive(t, a, "no-press motion + out-of-pane clicks")

	// Raw SGR bytes straight into the PTY: huge, truncated, split.
	raw := []string{
		"\x1b[<65;999;999M", "\x1b[<64;99999;99999M", "\x1b[<0;999;999M", "\x1b[<32;999;999M", "\x1b[<0;999;999m",
		"\x1b[<66;1;1M", "\x1b[<67;80;24M", "\x1b[<35;0;0M", "\x1b[<0;0;0M", "\x1b[<0;0;0m",
		"\x1b[<65;4", ";5M", "\x1b[<", "64;10;10M", "\x1b[<65;4",
	}
	for _, s := range raw {
		_, _ = a.term.pty.Write([]byte(s))
		time.Sleep(5 * time.Millisecond)
	}
	for i := range 200 {
		_, _ = fmt.Fprintf(a.term.pty, "\x1b[<%d;%d;%dM", 64+i%4, 1+i%cols, 1+i%rows)
	}
	dragAndMotionAlive(t, a, "raw SGR bytes")
}

// TestScrollDragAndMotionTmux sends raw SGR bytes through a real
// terminal multiplexer and checks bough survives and its composer
// stays clean.
func TestScrollDragAndMotionTmux(t *testing.T) {
	tm := startTmux(t, 80, 24)
	for i := range 8 {
		tm.keys("-l", fmt.Sprintf("line %d %s", i, strings.Repeat("word ", 30)))
		tm.keys("Enter")
		time.Sleep(300 * time.Millisecond)
	}
	tm.settled()
	seqs := []string{
		"\x1b[<64;10;10M", "\x1b[<65;999;999M", "\x1b[<66;5;5M", "\x1b[<67;5;5M",
		"\x1b[<0;10;5M", "\x1b[<32;10;1M", "\x1b[<32;80;30M", "\x1b[<0;200;200m",
		"\x1b[<35;40;12M", "\x1b[<65;4", ";5M", "\x1b[<65;4",
	}
	for _, s := range seqs {
		tm.keys("-l", s)
	}
	s := tm.settled()
	if panicky.MatchString(s) {
		t.Fatalf("crash text on screen:\n%s", s)
	}
	if !strings.Contains(s, "? keys") {
		t.Fatalf("status bar gone (bough exited?):\n%s", s)
	}
	for _, l := range strings.Split(s, "\n") {
		if strings.HasPrefix(l, "> ") && strings.Contains(l, "[<") {
			t.Errorf("SGR bytes in the composer: %q", l)
		}
	}
}
