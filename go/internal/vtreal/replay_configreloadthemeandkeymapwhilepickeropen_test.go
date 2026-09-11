package vtreal

// Config hot reload while the /model picker is open. The picker takes
// the whole pane; with it up the test rewrites ~/.bough/init.js (the
// quit binding, which the picker treats as "go back", moves from
// ctrl+c to ctrl+q) and the project overlay (theme forest -> dracula,
// and the init-js row's config, so the reload re-applies init.js:
// editing init.js alone reloads nothing). The reload is waited on
// deterministically by its visible effect: the picker's focus row
// turns dracula's focus colour (the ui row remounted on the new
// services). What must hold: the picker is still open and usable, one
// picker header and no duplicated row, the NEW binding closes it (esc
// always does: it is not bindable), and after close the status bar is
// in dracula's colours.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func configReloadThemeAndKeymapWhilePickerOpenYml(tape, theme string, gen int) string {
	return fmt.Sprintf(`
- id: llm
  plugin: replay
  config: {file: %q}
- id: theme
  plugin: theme
  config: {name: %s}
- id: init-js
  plugin: init-js
  config: {gen: %d}
`, tape, theme, gen) + statusbarQuietRows
}

type configReloadThemeAndKeymapWhilePickerOpenRun struct {
	*app
	cfg, initjs, tape string
}

// configReloadThemeAndKeymapWhilePickerOpenBoot boots on forest with no
// keymap override, in $HOME/work, and opens the /model picker.
func configReloadThemeAndKeymapWhilePickerOpenBoot(t *testing.T) *configReloadThemeAndKeymapWhilePickerOpenRun {
	t.Helper()
	home := t.TempDir()
	work := filepath.Join(home, "work")
	for _, d := range []string{filepath.Join(home, ".bough"), work} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// One turn the test never plays: replay refuses a tape with no reply.
	tape := statusbarSeed(t, "idle.jsonl",
		statusbarEntry("meta", map[string]any{"cwd": work}),
		statusbarEntry("input", map[string]any{"text": "unused"}),
		statusbarEntry("assistant", map[string]any{"text": "```stop\nUNUSED\n```"}),
		statusbarEntry("done", map[string]any{"usage": statusbarUsage}),
	)
	initjs := filepath.Join(home, ".bough", "init.js")
	if err := os.WriteFile(initjs, []byte("bough.setup({})\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(work, "bough.yml")
	if err := os.WriteFile(cfg, []byte(configReloadThemeAndKeymapWhilePickerOpenYml(tape, "forest", 1)), 0o644); err != nil {
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
	a.waitFor("say something")
	a.typeText("/model")
	a.key(uv.KeyEnter, 0)
	a.waitFor("pick a model")
	r := &configReloadThemeAndKeymapWhilePickerOpenRun{app: a, cfg: cfg, initjs: initjs, tape: tape}
	r.waitFocus("#dbbc7f", "forest's focus colour on the picker's ▸ row")
	return r
}

// focusFg is the foreground of the picker's highlighted row ("▸ x"),
// "" when no row is highlighted.
func (r *configReloadThemeAndKeymapWhilePickerOpenRun) focusFg() string {
	snap := r.term.Snapshot()
	for y, l := range strings.Split(r.text(), "\n") {
		if strings.HasPrefix(l, "▸ ") && y < len(snap.Cells) && len(snap.Cells[y]) > 2 {
			h, _ := themeHex(snap.Cells[y][2].Style.Fg)
			return h
		}
	}
	return ""
}

func (r *configReloadThemeAndKeymapWhilePickerOpenRun) waitFocus(hex, what string) {
	r.t.Helper()
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		if r.focusFg() == hex {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	r.t.Fatalf("timed out waiting for %s (focus fg %q):\n%s", what, r.focusFg(), r.text())
}

// reload rewrites init.js (quit -> ctrl+q) and the overlay (dracula,
// init-js gen 2), then waits for the picker to repaint in dracula.
func (r *configReloadThemeAndKeymapWhilePickerOpenRun) reload() {
	r.t.Helper()
	if err := os.WriteFile(r.initjs, []byte(`bough.setup({ui: {keymap: {quit: "ctrl+q"}}})`+"\n"), 0o644); err != nil {
		r.t.Fatal(err)
	}
	if err := os.WriteFile(r.cfg, []byte(configReloadThemeAndKeymapWhilePickerOpenYml(r.tape, "dracula", 2)), 0o644); err != nil {
		r.t.Fatal(err)
	}
	r.waitFocus("#f1fa8c", "dracula's focus colour on the picker's ▸ row (the reload, with the picker still open)")
}

// pickerIntact asserts one picker, its rows unduplicated, no crash.
func (r *configReloadThemeAndKeymapWhilePickerOpenRun) pickerIntact(where string) {
	r.t.Helper()
	s := r.settled()
	if panicky.MatchString(s) {
		r.t.Fatalf("%s: crash text on screen:\n%s", where, s)
	}
	if n := strings.Count(s, "pick a model"); n != 1 {
		r.t.Fatalf("%s: %d picker headers, want 1:\n%s", where, n, s)
	}
	seen := map[string]bool{}
	for _, l := range strings.Split(s, "\n") {
		l = strings.TrimPrefix(strings.TrimPrefix(l, "▸ "), "  ")
		if strings.TrimSpace(l) == "" || strings.Contains(l, "more above") || strings.Contains(l, "more below") {
			continue
		}
		if seen[l] {
			r.t.Fatalf("%s: row %q appears twice:\n%s", where, l, s)
		}
		seen[l] = true
	}
	if strings.Contains(s, "say something") {
		r.t.Fatalf("%s: the composer is showing under/instead of the picker:\n%s", where, s)
	}
}

// Reload with the picker open: it stays open and still filters.
func TestConfigReloadThemeAndKeymapWhilePickerOpenStaysUsable(t *testing.T) {
	t.Parallel()
	r := configReloadThemeAndKeymapWhilePickerOpenBoot(t)
	r.pickerIntact("before reload")
	r.reload()
	r.pickerIntact("after reload")
	r.typeText("zzzz-no-such-model")
	r.waitFor("nothing matches")
	r.key(uv.KeyEscape, 0)
	r.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") }, "esc to close the picker")
	r.check("after esc")
}

// The rebound quit key (ctrl+q) closes the picker after the reload;
// the old one (ctrl+c) is then plain quit, so it is not pressed here.
func TestConfigReloadThemeAndKeymapWhilePickerOpenNewBindingCloses(t *testing.T) {
	t.Parallel()
	r := configReloadThemeAndKeymapWhilePickerOpenBoot(t)
	r.reload()
	r.key('q', uv.ModCtrl)
	r.waitUntil(func(s string) bool { return !strings.Contains(s, "pick a model") },
		"ctrl+q (the reloaded quit binding) to close the picker")
	r.check("after ctrl+q")
	s := r.settled()
	if n := strings.Count(s, "? keys"); n != 1 {
		t.Fatalf("%d status bars after close, want 1:\n%s", n, s)
	}
	// Colours after close: the status bar wears dracula's status bg.
	snap := r.term.Snapshot()
	ls := strings.Split(s, "\n")
	y := composerRow(ls) - 1
	if y < 0 {
		t.Fatalf("no status bar row:\n%s", s)
	}
	x := strings.Index(ls[y], "? keys")
	if x < 0 {
		t.Fatalf("no \"? keys\" on the status row:\n%s", s)
	}
	x = len([]rune(ls[y][:x]))
	if bg, _ := themeHex(snap.Cells[y][x].Style.Bg); bg != "#44475a" {
		t.Fatalf("status bar bg %q after close, want dracula #44475a:\n%s", bg, s)
	}
	// The picker reopens once, on the new palette.
	r.typeText("/model")
	r.key(uv.KeyEnter, 0)
	r.waitFor("pick a model")
	r.waitFocus("#f1fa8c", "the reopened picker in dracula")
	r.pickerIntact("reopened")
}
