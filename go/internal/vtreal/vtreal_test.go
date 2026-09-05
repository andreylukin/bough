// Package vtreal runs the built bough binary inside a headless virtual
// terminal (charmbracelet/x/vttest: x/vt emulator over a real PTY) and
// asserts on the rendered cell grid — alt screen, mouse, paste,
// resize, styles, exit — the seams the in-process harness cannot see.
//
// Every test owns its own PTY, HOME and config, so the suite is
// parallel-safe and never touches ~/.bough.
package vtreal

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
	"runtime"
)

var bin string // built once in TestMain

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "vtreal-")
	if err != nil {
		panic(err)
	}
	// ".exe" on Windows, or the file builds and then cannot be
	// executed: "executable file not found in %PATH%", which was
	// about half of the Windows failures on its own.
	bin = filepath.Join(dir, "bough"+exeSuffix())
	build := exec.Command("go", "build", "-o", bin, "../../cmd/bough")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		fmt.Fprintln(os.Stderr, "vtreal: build:", err)
		os.Exit(1)
	}
	code := m.Run()
	os.RemoveAll(dir)
	os.Exit(code)
}

// The smallest config that gives a full turn without a network: the
// echo llm, codemode (loop needs it), tools (bash for CODE!), commands
// (so "/" lines dispatch), history in $HOME, loop, ui.
const config = `
- id: llm
  plugin: llm-echo
- id: codemode
  plugin: codemode
- id: commands
  plugin: commands
- id: tools
  plugin: tools-basic
- id: history
  plugin: history
- id: loop
  plugin: loop
- id: ui
  plugin: ui
`

type app struct {
	t    *testing.T
	term *Terminal
	cmd  *exec.Cmd
	cols int
	rows int
}

func start(t *testing.T, cols, rows int) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
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
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something") // the composer placeholder: boot is done
	return a
}

// text returns the screen as rows of plain text (trailing blanks trimmed).
func (a *app) text() string {
	snap := a.term.Snapshot()
	rows := make([]string, len(snap.Cells))
	for y, row := range snap.Cells {
		var sb strings.Builder
		for _, c := range row {
			if c.Width == 0 {
				continue // wide-char continuation
			}
			if c.Content == "" {
				sb.WriteByte(' ')
			} else {
				sb.WriteString(c.Content)
			}
		}
		rows[y] = strings.TrimRight(sb.String(), " ")
	}
	return strings.Join(rows, "\n")
}

func (a *app) lines() []string { return strings.Split(a.text(), "\n") }

func (a *app) waitFor(substr string) {
	a.t.Helper()
	a.waitUntil(func(s string) bool { return strings.Contains(s, substr) }, "screen to contain "+substr)
}

func (a *app) waitUntil(pred func(screen string) bool, what string) {
	a.t.Helper()
	// Generous: under a full -race run a bough boot can take well over
	// a few seconds; a real regression still fails, just later.
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if pred(a.text()) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.t.Fatalf("vtreal: timed out waiting for %s\nscreen:\n%s", what, a.text())
}

// settled waits until two consecutive snapshots 60ms apart match.
func (a *app) settled() string {
	prev := a.text()
	for range 50 {
		time.Sleep(60 * time.Millisecond)
		cur := a.text()
		if cur == prev {
			return cur
		}
		prev = cur
	}
	return prev
}

func (a *app) typeText(s string) { a.term.SendText(s) }

func (a *app) key(code rune, mod uv.KeyMod) {
	a.term.SendKey(uv.KeyPressEvent{Code: code, Mod: mod})
}

func (a *app) click(x, y int) {
	a.term.SendMouse(uv.MouseClickEvent{X: x, Y: y, Button: uv.MouseLeft})
	a.term.SendMouse(uv.MouseReleaseEvent{X: x, Y: y, Button: uv.MouseLeft})
}

// composerRow returns the index of the last row starting with the
// composer prompt, -1 when the composer is not on screen.
func composerRow(lines []string) int {
	for i, line := range slices.Backward(lines) {
		if strings.HasPrefix(line, "> ") {
			return i
		}
	}
	return -1
}

// --- tests ---

func TestBootEntersAltScreenWithMouse(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	snap := a.term.Snapshot()
	if !snap.AltScreen {
		t.Fatalf("not in the alt screen after boot")
	}
	mouse := false
	for mode, st := range snap.DEC {
		if (mode == ansi.ButtonEventMouseMode || mode == ansi.SgrExtMouseMode) && st.IsSet() {
			mouse = true
		}
	}
	if !mouse {
		t.Fatalf("mouse reporting not enabled: %v", snap.DEC)
	}
	if r := composerRow(a.lines()); r < 0 {
		t.Fatalf("composer prompt not on screen:\n%s", a.text())
	}
}

func TestEchoTurnRendersOnRealTerminal(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("hello real terminal")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: hello real terminal")
	s := a.settled()
	if !strings.Contains(s, "❯ hello real terminal") {
		t.Fatalf("user line missing:\n%s", s)
	}
	if strings.Contains(s, "> hello real terminal") {
		t.Fatalf("composer kept the submitted line:\n%s", s)
	}
}

// The composer draws a virtual cursor: the real terminal cursor is
// hidden and the cell right after the draft is reverse-video.
func TestVirtualCursorAfterDraft(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	a.typeText("abc")
	a.waitFor("> abc")
	a.settled()
	snap := a.term.Snapshot()
	row := composerRow(a.lines())
	if snap.CursorVis {
		t.Fatalf("real cursor visible at %v while the composer draws a virtual one", snap.Cursor)
	}
	cell := snap.Cells[row][len("> abc")]
	if cell.Style.Attrs&uv.AttrReverse == 0 {
		t.Fatalf("no reverse-video cursor cell after the draft (attrs=%b):\n%s", cell.Style.Attrs, a.text())
	}
}

// A bracketed paste with newlines lands in the composer as a multi-line
// draft; nothing is submitted until enter.
func TestBracketedPasteIsNotSubmitted(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	a.term.Paste("line one\nline two")
	a.waitFor("line two")
	s := a.settled()
	if strings.Contains(s, "❯ line one") || strings.Contains(s, "echo:") {
		t.Fatalf("paste was submitted:\n%s", s)
	}
	if !strings.Contains(s, "> line one") {
		t.Fatalf("paste not in composer:\n%s", s)
	}
}

// Shrinking and growing the terminal keeps the status bar and composer
// on the last rows. This one runs under tmux: x/vt drops the bottom
// rows after a resize (verified against tmux, where bough is fine), so
// the emulator cannot judge it.
func TestResizeKeepsComposerOnScreen(t *testing.T) {
	t.Parallel()
	tm := startTmux(t, 120, 40)
	tm.keys("resize me", "Enter")
	tm.waitFor("echo: resize me")
	for _, sz := range [][2]int{{40, 12}, {200, 50}, {60, 8}, {100, 30}} {
		tm.resize(sz[0], sz[1])
		tm.waitUntil(func(s string) bool {
			ls := strings.Split(s, "\n")
			return composerRow(ls) == len(ls)-1 && strings.Contains(s, "? keys")
		}, fmt.Sprintf("status bar + composer on the last rows after resize to %dx%d", sz[0], sz[1]))
		if s := tm.settled(); !strings.Contains(s, "resize me") {
			t.Fatalf("transcript lost after resize to %dx%d:\n%s", sz[0], sz[1], s)
		}
	}
}

// A collapsed code block expands on click and collapses again on a
// second click, through real SGR mouse reports.
func TestClickTogglesBlock(t *testing.T) {
	t.Parallel()
	a := start(t, 100, 30)
	a.typeText("CODE!")
	a.key(uv.KeyEnter, 0)
	a.waitFor("hi from codemode") // the tool output fed back and echoed
	s := a.settled()
	ls := strings.Split(s, "\n")
	row := -1
	for i, l := range ls {
		if strings.Contains(l, "▸") && strings.Contains(l, "Ran") {
			row = i
			break
		}
	}
	if row < 0 {
		t.Fatalf("no collapsed code block header:\n%s", s)
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "▾ Ran") }, "block expanded by click (▾)")
	// Expanding scrolls the transcript: find the open header where it is now.
	s = a.settled()
	row = -1
	for i, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "▾") && strings.Contains(l, "Ran") {
			row = i
		}
	}
	if row < 0 {
		t.Fatalf("no expanded code block header:\n%s", s)
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "▸ Ran") }, "block collapsed by second click (▸)")
}

// The user prompt marker carries the accent color: styling reaches the
// terminal (no NO_COLOR/colorprofile downgrade to plain text).
func TestUserMarkerIsStyled(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	a.typeText("styled")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: styled")
	a.settled()
	snap := a.term.Snapshot()
	for _, row := range snap.Cells {
		for _, c := range row {
			if c.Content == "❯" {
				if c.Style.Fg == nil {
					t.Fatalf("❯ has no foreground color; styling lost")
				}
				return
			}
		}
	}
	t.Fatalf("no ❯ cell on screen:\n%s", a.text())
}

// One ctrl+c arms, the second quits; the process exits 0 and the
// terminal leaves the alt screen.
func TestDoubleCtrlCExitsCleanly(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 24)
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c") // the quit hint
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v", err)
		}
	case <-time.After(8 * time.Second):
		t.Fatalf("process did not exit after two ctrl+c:\n%s", a.text())
	}
	// The process has exited; the emulator may still be draining the
	// bytes that leave the alt screen. Give it a moment under load.
	left := false
	for i := 0; i < 100 && !left; i++ {
		left = !a.term.Snapshot().AltScreen
		if !left {
			time.Sleep(50 * time.Millisecond)
		}
	}
	if !left {
		t.Fatalf("still in the alt screen after exit")
	}
}

// A long reply scrolls the transcript, not the composer: the prompt
// stays on the last rows and the newest text is visible.
func TestLongReplyKeepsComposerPinned(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 20)
	long := strings.Repeat("word ", 400)
	a.term.Paste(long)
	a.waitUntil(func(s string) bool { return strings.Count(s, "word") > 10 }, "paste in composer")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: word")
	s := a.settled()
	ls := strings.Split(s, "\n")
	if r := composerRow(ls); r < 0 || r < len(ls)-3 {
		t.Fatalf("composer not pinned to the bottom (row %d of %d):\n%s", r, len(ls), s)
	}
}

// Scrolling with the mouse wheel moves the transcript and the status
// bar reports it; the composer stays put.
func TestWheelScrollsTranscript(t *testing.T) {
	t.Parallel()
	a := start(t, 80, 16)
	for i := range 6 {
		a.typeText(fmt.Sprintf("turn %d", i))
		a.key(uv.KeyEnter, 0)
		a.waitFor(fmt.Sprintf("echo: turn %d", i))
	}
	a.settled()
	a.term.SendMouse(uv.MouseWheelEvent{X: 10, Y: 3, Button: uv.MouseWheelUp})
	a.term.SendMouse(uv.MouseWheelEvent{X: 10, Y: 3, Button: uv.MouseWheelUp})
	a.waitFor("scrolled")
	if composerRow(a.lines()) < 0 {
		t.Fatalf("composer lost after wheel scroll:\n%s", a.text())
	}
}

// exeSuffix is ".exe" on Windows and "" everywhere else.
func exeSuffix() string {
	if runtime.GOOS == "windows" {
		return ".exe"
	}
	return ""
}
