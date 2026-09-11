package vtreal

import (
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"
)

// Kitty keyboard protocol (CSI-u) input on a real PTY: bough booted
// with TERM advertising kitty, keys written as raw CSI-u bytes.

// kittyKeyboardProtocolStart is startCfg with a kitty TERM.
func kittyKeyboardProtocolStart(t *testing.T) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 80, 24)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-kitty", "KITTY_WINDOW_ID=1", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
	)
	if err := term.Start(cmd); err != nil {
		t.Fatal(err)
	}
	a := &app{t: t, term: term, cmd: cmd, cols: 80, rows: 24, home: home}
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

// kittyKeyboardProtocolRaw writes bytes straight to the app's PTY, the
// way a kitty terminal would, bypassing the emulator's key encoder.
func kittyKeyboardProtocolRaw(a *app, s string) {
	a.t.Helper()
	if _, err := a.term.pty.Write([]byte(s)); err != nil {
		a.t.Fatal(err)
	}
	time.Sleep(30 * time.Millisecond) // one key per read, like a person
}

// kittyKeyboardProtocolType sends each rune as a CSI-u press.
func kittyKeyboardProtocolType(a *app, s string) {
	for _, r := range s {
		kittyKeyboardProtocolRaw(a, "\x1b["+strconv.Itoa(int(r))+"u")
	}
}

// kittyKeyboardProtocolComposer is the composer's first line.
func kittyKeyboardProtocolComposer(a *app) string {
	lines := a.lines()
	if i := composerRow(lines); i >= 0 {
		return lines[i]
	}
	return ""
}

// kittyKeyboardProtocolRawLeak matches CSI-u residue on screen.
var kittyKeyboardProtocolRawLeak = regexp.MustCompile(`\[\d+(;[\d:]+)*u|\d+;[\d:]+u|\x1b`)

func kittyKeyboardProtocolNoLeak(t *testing.T, s string) {
	t.Helper()
	if m := kittyKeyboardProtocolRawLeak.FindString(s); m != "" {
		t.Fatalf("raw CSI bytes %q on screen:\n%s", m, s)
	}
}

func TestKittyKeyboardProtocol(t *testing.T) {
	t.Parallel()

	t.Run("plain_csi_u_text_types", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "hello")
		a.waitFor("> hello")
		kittyKeyboardProtocolNoLeak(t, a.settled())
	})

	t.Run("shift_enter_inserts_newline", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "one")
		a.waitFor("> one")
		kittyKeyboardProtocolRaw(a, "\x1b[13;2u") // shift+enter
		kittyKeyboardProtocolType(a, "two")
		a.waitFor("two")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "echo:") {
			t.Fatalf("shift+enter submitted:\n%s", s)
		}
		if strings.Contains(s, "onetwo") || !strings.Contains(s, "> one") {
			t.Fatalf("shift+enter did not break the line:\n%s", s)
		}
	})

	t.Run("enter_submits", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "ping")
		a.waitFor("> ping")
		kittyKeyboardProtocolRaw(a, "\x1b[13u")
		a.waitFor("echo:")
		kittyKeyboardProtocolNoLeak(t, a.settled())
	})

	t.Run("alt_enter_follow_up_submits_when_idle", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "later")
		a.waitFor("> later")
		kittyKeyboardProtocolRaw(a, "\x1b[13;3u") // alt+enter
		a.waitFor("echo:")
		kittyKeyboardProtocolNoLeak(t, a.settled())
	})

	t.Run("release_events_never_type", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "ab")
		a.waitFor("> ab")
		// Release events (event type 3) of letters, shifted letters,
		// enter and space: none may type or submit.
		for _, seq := range []string{"\x1b[120;1:3u", "\x1b[121;1:3u", "\x1b[122;2:3u", "\x1b[13;1:3u", "\x1b[32;1:3u"} {
			kittyKeyboardProtocolRaw(a, seq)
		}
		kittyKeyboardProtocolType(a, "c")
		a.waitFor("> abc")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if got := kittyKeyboardProtocolComposer(a); got != "> abc" {
			t.Fatalf("composer = %q after release events, want %q:\n%s", got, "> abc", s)
		}
		if strings.Contains(s, "echo:") {
			t.Fatalf("enter release submitted:\n%s", s)
		}
	})

	t.Run("esc_twice_clears_draft", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "draft")
		a.waitFor("> draft")
		kittyKeyboardProtocolRaw(a, "\x1b[27u")
		a.waitFor("press esc again to clear the draft")
		kittyKeyboardProtocolRaw(a, "\x1b[27u")
		a.waitUntil(func(s string) bool { return !strings.Contains(s, "> draft") }, "draft cleared")
		kittyKeyboardProtocolNoLeak(t, a.settled())
	})

	t.Run("ctrl_c_arms_quit_then_exits", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolRaw(a, "\x1b[99;5u")
		a.waitFor("ctrl+c")
		kittyKeyboardProtocolNoLeak(t, a.settled())
		kittyKeyboardProtocolRaw(a, "\x1b[99;5u")
		done := make(chan error, 1)
		go func() { done <- a.term.Wait(a.cmd) }()
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("exit: %v", err)
			}
		case <-time.After(8 * time.Second):
			t.Fatalf("no exit after two CSI-u ctrl+c:\n%s", a.text())
		}
	})

	t.Run("unknown_modifiers_leave_no_residue", func(t *testing.T) {
		t.Parallel()
		a := kittyKeyboardProtocolStart(t)
		kittyKeyboardProtocolType(a, "k")
		a.waitFor("> k")
		// super, hyper, meta, caps+num lock bits, an out-of-range mask,
		// an unassigned private-use key, F13, and a super repeat.
		for _, seq := range []string{
			"\x1b[97;9u", "\x1b[97;17u", "\x1b[97;33u", "\x1b[97;193u",
			"\x1b[97;999u", "\x1b[57999;5u", "\x1b[57376u", "\x1b[97;9:2u",
		} {
			kittyKeyboardProtocolRaw(a, seq)
		}
		kittyKeyboardProtocolType(a, "z")
		a.waitUntil(func(s string) bool { return strings.HasSuffix(kittyKeyboardProtocolComposer(a), "z") }, "z typed")
		s := a.settled()
		kittyKeyboardProtocolNoLeak(t, s)
		if strings.Contains(s, "echo:") {
			t.Fatalf("modifier key submitted:\n%s", s)
		}
	})
}
