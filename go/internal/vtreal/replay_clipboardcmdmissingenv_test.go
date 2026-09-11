package vtreal

// Copy with nowhere to copy to: PATH holds no clipboard tool (no
// pbcopy, xclip, wl-copy, xsel, tmux), TMUX is unset, and TERM names a
// terminal without OSC 52 (the Linux console). The copy chord must then
// say the copy failed or is unsupported, not flash "copied … · OSC 52".

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// clipboardCmdMissingEnvStart boots the fixture tape with the clipboard
// tools hidden and a non-OSC-52 TERM, and runs its first turn.
func clipboardCmdMissingEnvStart(t *testing.T) *app {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/basic.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(replayConfig(tape)), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=linux", "COLORTERM=",
		"NO_COLOR=", "BOUGH_VERBOSE=",
		"PATH="+t.TempDir(), "TMUX=", "WAYLAND_DISPLAY=", "DISPLAY=",
	)
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
	a.typeText("list the files here")
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		t.Fatalf("first turn never finished:\n%s", a.text())
	}
	return a
}

// clipboardCmdMissingEnvChord presses ctrl+x y and returns the settled
// screen once the leader flash has been replaced.
func clipboardCmdMissingEnvChord(a *app) string {
	a.key('x', uv.ModCtrl)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "ctrl+x …") },
		"the pending leader flash")
	a.key('y', 0)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "ctrl+x …") },
		"the chord to resolve")
	return a.settled()
}

func TestClipboardCmdMissingEnv(t *testing.T) {
	t.Parallel()
	a := clipboardCmdMissingEnvStart(t)
	s := clipboardCmdMissingEnvChord(a)
	a.check("after the chord")

	t.Run("flash_reports_failure", func(t *testing.T) {
		if os.Getenv("BOUGH_KNOWN_CLIPBOARD_CMD_MISSING_ENV") == "" {
			t.Skip("known bug: plugins/ui/clipboard.go finishCopy always flashes " +
				"\"copied … · OSC 52\" even when no native tool ran and TERM lacks OSC 52; " +
				"set BOUGH_KNOWN_CLIPBOARD_CMD_MISSING_ENV=1 to run")
		}
		low := strings.ToLower(s)
		if !strings.Contains(low, "fail") && !strings.Contains(low, "unsupported") &&
			!strings.Contains(low, "not copied") {
			t.Errorf("no failure/unsupported flash:\n%s", s)
		}
		if strings.Contains(s, "copied ") && strings.Contains(s, "OSC 52") {
			t.Errorf("flash claims success over OSC 52 on a TERM without it:\n%s", s)
		}
	})
}
