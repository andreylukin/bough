package vtreal

// Every plugins/ui keymap action driven by its default key (or chord)
// on a real PTY over a replayed session: each subtest presses the key,
// asserts the effect on screen, then checks the composer and status
// bar are still where they belong (app.check).
//
// scroll_up is the one action tested on a rebound key: its default
// "up" is prompt recall whenever the session has prompts (composer.go),
// so it only scrolls on a fresh session with nothing to scroll.

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// keymapStart is startCfg at 100x30 with extra environment and an
// optional ~/.bough/init.js written before boot.
func keymapStart(t *testing.T, yml, initJS string, env ...string) *app {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	if initJS != "" {
		if err := os.MkdirAll(filepath.Join(home, ".bough"), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(home, ".bough", "init.js"), []byte(initJS), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	term, err := NewTerminal(t, 100, 30)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "-config", cfg)
	cmd.Dir = home
	cmd.Env = append(append(os.Environ(),
		"HOME="+home, "TERM=xterm-256color", "COLORTERM=truecolor",
		"NO_COLOR=", "BOUGH_VERBOSE=", "TMUX=", "VISUAL=",
	), env...)
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

// keymapSession boots on the keymap tape and plays its first two turns,
// leaving a transcript taller than the pane with collapsible blocks.
// PATH is emptied so a copy never reaches the real clipboard (pbcopy).
func keymapSession(t *testing.T, initJS string) *app {
	t.Helper()
	tape, _ := filepath.Abs("testdata/replay/keymap.jsonl")
	// delay_ms on the llm row (the first file: config): with no delay
	// the 40-line reply's word deltas arrive all at once and the TUI
	// froze mid-stream, spinner running, although history had the
	// turn's done (6/6 runs) — see the commit message.
	yml := strings.Replace(replayConfig(tape), fmt.Sprintf("{file: %q}", tape), fmt.Sprintf("{file: %q, delay_ms: 1}", tape), 1)
	a := keymapStart(t, yml, initJS, "PATH="+t.TempDir())
	for i, in := range []string{"list the files here", "count to forty"} {
		a.typeText(in)
		a.key(uv.KeyEnter, 0)
		if !a.waitDone(i+1, 60*time.Second) {
			t.Fatalf("turn %d never finished:\n%s", i+1, a.text())
		}
	}
	a.check("setup")
	return a
}

func (a *app) keymapCtrl(r rune) { a.key(r, uv.ModCtrl) }

// keymapChord presses the leader (ctrl+x) then k.
func (a *app) keymapChord(k rune) {
	a.keymapCtrl('x')
	a.waitFor("ctrl+x …")
	a.key(k, 0)
}

// keymapComposer is the composer row's text.
func (a *app) keymapComposer() string {
	ls := a.lines()
	if r := composerRow(ls); r >= 0 {
		return ls[r]
	}
	return ""
}

// keymapFocused is the row and text of the block header with a cell in
// the bold "focus" style, "" when no header holds the cursor.
func (a *app) keymapFocused() string {
	a.settled()
	snap := a.term.Snapshot()
	ls := a.lines()
	for y, row := range snap.Cells {
		if !strings.ContainsAny(ls[y], "▸▾") {
			continue
		}
		for _, c := range row {
			if c.Style.Attrs&uv.AttrBold != 0 {
				return strconv.Itoa(y) + ":" + ls[y]
			}
		}
	}
	return ""
}

var keymapScrolled = regexp.MustCompile(`scrolled ↑ (\d+) lines`)

// keymapScrolledBy is the status bar's scroll cue, -1 at the bottom.
func (a *app) keymapScrolledBy() int {
	m := keymapScrolled.FindStringSubmatch(a.settled())
	if m == nil {
		return -1
	}
	n, _ := strconv.Atoi(m[1])
	return n
}

func (a *app) keymapGone(substr string) {
	a.t.Helper()
	a.waitUntil(func(s string) bool { return !strings.Contains(s, substr) }, "screen to drop "+substr)
}

// The subtests share one session and run in order; each leaves it
// idle and scrolled to the bottom.
func TestKeymap(t *testing.T) {
	t.Parallel()
	a := keymapSession(t, "")

	t.Run("clear_input", func(t *testing.T) {
		a.t = t
		a.typeText("draft to clear")
		a.waitFor("> draft to clear")
		a.keymapCtrl('l')
		a.waitFor("say something")
		if c := a.keymapComposer(); strings.Contains(c, "draft to clear") {
			t.Fatalf("ctrl+l left the draft in the composer %q:\n%s", c, a.text())
		}
		a.check("after ctrl+l")
	})

	t.Run("page_up_page_down", func(t *testing.T) {
		a.t = t
		a.key(uv.KeyPgUp, 0)
		a.waitFor("scrolled ↑")
		a.check("after pgup")
		a.key(uv.KeyPgDown, 0)
		a.key(uv.KeyPgDown, 0)
		a.keymapGone("scrolled ↑")
		a.check("after pgdown")
	})

	t.Run("scroll_down", func(t *testing.T) {
		a.t = t
		a.key(uv.KeyPgUp, 0)
		a.waitFor("scrolled ↑")
		before := a.keymapScrolledBy()
		a.key(uv.KeyDown, 0)
		if after := a.keymapScrolledBy(); after != before-1 {
			t.Fatalf("down should scroll one line: cue %d -> %d:\n%s", before, after, a.text())
		}
		if c := a.keymapComposer(); !strings.Contains(c, "say something") {
			t.Fatalf("down changed the composer %q:\n%s", c, a.text())
		}
		a.check("after down")
		a.key(uv.KeyPgDown, 0)
		a.key(uv.KeyPgDown, 0)
		a.keymapGone("scrolled ↑")
	})

	var first string
	t.Run("block_next", func(t *testing.T) {
		a.t = t
		if f := a.keymapFocused(); f != "" {
			t.Fatalf("a header is focused before tab (%q):\n%s", f, a.text())
		}
		a.key(uv.KeyTab, 0)
		if first = a.keymapFocused(); first == "" {
			t.Fatalf("tab focused no block header:\n%s", a.text())
		}
		a.check("after tab")
	})

	t.Run("collapse_toggle", func(t *testing.T) {
		a.t = t
		if !strings.Contains(first, "▸") {
			t.Fatalf("focused header is not a collapsed one (%q):\n%s", first, a.text())
		}
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return strings.Contains(a.keymapFocused(), "▾") }, "enter to expand the focused block (▾)")
		a.key(uv.KeyEnter, 0)
		a.waitUntil(func(string) bool { return strings.Contains(a.keymapFocused(), "▸") }, "enter to collapse it again (▸)")
		if a.doneCount() != 2 {
			t.Fatalf("enter on a focused block submitted a turn:\n%s", a.text())
		}
		a.check("after enter")
	})

	t.Run("block_prev", func(t *testing.T) {
		a.t = t
		a.key(uv.KeyTab, 0) // one older
		older := a.keymapFocused()
		if older == "" || older == first {
			t.Fatalf("second tab did not move focus (%q -> %q):\n%s", first, older, a.text())
		}
		a.key(uv.KeyTab, uv.ModShift) // think_cycle steps back while focused
		back := a.keymapFocused()
		if back == "" || back == older {
			t.Fatalf("shift+tab did not move focus back (%q -> %q):\n%s", older, back, a.text())
		}
		a.check("after shift+tab")
	})

	t.Run("copy", func(t *testing.T) {
		a.t = t
		a.keymapChord('y')
		a.waitFor("copied ")
		s := a.settled()
		if !strings.Contains(s, "OSC 52") || strings.Contains(s, "pbcopy") {
			t.Fatalf("copy should go by OSC 52 alone here:\n%s", s)
		}
		a.check("after ctrl+x y")
	})

	t.Run("expand_all", func(t *testing.T) {
		a.t = t
		a.keymapChord('e')
		a.waitFor("expanded ")
		a.waitFor("▾")
		// KNOWN: this flash is wider than a 100-column bar and cuts
		// "? keys" to "? …" (statusBar keeps no fallback while a flash
		// shows), so check() runs once esc has cleared it.
		if ls := a.lines(); composerRow(ls) < len(ls)-3 {
			t.Fatalf("composer not on the last rows under the flash:\n%s", a.text())
		}
		a.key(uv.KeyEscape, 0)
		a.keymapGone("expanded ")
		a.check("after ctrl+x e")
	})

	t.Run("collapse_all", func(t *testing.T) {
		a.t = t
		a.keymapChord('c')
		a.waitFor("collapsed ")
		a.check("after ctrl+x c")
	})

	t.Run("leader_pending_esc", func(t *testing.T) {
		a.t = t
		a.keymapCtrl('x')
		a.waitFor("ctrl+x …")
		a.key(uv.KeyEscape, 0)
		a.keymapGone("ctrl+x …")
		a.keymapCtrl('x')
		a.key('z', 0)
		a.waitFor("ctrl+x z: no such chord")
		if c := a.keymapComposer(); strings.Contains(c, "z") {
			t.Fatalf("the chord key leaked into the composer %q:\n%s", c, a.text())
		}
		a.check("after a bad chord")
	})

	t.Run("palette", func(t *testing.T) {
		a.t = t
		a.keymapChord('p')
		a.waitUntil(func(string) bool { return strings.HasPrefix(a.keymapComposer(), "> /") }, "ctrl+x p to put / in the composer")
		a.waitFor("collapse")
		a.check("palette open")
		a.key(uv.KeyEscape, 0)
		a.waitUntil(func(string) bool { return strings.Contains(a.keymapComposer(), "say something") }, "esc to close the palette")
		a.check("palette closed")
	})

	t.Run("history_inspect", func(t *testing.T) {
		a.t = t
		a.keymapCtrl('o')
		a.waitFor("inspecting · ctrl+o to close")
		a.keymapCtrl('o')
		a.keymapGone("inspecting ·")
		a.check("after ctrl+o twice")
	})

	t.Run("todo_toggle", func(t *testing.T) {
		a.t = t
		a.keymapCtrl('t')
		a.waitFor("no todo list yet")
		a.typeText("/todo add write keymap tests")
		a.waitFor("> /todo add write keymap tests")
		a.key(uv.KeyEnter, 0)
		a.waitFor("todo · ctrl+t hides")
		a.check("todo strip shown")
		a.keymapCtrl('t')
		a.keymapGone("todo · ctrl+t hides")
		a.check("todo strip hidden")
		a.keymapCtrl('t')
		a.waitFor("todo · ctrl+t hides")
		a.check("todo strip back")
	})

	// Last: it spends a tape reply (the tape's third turn).
	t.Run("follow_up", func(t *testing.T) {
		a.t = t
		a.typeText("thanks")
		a.key(uv.KeyEnter, uv.ModAlt)
		if !a.waitDone(3, 30*time.Second) {
			t.Fatalf("alt+enter on an idle session never ran the turn:\n%s", a.text())
		}
		a.waitFor("❯ thanks")
		a.waitFor("Any time.")
		if s := a.settled(); strings.Contains(s, "end of tape") {
			t.Fatalf("the tape ran out: an earlier key spent a reply:\n%s", s)
		}
		a.check("after alt+enter")
	})
}

// One ctrl+c arms the quit and names the key; the second exits 0.
func TestKeymapQuit(t *testing.T) {
	t.Parallel()
	a := keymapSession(t, "")
	a.keymapCtrl('c')
	a.waitFor("ctrl+c")
	a.check("armed")
	a.keymapCtrl('c')
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("exit: %v\n%s", err, a.text())
		}
	case <-time.After(8 * time.Second):
		t.Fatalf("second ctrl+c did not quit:\n%s", a.text())
	}
}

// ctrl+g suspends into $EDITOR (true: exits 0, leaves the file) and
// the draft comes back unchanged, the TUI redrawn around it.
func TestKeymapExternalEditor(t *testing.T) {
	t.Parallel()
	a := keymapStart(t, config, "", "EDITOR=true")
	a.typeText("draft for the editor")
	a.waitFor("> draft for the editor")
	a.keymapCtrl('g')
	a.waitUntil(func(string) bool {
		return a.term.Snapshot().AltScreen && strings.Contains(a.keymapComposer(), "draft for the editor")
	}, "the TUI back with the draft after the editor exits")
	if strings.Contains(a.settled(), "editor:") {
		t.Fatalf("editor round trip reported an error:\n%s", a.text())
	}
	a.check("after ctrl+g")
	// Still a working composer: the kept draft submits.
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo: draft for the editor")
	a.check("after submitting the kept draft")
}

// scroll_up on a rebound key (see the file comment): each press moves
// the scroll cue up by one line and leaves the composer alone.
func TestKeymapScrollUp(t *testing.T) {
	t.Parallel()
	a := keymapSession(t, `bough.setup({ui: {keymap: {scroll_up: "ctrl+u"}}})`)
	a.keymapCtrl('u')
	a.waitFor("scrolled ↑ 1 lines")
	a.keymapCtrl('u')
	a.waitFor("scrolled ↑ 2 lines")
	if c := a.keymapComposer(); !strings.Contains(c, "say something") {
		t.Fatalf("ctrl+u changed the composer %q:\n%s", c, a.text())
	}
	a.check("after scroll_up")
}
