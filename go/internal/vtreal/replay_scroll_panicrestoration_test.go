package vtreal

// Panic restoration: a panic anywhere in the TUI must hand the terminal
// back (alt screen, mouse 1000/1002/1003/1006, bracketed paste, cursor)
// and leave the panic + stack on stderr and in ~/.bough/bough.log.
//
// A second bough binary is built with `go build -overlay`: model.go gets
// hook calls in Update, View and waitEvent, and one extra file arms them
// on the first wheel event, so no product source changes. BOUGH_PANIC_AT
// picks the site: update, view, cmd (a tea.Cmd), pump (the waitEvent
// goroutine that feeds loop events), goroutine (a plain goroutine, as a
// plugin emitting events would run).

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
)

const panicHookSrc = `package ui

import (
	"os"
	"sync/atomic"
	"time"

	tea "charm.land/bubbletea/v2"
)

var panicArmed atomic.Bool
var panicAt = os.Getenv("BOUGH_PANIC_AT")

func init() {
	if panicAt == "goroutine" {
		go func() {
			for !panicArmed.Load() {
				time.Sleep(10 * time.Millisecond)
			}
			panic("forced panic: plugin goroutine")
		}()
	}
}

func panicHookUpdate(msg tea.Msg) tea.Cmd {
	if _, ok := msg.(tea.MouseWheelMsg); !ok || panicAt == "" {
		return nil
	}
	panicArmed.Store(true)
	switch panicAt {
	case "update":
		panic("forced panic: Update")
	case "cmd":
		return func() tea.Msg { panic("forced panic: tea.Cmd") }
	}
	return nil
}

func panicHookView() {
	if panicAt == "view" && panicArmed.Load() {
		panic("forced panic: View")
	}
}

func panicHookPump() {
	if panicAt == "pump" && panicArmed.Load() {
		panic("forced panic: event pump")
	}
}
`

var (
	panicBinOnce sync.Once
	panicBin     string
	panicBinErr  error
)

func buildPanicBin(t *testing.T) string {
	t.Helper()
	panicBinOnce.Do(func() {
		dir, err := os.MkdirTemp("", "vtreal-panic-")
		if err != nil {
			panicBinErr = err
			return
		}
		src, _ := filepath.Abs("../../plugins/ui/model.go")
		b, err := os.ReadFile(src)
		if err != nil {
			panicBinErr = err
			return
		}
		s := string(b)
		for _, r := range [][2]string{
			{"func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {\n",
				"func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {\n\tif c := panicHookUpdate(msg); c != nil {\n\t\treturn m, c\n\t}\n"},
			{"func (m model) View() tea.View {\n", "func (m model) View() tea.View {\n\tpanicHookView()\n"},
			{"\t\tev, ok := <-m.events\n", "\t\tev, ok := <-m.events\n\t\tpanicHookPump()\n"},
		} {
			if strings.Count(s, r[0]) != 1 {
				panicBinErr = fmt.Errorf("overlay anchor not found: %q", r[0])
				return
			}
			s = strings.Replace(s, r[0], r[1], 1)
		}
		patched := filepath.Join(dir, "model.go")
		hook := filepath.Join(dir, "panichook.go")
		_ = os.WriteFile(patched, []byte(s), 0o644)
		_ = os.WriteFile(hook, []byte(panicHookSrc), 0o644)
		hookDst, _ := filepath.Abs("../../plugins/ui/zz_panichook.go")
		ov, _ := json.Marshal(map[string]any{"Replace": map[string]string{src: patched, hookDst: hook}})
		ovPath := filepath.Join(dir, "overlay.json")
		_ = os.WriteFile(ovPath, ov, 0o644)
		panicBin = filepath.Join(dir, "bough-panic"+exeSuffix())
		out, err := exec.Command("go", "build", "-overlay", ovPath, "-o", panicBin, "../../cmd/bough").CombinedOutput()
		if err != nil {
			panicBinErr = fmt.Errorf("build: %v: %s", err, out)
		}
	})
	if panicBinErr != nil {
		t.Fatalf("panic binary: %v", panicBinErr)
	}
	return panicBin
}

func startPanic(t *testing.T, at string) *app {
	t.Helper()
	pb := buildPanicBin(t)
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(pb, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(), "HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "BOUGH_PANIC_AT="+at)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 100, rows: 30, home: home}
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

func TestScrollPanicRestoration(t *testing.T) {
	for _, at := range []string{"update", "view", "cmd", "pump", "goroutine"} {
		t.Run(at, func(t *testing.T) {
			t.Parallel()
			a := startPanic(t, at)
			a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
			if at == "pump" { // the pump fires on the next loop event
				time.Sleep(200 * time.Millisecond)
				a.term.SendText("hi")
				a.term.SendKey(uv.KeyPressEvent{Code: uv.KeyEnter})
			}
			done := make(chan error, 1)
			go func() { done <- a.term.Wait(a.cmd) }()
			select {
			case <-done:
			case <-time.After(15 * time.Second):
				t.Fatalf("process did not die after the forced panic:\n%s", a.text())
			}
			var s Snapshot
			for range 40 { // let the emulator drain the restore bytes
				s = a.term.Snapshot()
				time.Sleep(50 * time.Millisecond)
			}
			var bad []string
			if s.AltScreen {
				bad = append(bad, "alt screen still on")
			}
			for _, m := range []ansi.DECMode{ansi.NormalMouseMode, ansi.ButtonEventMouseMode,
				ansi.AnyEventMouseMode, ansi.SgrExtMouseMode, ansi.ModeBracketedPaste} {
				if st, ok := s.DEC[m]; ok && st.IsSet() {
					bad = append(bad, fmt.Sprintf("DEC %d still set", int(m)))
				}
			}
			if !s.CursorVis {
				bad = append(bad, "cursor hidden")
			}
			scr := a.text()
			if !strings.Contains(scr, "panic") || !strings.Contains(scr, "goroutine ") {
				bad = append(bad, "panic text + stack not on the terminal")
			}
			logb, _ := os.ReadFile(filepath.Join(a.home, ".bough", "bough.log"))
			if !strings.Contains(string(logb), "forced panic") {
				bad = append(bad, "panic not in ~/.bough/bough.log")
			}
			if len(bad) > 0 {
				t.Errorf("%s: %s\nscreen:\n%s", at, strings.Join(bad, "; "), scr)
			}
		})
	}
}
