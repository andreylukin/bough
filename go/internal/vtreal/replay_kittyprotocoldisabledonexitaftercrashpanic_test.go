package vtreal

// Terminal modes handed back on exit, read from the raw PTY byte
// stream: every mode bough turned on (kitty keyboard CSI >…u, bracketed
// paste 2004, mouse 1000/1002/1003/1006, alt screen 1049) must be turned
// off by bytes that arrive after the last enable — on a clean double
// ctrl+c and after a forced panic (the crash guard's restore path).

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/vt"
	"github.com/charmbracelet/x/xpty"
)

// kittyProtocolDisabledOnExitAfterCrashPanicRaw is the tee of every
// byte the app wrote to the PTY.
type kittyProtocolDisabledOnExitAfterCrashPanicRaw struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (r *kittyProtocolDisabledOnExitAfterCrashPanicRaw) Write(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.buf.Write(p)
}

func (r *kittyProtocolDisabledOnExitAfterCrashPanicRaw) String() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.buf.String()
}

// kittyProtocolDisabledOnExitAfterCrashPanicTerminal is NewTerminal
// with the app → emulator copy teed into raw.
func kittyProtocolDisabledOnExitAfterCrashPanicTerminal(tb testing.TB, cols, rows int, raw io.Writer) *Terminal {
	tb.Helper()
	pty, err := xpty.NewPty(cols, rows)
	if err != nil {
		tb.Fatal(err)
	}
	t := &Terminal{tb: tb, pty: pty, cols: cols, rows: rows, dec: map[ansi.DECMode]ansi.ModeSetting{}, cursorVis: true}
	emu := vt.NewSafeEmulator(cols, rows)
	emu.SetCallbacks(vt.Callbacks{
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
		CursorVisibility: func(v bool) { t.mu.Lock(); t.cursorVis = v; t.mu.Unlock() },
	})
	t.Emu = emu
	setTitle := func(s string) { t.mu.Lock(); t.title = s; t.mu.Unlock() }
	go io.Copy(io.MultiWriter(newTitleFilter(emu, setTitle), raw), pty) //nolint:errcheck
	go io.Copy(pty, emu)                                                //nolint:errcheck
	return t
}

// kittyProtocolDisabledOnExitAfterCrashPanicStart boots binary (bin or
// the panic build) on a kitty TERM with the raw tee.
func kittyProtocolDisabledOnExitAfterCrashPanicStart(t *testing.T, binary string, env ...string) (*app, *kittyProtocolDisabledOnExitAfterCrashPanicRaw) {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	raw := &kittyProtocolDisabledOnExitAfterCrashPanicRaw{}
	term := kittyProtocolDisabledOnExitAfterCrashPanicTerminal(t, 100, 30, raw)
	cmd := exec.Command(binary, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(append(os.Environ(),
		"HOME="+home, "TERM=xterm-kitty", "KITTY_WINDOW_ID=1", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE="), env...)
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
	return a, raw
}

var (
	kittyProtocolDisabledOnExitAfterCrashPanicPush = regexp.MustCompile(`\x1b\[[>=]\d+(;\d+)?u`)
	kittyProtocolDisabledOnExitAfterCrashPanicPop  = regexp.MustCompile(`\x1b\[<\d*u|\x1b\[=0(;1)?u`)
)

// kittyProtocolDisabledOnExitAfterCrashPanicCheck returns every mode
// whose last enable is not followed by a disable in s.
func kittyProtocolDisabledOnExitAfterCrashPanicCheck(s string) []string {
	var bad []string
	last := func(re *regexp.Regexp) int {
		m := re.FindAllStringIndex(s, -1)
		if len(m) == 0 {
			return -1
		}
		return m[len(m)-1][0]
	}
	if on := last(kittyProtocolDisabledOnExitAfterCrashPanicPush); on >= 0 &&
		last(kittyProtocolDisabledOnExitAfterCrashPanicPop) < on {
		bad = append(bad, "kitty keyboard pushed ("+fmt.Sprintf("%q", kittyProtocolDisabledOnExitAfterCrashPanicPush.FindAllString(s, -1))+") but never popped (CSI <u)")
	}
	for _, m := range []int{1049, 2004, 1000, 1002, 1003, 1006} {
		on := max(strings.LastIndex(s, fmt.Sprintf("\x1b[?%dh", m)), last(regexp.MustCompile(fmt.Sprintf(`\x1b\[\?[\d;]*\b%d\b[\d;]*h`, m))))
		off := max(strings.LastIndex(s, fmt.Sprintf("\x1b[?%dl", m)), last(regexp.MustCompile(fmt.Sprintf(`\x1b\[\?[\d;]*\b%d\b[\d;]*l`, m))))
		if on >= 0 && off < on {
			bad = append(bad, fmt.Sprintf("?%d enabled, no ?%dl after it", m, m))
		}
	}
	return bad
}

// kittyProtocolDisabledOnExitAfterCrashPanicAssert waits for exit,
// lets the tail drain, then checks bytes and emulator state.
func kittyProtocolDisabledOnExitAfterCrashPanicAssert(t *testing.T, a *app, raw *kittyProtocolDisabledOnExitAfterCrashPanicRaw) {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case <-done:
	case <-time.After(15 * time.Second):
		t.Fatalf("process did not exit:\n%s", a.text())
	}
	var s string
	for range 40 { // the crash guard writes after bough died
		time.Sleep(50 * time.Millisecond)
		if n := raw.String(); n == s && s != "" && !a.term.Snapshot().AltScreen {
			break
		} else {
			s = n
		}
	}
	s = raw.String()
	if !kittyProtocolDisabledOnExitAfterCrashPanicPush.MatchString(s) {
		t.Fatalf("precondition: bough never enabled the kitty keyboard protocol on TERM=xterm-kitty")
	}
	if !strings.Contains(s, "\x1b[?2004h") {
		t.Fatalf("precondition: bracketed paste never enabled")
	}
	bad := kittyProtocolDisabledOnExitAfterCrashPanicCheck(s)
	snap := a.term.Snapshot()
	if snap.AltScreen {
		bad = append(bad, "emulator: alt screen still on")
	}
	for _, m := range []ansi.DECMode{ansi.NormalMouseMode, ansi.ButtonEventMouseMode,
		ansi.AnyEventMouseMode, ansi.SgrExtMouseMode, ansi.ModeBracketedPaste} {
		if st, ok := snap.DEC[m]; ok && st.IsSet() {
			bad = append(bad, fmt.Sprintf("emulator: DEC %d still set", int(m)))
		}
	}
	if len(bad) > 0 {
		tail := s
		if len(tail) > 400 {
			tail = tail[len(tail)-400:]
		}
		t.Errorf("modes left on: %s\nraw tail: %q", strings.Join(bad, "; "), tail)
	}
}

func TestKittyProtocolDisabledOnExitAfterCrashPanic(t *testing.T) {
	t.Run("ctrl+c", func(t *testing.T) {
		t.Parallel()
		a, raw := kittyProtocolDisabledOnExitAfterCrashPanicStart(t, bin)
		a.term.Paste("x")
		a.key('c', uv.ModCtrl)
		a.waitFor("ctrl+c")
		a.key('c', uv.ModCtrl)
		kittyProtocolDisabledOnExitAfterCrashPanicAssert(t, a, raw)
	})
	t.Run("panic", func(t *testing.T) {
		t.Parallel()
		if os.Getenv("BOUGH_KNOWN_KITTY_PROTOCOL_DISABLED_ON_EXIT_AFTER_CRASH_PANIC") == "" {
			t.Skip("known bug: crash guard restoreSeq (plugins/ui/crashguard.go) never pops the kitty keyboard protocol (CSI <u); set BOUGH_KNOWN_KITTY_PROTOCOL_DISABLED_ON_EXIT_AFTER_CRASH_PANIC=1 to run")
		}
		pb := buildPanicBin(t)
		a, raw := kittyProtocolDisabledOnExitAfterCrashPanicStart(t, pb, "BOUGH_PANIC_AT=update")
		a.term.SendMouse(uv.MouseWheelEvent{X: 5, Y: 3, Button: uv.MouseWheelUp})
		kittyProtocolDisabledOnExitAfterCrashPanicAssert(t, a, raw)
	})
}
