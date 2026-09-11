package vtreal

// Copy on a real PTY: drag-select in the transcript, and the leader
// chord copy action. What lands in the system clipboard cannot be
// observed here — OSC 52 bytes are consumed by the emulator and never
// reach Snapshot without a hook in term.go — so these tests assert the
// status-bar flash the copy raises, and that the next key clears it.
//
// The app boots with an empty PATH so the native clipboard tools
// (pbcopy, tmux load-buffer) are not found: the run must not clobber
// the developer's real clipboard, and the flash then reads
// "copied N … · OSC 52" on every platform.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// copyStart is startCfg with the clipboard tools hidden: same boot,
// PATH pointed at an empty directory.
func copyStart(t *testing.T, cols, rows int, yml string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, cols, rows)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
		"PATH="+t.TempDir(), "TMUX=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	return a
}

// copyBoot boots the fixture tape and runs its first turn, so the
// transcript holds a reply to select.
func copyBoot(t *testing.T) *app {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/basic.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	a := copyStart(t, 100, 30, replayConfig(tape))
	a.typeText("list the files here")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("first turn never finished:\n%s", a.text())
	}
	a.check("after turn 1")
	return a
}

// copyRow is the screen row holding substr, -1 when it is not there.
func copyRow(a *app, substr string) int {
	for i, l := range a.lines() {
		if strings.Contains(l, substr) {
			return i
		}
	}
	return -1
}

// copyDrag presses at (x0,y0), moves to (x1,y1) and releases.
func copyDrag(a *app, x0, y0, x1, y1 int) {
	a.term.SendMouse(uv.MouseClickEvent{X: x0, Y: y0, Button: uv.MouseLeft})
	a.term.SendMouse(uv.MouseMotionEvent{X: x1, Y: y1, Button: uv.MouseLeft})
	a.term.SendMouse(uv.MouseReleaseEvent{X: x1, Y: y1, Button: uv.MouseLeft})
}

// A drag over the transcript raises the "copied …" flash, and the next
// key clears it again.
func TestCopyDragFlash(t *testing.T) {
	t.Parallel()
	a := copyBoot(t)
	row := copyRow(a, "Two Go files")
	if row < 0 {
		t.Fatalf("the reply is not on screen:\n%s", a.text())
	}
	copyDrag(a, 2, row, 20, row+1)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "copied ") },
		"the copy flash after a drag")
	s := a.settled()
	if !strings.Contains(s, "OSC 52") {
		t.Errorf("the flash should name the OSC 52 path:\n%s", s)
	}
	a.check("after the drag")

	// The flash is a one-key chip: the next key press clears it.
	a.typeText("x")
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "copied ") },
		"the copy flash to expire on the next key")
	a.check("after the flash expired")
}

// A press and release with no motion is a click, not a selection: it
// toggles a block and never copies.
func TestCopyPlainClickDoesNotCopy(t *testing.T) {
	t.Parallel()
	a := copyBoot(t)
	row := copyRow(a, "Two Go files")
	if row < 0 {
		t.Fatalf("the reply is not on screen:\n%s", a.text())
	}
	a.click(2, row)
	if s := a.settled(); strings.Contains(s, "copied ") {
		t.Errorf("a plain click copied:\n%s", s)
	}
	a.check("after the click")
}

// The leader chord (ctrl+x y) copies the last reply: the leader shows
// as a pending flash, and the chord replaces it with "copied …".
func TestCopyChordCopiesLastReply(t *testing.T) {
	t.Parallel()
	a := copyBoot(t)
	a.key('x', uv.ModCtrl)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "ctrl+x …") },
		"the pending leader flash")
	a.key('y', 0)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "copied ") },
		"the copy flash after ctrl+x y")
	if s := a.settled(); !strings.Contains(s, "OSC 52") {
		t.Errorf("the flash should name the OSC 52 path:\n%s", s)
	}
	a.check("after the chord")
}
