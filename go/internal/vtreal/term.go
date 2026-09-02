package vtreal

// A headless terminal: charmbracelet/x/vt emulator wired to a real PTY
// (x/xpty), with a snapshot of what the screen shows. This is
// x/vttest's shape with its locking fixed: vttest holds one mutex
// across emulator calls while the emulator's callbacks take the same
// mutex (Resize → cursor callback deadlocks; Snapshot vs Write is a
// lock-order inversion). Here the callback state has its own mutex
// that is never held across an emulator call.

import (
	"fmt"
	"io"
	"os/exec"
	"sync"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"
	"github.com/charmbracelet/x/xpty"
)

type Terminal struct {
	Emu *vt.SafeEmulator
	tb  testing.TB
	pty xpty.Pty

	mu        sync.Mutex // guards the fields below only
	cols      int
	rows      int
	title     string
	altScreen bool
	dec       map[ansi.DECMode]ansi.ModeSetting
	cursor    uv.Position
	cursorVis bool
}

type Snapshot struct {
	Cols, Rows int
	Title      string
	AltScreen  bool
	DEC        map[ansi.DECMode]ansi.ModeSetting
	Cursor     uv.Position
	CursorVis  bool
	Cells      [][]uv.Cell
}

func NewTerminal(tb testing.TB, cols, rows int) (*Terminal, error) {
	pty, err := xpty.NewPty(cols, rows)
	if err != nil {
		return nil, fmt.Errorf("pty: %w", err)
	}
	t := &Terminal{tb: tb, pty: pty, cols: cols, rows: rows, dec: map[ansi.DECMode]ansi.ModeSetting{}, cursorVis: true}
	emu := vt.NewSafeEmulator(cols, rows)
	emu.SetCallbacks(vt.Callbacks{
		Title:     func(s string) { t.mu.Lock(); t.title = s; t.mu.Unlock() },
		AltScreen: func(alt bool) { t.mu.Lock(); t.altScreen = alt; t.mu.Unlock() },
		EnableMode: func(mode ansi.Mode) {
			if m, ok := mode.(ansi.DECMode); ok {
				t.mu.Lock()
				t.dec[m] = ansi.ModeSet
				t.mu.Unlock()
			}
		},
		DisableMode: func(mode ansi.Mode) {
			if m, ok := mode.(ansi.DECMode); ok {
				t.mu.Lock()
				t.dec[m] = ansi.ModeReset
				t.mu.Unlock()
			}
		},
		CursorPosition:   func(_, p uv.Position) { t.mu.Lock(); t.cursor = p; t.mu.Unlock() },
		CursorVisibility: func(v bool) { t.mu.Lock(); t.cursorVis = v; t.mu.Unlock() },
	})
	t.Emu = emu
	go io.Copy(emu, pty) //nolint:errcheck // app output → emulator
	go io.Copy(pty, emu) //nolint:errcheck // emulator input (keys, replies) → app
	return t, nil
}

func (t *Terminal) Start(cmd *exec.Cmd) error { return t.pty.Start(cmd) }

func (t *Terminal) Wait(cmd *exec.Cmd) error { return xpty.WaitProcess(t.tb.Context(), cmd) }

// Close closes the PTY; the emulator is left open on purpose. x/vt's
// SafeEmulator.Close races its own Read (the io.Copy goroutine), which
// the race detector flags on every run. The reader goroutine leaks
// quietly for the rest of the test binary.
func (t *Terminal) Close() error { return t.pty.Close() }

func (t *Terminal) Resize(cols, rows int) error {
	t.mu.Lock()
	t.cols, t.rows = cols, rows
	t.mu.Unlock()
	t.Emu.Resize(cols, rows)
	return t.pty.Resize(cols, rows)
}

func (t *Terminal) SendText(s string)         { t.Emu.SendText(s) }
func (t *Terminal) SendKey(k uv.KeyEvent)     { t.Emu.SendKey(k) }
func (t *Terminal) SendMouse(m uv.MouseEvent) { t.Emu.SendMouse(m) }
func (t *Terminal) Paste(s string)            { t.Emu.Paste(s) }

func (t *Terminal) Snapshot() Snapshot {
	t.mu.Lock()
	s := Snapshot{Cols: t.cols, Rows: t.rows, Title: t.title, AltScreen: t.altScreen,
		Cursor: t.cursor, CursorVis: t.cursorVis, DEC: make(map[ansi.DECMode]ansi.ModeSetting, len(t.dec))}
	for k, v := range t.dec {
		s.DEC[k] = v
	}
	t.mu.Unlock()
	// Draw copies the screen into a private buffer under the emulator's
	// lock; CellAt would hand back pointers into the live buffer that
	// the parser goroutine keeps writing to.
	buf := uv.NewScreenBuffer(s.Cols, s.Rows)
	t.Emu.Draw(&buf, uv.Rect(0, 0, s.Cols, s.Rows))
	s.Cells = make([][]uv.Cell, s.Rows)
	for y := 0; y < s.Rows; y++ {
		s.Cells[y] = make([]uv.Cell, s.Cols)
		for x := 0; x < s.Cols; x++ {
			if c := buf.CellAt(x, y); c != nil {
				s.Cells[y][x] = *c
			}
		}
	}
	return s
}
