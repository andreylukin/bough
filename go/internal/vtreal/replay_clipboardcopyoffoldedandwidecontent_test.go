package vtreal

// Copy of folded and wide content on a real PTY, observed through a
// fake clipboard: PATH holds only a pbcopy (and xclip, for Linux) that
// writes its stdin to a file, so what the copy put on the clipboard is
// read back byte for byte. The clipboard must receive the original
// text: no fold glyphs, no box drawing, no ANSI, and a table rewrapped
// at 40 columns copied (by the chord) as the markdown the model wrote.

import (
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// clipboardCopyOfFoldedAndWideContentTable is a reply whose table is
// far wider than a 40-column pane.
const clipboardCopyOfFoldedAndWideContentTable = `Here is the matrix.

| component | owner | status | latency budget | notes |
| --- | --- | --- | --- | --- |
| ingest pipeline | platform team | green | 250 milliseconds | backfill still running nightly |
| query planner | data team | amber | 900 milliseconds | rewrite landed behind a flag |

That is everything.`

// clipboardCopyOfFoldedAndWideContentGlyphs are the characters the TUI
// draws around text and must never reach the clipboard.
var clipboardCopyOfFoldedAndWideContentGlyphs = regexp.MustCompile(`[\x{2500}-\x{257F}▸▾▶▼…]|\x1b`)

// clipboardCopyOfFoldedAndWideContentStart boots yml with the fake
// clipboard alone on PATH and returns the app and the capture file.
func clipboardCopyOfFoldedAndWideContentStart(t *testing.T, cols, rows int, yml string) (*app, string) {
	t.Helper()
	home := t.TempDir()
	cfg := filepath.Join(home, "bough.yml")
	if err := os.WriteFile(cfg, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	bindir := t.TempDir()
	clip := filepath.Join(t.TempDir(), "clip")
	script := "#!/bin/sh\n/bin/cat > " + clip + ".tmp && /bin/mv " + clip + ".tmp " + clip + "\n"
	for _, name := range []string{"pbcopy", "xclip"} {
		if err := os.WriteFile(filepath.Join(bindir, name), []byte(script), 0o755); err != nil {
			t.Fatal(err)
		}
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
		"PATH="+bindir, "TMUX=", "WAYLAND_DISPLAY=",
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
	a.waitFor("say something")
	return a, clip
}

// clipboardCopyOfFoldedAndWideContentTurn submits input and waits for
// the turn to finish.
func clipboardCopyOfFoldedAndWideContentTurn(a *app, input string) {
	a.t.Helper()
	a.typeText(input)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(1, 60*time.Second) {
		a.t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.check("after the turn")
}

// clipboardCopyOfFoldedAndWideContentRead waits for the fake clipboard
// to receive a copy and returns it, removing the file for the next one.
func clipboardCopyOfFoldedAndWideContentRead(a *app, clip string) string {
	a.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if b, err := os.ReadFile(clip); err == nil {
			_ = os.Remove(clip)
			return string(b)
		}
		time.Sleep(20 * time.Millisecond)
	}
	a.t.Fatalf("the fake clipboard never received a copy:\n%s", a.text())
	return ""
}

// clipboardCopyOfFoldedAndWideContentChord presses ctrl+x y.
func clipboardCopyOfFoldedAndWideContentChord(a *app) {
	a.key('x', uv.ModCtrl)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "ctrl+x …") },
		"the pending leader flash")
	a.key('y', 0)
}

// clipboardCopyOfFoldedAndWideContentClean fails when got carries a
// fold glyph, box drawing or an escape sequence.
func clipboardCopyOfFoldedAndWideContentClean(t *testing.T, got string) {
	t.Helper()
	if m := clipboardCopyOfFoldedAndWideContentGlyphs.FindString(got); m != "" {
		t.Errorf("clipboard carries TUI glyph/escape %q:\n%q", m, got)
	}
}

// clipboardCopyOfFoldedAndWideContentKnown skips a subtest pinned to a
// known product bug unless BOUGH_KNOWN_CLIPBOARD_COPY_OF_FOLDED_AND_WIDE_CONTENT is set.
func clipboardCopyOfFoldedAndWideContentKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_CLIPBOARD_COPY_OF_FOLDED_AND_WIDE_CONTENT") == "" {
		t.Skip("known bug: " + bug + "; set BOUGH_KNOWN_CLIPBOARD_COPY_OF_FOLDED_AND_WIDE_CONTENT=1 to run")
	}
}

// clipboardCopyOfFoldedAndWideContentFolded boots the basic tape, runs
// its first turn and returns the row of the collapsed "▸ Ran" header.
func clipboardCopyOfFoldedAndWideContentFolded(t *testing.T) (*app, string, int) {
	t.Helper()
	tape, err := filepath.Abs("testdata/replay/basic.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	a, clip := clipboardCopyOfFoldedAndWideContentStart(t, 100, 30, replayConfig(tape))
	clipboardCopyOfFoldedAndWideContentTurn(a, "list the files here")
	row := copyRow(a, "▸ Ran")
	if row < 0 {
		t.Fatalf("no folded Ran block:\n%s", a.text())
	}
	return a, clip, row
}

func TestClipboardCopyOfFoldedAndWideContent(t *testing.T) {
	t.Parallel()

	// Focus the folded block (open it with a click, close it again) and
	// copy it with the chord: its raw code lands on the clipboard.
	t.Run("chord_folded_block", func(t *testing.T) {
		t.Parallel()
		a, clip, row := clipboardCopyOfFoldedAndWideContentFolded(t)
		a.click(2, row)
		a.waitUntil(func(s string) bool { return strings.Contains(s, "▾ Ran") }, "the block to open")
		a.click(2, copyRow(a, "▾ Ran"))
		a.waitUntil(func(s string) bool { return strings.Contains(s, "▸ Ran") }, "the block to fold again")
		clipboardCopyOfFoldedAndWideContentChord(a)
		got := clipboardCopyOfFoldedAndWideContentRead(a, clip)
		if !strings.Contains(got, `tools.bash("ls -la")`) {
			t.Errorf("clipboard lacks the folded block's code:\n%q", got)
		}
		clipboardCopyOfFoldedAndWideContentClean(t, got)
	})

	// A drag across the folded header copies what is shown, minus the
	// fold glyph.
	t.Run("drag_folded_header", func(t *testing.T) {
		t.Parallel()
		clipboardCopyOfFoldedAndWideContentKnown(t, "a drag over a folded header copies the ▸ fold glyph "+
			"(plugins/ui/select.go selectedText copies rendered rows)")
		a, clip, row := clipboardCopyOfFoldedAndWideContentFolded(t)
		copyDrag(a, 0, row, a.cols-1, row)
		got := clipboardCopyOfFoldedAndWideContentRead(a, clip)
		if !strings.Contains(got, "Ran") {
			t.Errorf("clipboard lacks the header text:\n%q", got)
		}
		clipboardCopyOfFoldedAndWideContentClean(t, got)
	})

	// The chord copies the reply holding a table rewrapped at 40 cols
	// as the markdown the model wrote, byte for byte.
	t.Run("chord_wide_table_40", func(t *testing.T) {
		t.Parallel()
		tape := markdownTape(t, "show the matrix", clipboardCopyOfFoldedAndWideContentTable)
		a, clip := clipboardCopyOfFoldedAndWideContentStart(t, 40, markdownRows, markdownConfig(tape, 0))
		clipboardCopyOfFoldedAndWideContentTurn(a, "show the matrix")
		clipboardCopyOfFoldedAndWideContentChord(a)
		got := clipboardCopyOfFoldedAndWideContentRead(a, clip)
		if got != clipboardCopyOfFoldedAndWideContentTable {
			t.Errorf("clipboard is not the original reply:\ngot  %q\nwant %q", got, clipboardCopyOfFoldedAndWideContentTable)
		}
		clipboardCopyOfFoldedAndWideContentClean(t, got)
	})

	// A drag across the rewrapped table copies its cells without the
	// table's box drawing.
	t.Run("drag_wide_table_40", func(t *testing.T) {
		t.Parallel()
		clipboardCopyOfFoldedAndWideContentKnown(t, "a drag over a table rewrapped at 40 cols copies "+
			"│ ┼ ─ borders, … truncations and cell text split across rows "+
			"(plugins/ui/select.go selectedText copies rendered rows)")
		tape := markdownTape(t, "show the matrix", clipboardCopyOfFoldedAndWideContentTable)
		a, clip := clipboardCopyOfFoldedAndWideContentStart(t, 40, markdownRows, markdownConfig(tape, 0))
		clipboardCopyOfFoldedAndWideContentTurn(a, "show the matrix")
		r0, r1 := copyRow(a, "Here is the matrix"), copyRow(a, "That is everything")
		if r0 < 0 || r1 < 0 {
			t.Fatalf("reply not on screen:\n%s", a.text())
		}
		copyDrag(a, 0, r0, a.cols-1, r1)
		got := clipboardCopyOfFoldedAndWideContentRead(a, clip)
		for _, w := range []string{"Here is the matrix", "ingest", "That is everything"} {
			if !strings.Contains(got, w) {
				t.Errorf("clipboard lacks %q:\n%q", w, got)
			}
		}
		clipboardCopyOfFoldedAndWideContentClean(t, got)
	})
}
