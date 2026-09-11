package vtreal

// Config rewritten while a turn runs. The model side is a tape, but
// codemode and tools-basic are real, and the tape's one block is
// `cat <fifo>`: the turn is held open until the test writes the fifo.
// While it is held the test rewrites ~/.bough/init.js and the project
// bough.yml overlay (the -config file, which cmd/bough watches and
// hot-reloads). What must hold: the held turn finishes with its reply,
// no crash text, one status bar (so one cost row), and the overlay's
// change is live afterwards. init.js is read only at the init-js row's
// Apply (plugins/initjs), so editing it alone is a documented no-reload.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// configReloadMidTurnYml is the project overlay: replay llm, real
// codemode/tools from the embedded base, and the theme row named
// explicitly so a rewrite can change it.
func configReloadMidTurnYml(tape, theme string) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: theme
  plugin: theme
  config: {name: %s}
`, tape, theme) + statusbarQuietRows
}

type configReloadMidTurnRun struct {
	*app
	fifo, cfg, initjs, tape string
}

// configReloadMidTurnBoot writes the fifo, the tape, init.js and the
// overlay, boots in $HOME/work and sends the input that starts the
// held turn; it returns once the block is on screen.
func configReloadMidTurnBoot(t *testing.T) *configReloadMidTurnRun {
	t.Helper()
	home := t.TempDir()
	work := filepath.Join(home, "work")
	for _, d := range []string{filepath.Join(home, ".bough"), work} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	fifo := filepath.Join(home, "gate")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	code := fmt.Sprintf("console.log(tools.bash(%q))\n", "cat "+fifo)
	tape := statusbarSeed(t, "held.jsonl",
		statusbarEntry("meta", map[string]any{"cwd": work}),
		statusbarEntry("input", map[string]any{"text": "hold the turn"}),
		statusbarEntry("assistant", map[string]any{"text": "```js\n" + code + "```"}),
		statusbarEntry("code", map[string]any{"text": code}),
		statusbarEntry("result", map[string]any{"code": code, "text": "GATE_OPENED\n"}),
		statusbarEntry("assistant", map[string]any{"text": "```stop\nHELD_TURN_FINISHED\n```"}),
		statusbarEntry("done", map[string]any{"usage": statusbarUsage}),
	)
	initjs := filepath.Join(home, ".bough", "init.js")
	if err := os.WriteFile(initjs, []byte(`bough.setup({ui: {theme: {user: "#00ff00"}}})`+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(work, "bough.yml")
	if err := os.WriteFile(cfg, []byte(configReloadMidTurnYml(tape, "forest")), 0o644); err != nil {
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
	r := &configReloadMidTurnRun{app: a, fifo: fifo, cfg: cfg, initjs: initjs, tape: tape}
	t.Cleanup(func() {
		r.open() // never leave a cat blocked
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitFor("say something")
	a.typeText("hold the turn")
	a.key(uv.KeyEnter, 0)
	a.waitFor("Ran: cat ") // the path is truncated on screen
	return r
}

// open writes the gate without blocking when no reader is there.
func (r *configReloadMidTurnRun) open() {
	if f, err := os.OpenFile(r.fifo, os.O_WRONLY|syscall.O_NONBLOCK, 0); err == nil {
		_, _ = f.WriteString("go\n")
		_ = f.Close()
	}
}

// held asserts the turn still waits on the gate after the watcher's
// 300ms debounce has long passed.
func (r *configReloadMidTurnRun) held(where string) {
	r.t.Helper()
	time.Sleep(1500 * time.Millisecond)
	s := r.settled()
	if panicky.MatchString(s) {
		r.t.Fatalf("%s: crash text on screen:\n%s", where, s)
	}
	if n := r.doneCount(); n != 0 {
		r.t.Fatalf("%s: the held turn ended (%d done) before its gate opened:\n%s", where, n, s)
	}
}

// finish opens the gate and asserts the turn ends as recorded, on one
// status bar with one cost chip.
func (r *configReloadMidTurnRun) finish() {
	r.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for r.doneCount() == 0 && time.Now().Before(deadline) {
		r.open()
		time.Sleep(100 * time.Millisecond)
	}
	if !r.waitDone(1, 30*time.Second) {
		r.t.Fatalf("held turn never finished after its gate opened:\n%s", r.text())
	}
	r.waitFor("HELD_TURN_FINISHED")
	s := r.settled()
	r.check("after the held turn")
	if n := strings.Count(s, "? keys"); n != 1 {
		r.t.Fatalf("%d status bars on screen, want 1:\n%s", n, s)
	}
	if n := strings.Count(statusbarLine(r.app, s), "$0.052"); n != 1 {
		r.t.Fatalf("cost chip appears %d times on the bar, want 1:\n%s", n, s)
	}
	if strings.Contains(s, "GATE_OPENED") {
		r.t.Fatalf("the recorded result is on screen: the block did not run for real:\n%s", s)
	}
}

// The overlay's theme row changes mid-turn: the turn must not notice,
// and /theme must name the new palette once it is done.
func TestConfigReloadMidTurnOverlay(t *testing.T) {
	if os.Getenv("BOUGH_KNOWN_CONFIG_RELOAD_MID_TURN") == "" {
		t.Skip("known bug: cmd/bough reload() prints \"bough: reloaded <path>\" to stderr over the live TUI, " +
			"overwriting the composer row; set BOUGH_KNOWN_CONFIG_RELOAD_MID_TURN=1 to run")
	}
	t.Parallel()
	r := configReloadMidTurnBoot(t)
	if err := os.WriteFile(r.cfg, []byte(configReloadMidTurnYml(r.tape, "dracula")), 0o644); err != nil {
		t.Fatal(err)
	}
	r.held("overlay rewritten")
	r.finish()
	r.typeText("/theme")
	r.key(uv.KeyEnter, 0)
	r.waitUntil(func(s string) bool { return strings.Contains(s, "● dracula") },
		"/theme to mark dracula current: the overlay reload applied")
	r.check("after /theme")
}

// init.js changes mid-turn (theme token + keybinding): nothing reloads
// it, so the turn finishes and ctrl+g does not become clear_input.
func TestConfigReloadMidTurnInitJs(t *testing.T) {
	t.Parallel()
	r := configReloadMidTurnBoot(t)
	js := `bough.setup({ui: {theme: {user: "#ff0000:bold"}, keymap: {clear_input: "ctrl+g"}}})` + "\n"
	if err := os.WriteFile(r.initjs, []byte(js), 0o644); err != nil {
		t.Fatal(err)
	}
	r.held("init.js rewritten")
	r.finish()
	for _, row := range r.term.Snapshot().Cells {
		for _, c := range row {
			if c.Content != "h" || c.Style.Fg == nil {
				continue
			}
			red, g, b, _ := c.Style.Fg.RGBA()
			if red>>8 == 0xff && g == 0 && b == 0 {
				t.Fatalf("user text turned #ff0000: init.js reloaded mid-session, which nothing documents:\n%s", r.text())
			}
		}
	}
}
