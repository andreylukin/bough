package vtreal

// The terminal environment matrix: bough must boot, keep its documented
// mouse mode (cell motion + SGR, plugins/ui/model.go), put no garbage
// on screen and lay out exactly like the baseline whatever TERM, LANG,
// COLORTERM and TMUX say — colours are the only thing allowed to vary.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"unicode"

	"github.com/charmbracelet/x/ansi"
)

// terminalEnvStart boots the echo config like startCfg, but builds the
// terminal environment from scratch: TERM, COLORTERM, locale and TMUX
// are dropped from the inherited environment and only extra is added,
// so a variable left out of extra is really unset.
func terminalEnvStart(t *testing.T, cols, rows int, extra ...string) *app {
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
	var env []string
	for _, kv := range os.Environ() {
		switch k, _, _ := strings.Cut(kv, "="); k {
		case "TERM", "COLORTERM", "LANG", "LC_ALL", "LC_CTYPE", "TMUX", "TMUX_PANE",
			"NO_COLOR", "BOUGH_VERBOSE", "HOME":
			continue
		}
		env = append(env, kv)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(append(env, "HOME="+home), extra...)
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
	a.waitFor("say something") // boot is done
	return a
}

// A CSI that lost its ESC prints as "[?25h", "[38;5;2m" and the like.
var terminalEnvDebris = regexp.MustCompile(`\[\??[0-9]+(;[0-9]+)*[A-Za-z~]`)

// terminalEnvGarbage describes the first garbage on screen, "" if clean.
func terminalEnvGarbage(a *app) string {
	for y, row := range a.term.Snapshot().Cells {
		for x, c := range row {
			for _, r := range c.Content {
				if r == unicode.ReplacementChar || r < 0x20 || (r >= 0x7f && r <= 0x9f) {
					return fmt.Sprintf("rune %U at col %d row %d", r, x, y)
				}
			}
		}
	}
	if m := terminalEnvDebris.FindString(a.text()); m != "" {
		return "escape debris " + m
	}
	return ""
}

// The system prompt embeds the temp $HOME path, so its length varies
// run to run; the count is masked before layouts are compared.
var terminalEnvPromptChars = regexp.MustCompile(`[0-9]+ chars in the system prompt`)

// terminalEnvTurn boots, runs one echo turn and returns the settled
// screen with the prompt length masked.
func terminalEnvTurn(t *testing.T, extra ...string) (*app, string) {
	a := terminalEnvStart(t, 100, 30, extra...)
	a.typeText("hello env")
	a.key('\r', 0)
	a.waitFor("echo: hello env")
	return a, terminalEnvPromptChars.ReplaceAllString(a.settled(), "N chars in the system prompt")
}

func TestTerminalEnv(t *testing.T) {
	t.Parallel()
	_, baseline := terminalEnvTurn(t, "TERM=xterm-256color", "COLORTERM=truecolor", "LANG=en_US.UTF-8")

	cases := []struct {
		name string
		env  []string
	}{
		{"TermDumb", []string{"TERM=dumb", "LANG=en_US.UTF-8"}},
		{"TermXterm", []string{"TERM=xterm", "LANG=en_US.UTF-8"}},
		{"TermScreen256", []string{"TERM=screen-256color", "LANG=en_US.UTF-8"}},
		{"LangC", []string{"TERM=xterm-256color", "COLORTERM=truecolor", "LANG=C"}},
		{"NoColorterm", []string{"TERM=xterm-256color", "LANG=en_US.UTF-8"}},
		{"NoTmux", []string{"TERM=xterm-256color", "COLORTERM=truecolor", "LANG=en_US.UTF-8"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			a, screen := terminalEnvTurn(t, tc.env...)
			snap := a.term.Snapshot()
			if !snap.AltScreen {
				t.Fatalf("%v: not in the alt screen\nscreen:\n%s", tc.env, a.text())
			}
			if !snap.DEC[ansi.ButtonEventMouseMode].IsSet() || !snap.DEC[ansi.SgrExtMouseMode].IsSet() {
				t.Fatalf("%v: mouse modes 1002+1006 not both set: %v\nscreen:\n%s", tc.env, snap.DEC, a.text())
			}
			if snap.DEC[ansi.AnyEventMouseMode].IsSet() {
				t.Fatalf("%v: all-motion mouse (1003) set outside the board\nscreen:\n%s", tc.env, a.text())
			}
			if g := terminalEnvGarbage(a); g != "" {
				t.Fatalf("%v: garbage on screen: %s\nscreen:\n%s", tc.env, g, a.text())
			}
			if screen != baseline {
				t.Fatalf("%v: layout differs from the baseline\nbaseline:\n%s\nscreen:\n%s", tc.env, baseline, a.text())
			}
		})
	}
}
