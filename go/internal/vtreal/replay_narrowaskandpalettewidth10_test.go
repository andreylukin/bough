package vtreal

// Ask card, action palette and /model picker in 10-20 column panes:
// no crash text, no row wider than the pane, the composer pinned to
// column 0 (a sideways-scrolled viewport pushes it off the left
// edge), and a number key still answers the option it names.

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

var narrowAskAndPaletteWidth10Sizes = [][2]int{{10, 14}, {14, 16}, {20, 12}}

const narrowAskAndPaletteWidth10Third = "charlie_unbroken_identifier_that_is_very_long_indeed"

func narrowAskAndPaletteWidth10Each(t *testing.T, body func(t *testing.T, a *app)) {
	tape, _ := filepath.Abs("testdata/replay/narrow_ask_palette_width10.jsonl")
	for _, sz := range narrowAskAndPaletteWidth10Sizes {
		t.Run(fmt.Sprintf("%dx%d", sz[0], sz[1]), func(t *testing.T) {
			t.Parallel()
			body(t, narrowAskAndPaletteWidth10Boot(t, sz[0], sz[1], askConfig(tape)))
		})
	}
}

// narrowAskAndPaletteWidth10Boot is startCfg at the size under test.
// startCfg waits for the whole "say something" placeholder, which a
// 10-column composer truncates, and Terminal.Resize on this PTY does
// not reach bough (resize tests go through tmux), so boot here and
// wait for the composer row instead.
func narrowAskAndPaletteWidth10Boot(t *testing.T, cols, rows int, yml string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
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
	a := &app{t: t, term: term, cmd: cmd, cols: cols, rows: rows, home: home}
	t.Cleanup(func() {
		if cmd.ProcessState == nil {
			_ = cmd.Process.Kill()
			_, _ = cmd.Process.Wait()
		}
		_ = term.Close()
	})
	a.waitUntil(func(s string) bool {
		ls := strings.Split(s, "\n")
		r := composerRow(ls)
		return r >= 0 && strings.HasPrefix(ls[r], "> say")
	}, "the composer placeholder (boot done)")
	return a
}

// narrowAskAndPaletteWidth10Check: no crash, no overwide row, and the
// composer's "> " at column 0 on one of the last rows.
func narrowAskAndPaletteWidth10Check(a *app, where string) {
	a.t.Helper()
	narrowAskAndPaletteWidth10Screen(a, where, true)
}

// narrowAskAndPaletteWidth10Screen is the check; composer=false for
// full-screen overlays (the /model picker) that hide the composer.
func narrowAskAndPaletteWidth10Screen(a *app, where string, composer bool) {
	a.t.Helper()
	s := a.settled()
	ls := strings.Split(s, "\n")
	if panicky.MatchString(s) {
		a.t.Errorf("%s: crash text on screen:\n%s", where, s)
	}
	if r := composerRow(ls); composer && (r < 0 || r < len(ls)-4) {
		a.t.Errorf("%s: composer not at column 0 on the last rows (row %d of %d):\n%s", where, r, len(ls), s)
	}
	for i, l := range ls {
		if w := len([]rune(l)); w > a.cols {
			a.t.Errorf("%s: row %d is %d cells wide in a %d-column pane:\n%s", where, i, w, a.cols, s)
		}
	}
}

// narrowAskAndPaletteWidth10History is this run's newest history file.
func narrowAskAndPaletteWidth10History(a *app) string {
	paths, _ := filepath.Glob(filepath.Join(a.home, ".bough", "history", "*.jsonl"))
	var out string
	var at time.Time
	for _, p := range paths {
		if st, err := os.Stat(p); err == nil && !st.ModTime().Before(at) {
			b, _ := os.ReadFile(p)
			out, at = string(b), st.ModTime()
		}
	}
	return out
}

func TestNarrowAskAndPaletteWidth10AskNumber(t *testing.T) {
	t.Parallel()
	narrowAskAndPaletteWidth10Each(t, func(t *testing.T, a *app) {
		a.typeText("go")
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool {
			return strings.Contains(narrowAskAndPaletteWidth10History(a), `"kind":"ask"`)
		}, "the ask to be pending")
		narrowAskAndPaletteWidth10Check(a, "ask pending")
		a.typeText("3")
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(1, 30*time.Second) {
			t.Fatalf("turn never finished after answering 3:\n%s", a.text())
		}
		h := narrowAskAndPaletteWidth10History(a)
		if !strings.Contains(h, "picked "+narrowAskAndPaletteWidth10Third) {
			t.Errorf("answer 3 did not pick option 3; history:\n%s", h)
		}
		narrowAskAndPaletteWidth10Check(a, "ask answered")
	})
}

func TestNarrowAskAndPaletteWidth10ActionPalette(t *testing.T) {
	t.Parallel()
	narrowAskAndPaletteWidth10Each(t, func(t *testing.T, a *app) {
		a.key('x', uv.ModCtrl)
		a.key('p', 0)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "> /") }, "palette to open")
		narrowAskAndPaletteWidth10Check(a, "palette open")
		a.typeText("expand")
		narrowAskAndPaletteWidth10Check(a, "palette filtered")
		a.key(uv.KeyEscape, 0)
		narrowAskAndPaletteWidth10Check(a, "palette closed")
	})
}

func TestNarrowAskAndPaletteWidth10ModelPicker(t *testing.T) {
	t.Parallel()
	narrowAskAndPaletteWidth10Each(t, func(t *testing.T, a *app) {
		a.typeText("/model")
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "replay") }, "model picker to open")
		narrowAskAndPaletteWidth10Screen(a, "picker open", false)
		a.key(uv.KeyDown, 0)
		narrowAskAndPaletteWidth10Screen(a, "picker moved", false)
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(s string) bool { return composerRow(strings.Split(s, "\n")) >= 0 }, "picker to close")
		narrowAskAndPaletteWidth10Check(a, "picker closed")
		if a.doneCount() != 0 {
			t.Errorf("picker ran a turn:\n%s", a.text())
		}
	})
}
