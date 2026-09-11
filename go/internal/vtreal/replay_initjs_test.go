package vtreal

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	uv "github.com/charmbracelet/ultraviolet"
)

// initJsScript registers one of each: a theme token (user text red +
// bold), a keymap rebinding (clear_input on ctrl+g), and a command.
const initJsScript = `
bough.setup({ui: {theme: {user: "#ff0000:bold"}, keymap: {clear_input: "ctrl+g"}}})
bough.command("initjs-hello", "[name]", "greets from init.js", function (args) {
	return "INITJS_HELLO " + args
})
`

// initJsBoot is startCfg with $HOME/.bough/init.js written before the
// binary starts. The cwd is a subdirectory of $HOME: init-js also runs
// ./.bough/init.js, and with cwd == $HOME the same file would run twice
// (the second bough.command registration fails Apply). It does not
// wait for boot; the caller decides what a finished boot looks like.
func initJsBoot(t *testing.T, script string) *app {
	t.Helper()
	home := t.TempDir()
	work := filepath.Join(home, "work")
	for _, d := range []string{filepath.Join(home, ".bough"), work} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(home, ".bough", "init.js"), []byte(script), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(config), 0o644); err != nil {
		t.Fatal(err)
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = work
	cmd.Env = append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=",
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
	return a
}

func TestInitJsThemeToken(t *testing.T) {
	t.Parallel()
	a := initJsBoot(t, initJsScript)
	a.waitFor("say something")
	a.typeText("paint me")
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: paint me")
	a.settled()
	for _, row := range a.term.Snapshot().Cells {
		for _, c := range row {
			if c.Content != "p" || c.Style.Fg == nil {
				continue
			}
			r, g, b, _ := c.Style.Fg.RGBA()
			if r>>8 == 0xff && g == 0 && b == 0 && c.Style.Attrs&uv.AttrBold != 0 {
				return
			}
		}
	}
	t.Fatalf("no bold #ff0000 user-text cell: init.js theme token did not reach the terminal\nscreen:\n%s", a.text())
}

func TestInitJsReboundKey(t *testing.T) {
	t.Parallel()
	a := initJsBoot(t, initJsScript)
	a.waitFor("say something")
	a.typeText("draft to clear")
	a.waitFor("> draft to clear")
	a.key('g', uv.ModCtrl)
	a.waitUntil(func(s string) bool { return !strings.Contains(s, "draft to clear") },
		"ctrl+g (clear_input rebound in init.js) to clear the composer")
	if s := a.settled(); strings.Contains(s, "echo: draft") {
		t.Fatalf("ctrl+g submitted the draft instead of clearing it\nscreen:\n%s", s)
	}
}

func TestInitJsCommand(t *testing.T) {
	t.Parallel()
	a := initJsBoot(t, initJsScript)
	a.waitFor("say something")

	a.typeText("/help")
	a.key(uv.KeyEnter, 0)
	a.waitFor("greets from init.js")
	if s := a.settled(); !strings.Contains(s, "initjs-hello") {
		t.Fatalf("/help lists the summary but not the command name\nscreen:\n%s", s)
	}

	a.typeText("/initjs-hello world")
	a.key(uv.KeyEnter, 0)
	a.waitFor("INITJS_HELLO world")
	if s := a.settled(); strings.Contains(s, "echo: /initjs-hello") {
		t.Fatalf("/initjs-hello went to the model instead of the command\nscreen:\n%s", s)
	}
}

// A syntax error in init.js must be visible, naming the file: init-js
// publishes it as the "notice" service instead of failing its mount.
// TestInitJsSyntaxErrorKeepsBoot asserts the rest of the tree boots.
func TestInitJsSyntaxErrorVisible(t *testing.T) {
	t.Parallel()
	a := initJsBoot(t, "bough.setup({ui: {theme: {user: \"#ff0000\"}}\n")
	a.waitUntil(func(s string) bool {
		return strings.Contains(s, "init.js") && strings.Contains(s, "SyntaxError")
	}, "a SyntaxError naming init.js on screen")
}

func TestInitJsSyntaxErrorKeepsBoot(t *testing.T) {
	t.Parallel()
	a := initJsBoot(t, "bough.setup({\n")
	a.waitFor("say something")
	a.waitFor("init.js")
}
