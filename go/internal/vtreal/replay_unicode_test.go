package vtreal

// Width correctness for text a terminal cannot measure by counting
// runes: wide CJK, emoji (single and ZWJ sequences), combining marks,
// zero-width joiners and RTL — in the composer draft, in user lines,
// in assistant text and in block results.
//
// The invariants are read off Snapshot.Cells rather than the rendered
// string: a row that overflows the pane, or a wide character cut in
// half at the right edge, both look fine in plain text.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"

	"github.com/andreylukin/bough/plugins/replay"
)

// unicodeTape is the fixture path and the inputs recorded in it.
func unicodeTape(t *testing.T) (string, []string) {
	t.Helper()
	path, err := filepath.Abs("testdata/replay/unicode.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	tp, err := replay.Load(path)
	if err != nil {
		t.Fatal(err)
	}
	return path, tp.Inputs
}

// unicodeRow renders one row of cells the way the screen shows it:
// continuation cells (Width 0) contribute nothing, untouched cells a
// blank.
func unicodeRow(row []uv.Cell) string {
	var sb strings.Builder
	for _, c := range row {
		switch {
		case c.Width == 0:
		case c.Content == "":
			sb.WriteByte(' ')
		default:
			sb.WriteString(c.Content)
		}
	}
	return sb.String()
}

// unicodeCheckWidths asserts the cell-grid invariants on a settled
// screen: nothing wider than the pane, no wide cell split by the right
// edge, and every wide cell followed by its continuation.
func unicodeCheckWidths(a *app, where string) {
	a.t.Helper()
	snap := a.term.Snapshot()
	for y, row := range snap.Cells {
		for x, c := range row {
			if c.Width > 2 {
				a.t.Errorf("%s: cell (%d,%d) %q claims width %d:\n%s", where, x, y, c.Content, c.Width, a.text())
			}
			if c.Width != 2 {
				continue
			}
			if x == snap.Cols-1 {
				a.t.Errorf("%s: wide cell %q split by the right edge at (%d,%d) in a %d-column pane:\n%s",
					where, c.Content, x, y, snap.Cols, a.text())
				continue
			}
			if n := row[x+1]; n.Width != 0 || n.Content != "" {
				a.t.Errorf("%s: wide cell %q at (%d,%d) is not followed by a continuation cell (next %q width %d):\n%s",
					where, c.Content, x, y, n.Content, n.Width, a.text())
			}
		}
		if w := ansi.StringWidth(strings.TrimRight(unicodeRow(row), " ")); w > snap.Cols {
			a.t.Errorf("%s: row %d is %d cells wide in a %d-column pane: %q\n%s",
				where, y, w, snap.Cols, unicodeRow(row), a.text())
		}
	}
}

// TestUnicodeTranscript replays a tape whose inputs, assistant text and
// block results are full of wide, combining and RTL text, and checks the
// widths after every turn.
func TestUnicodeTranscript(t *testing.T) {
	t.Parallel()
	tape, inputs := unicodeTape(t)
	for _, sz := range [][2]int{{100, 30}, {41, 20}} {
		t.Run(fmt.Sprintf("%dx%d", sz[0], sz[1]), func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, sz[0], sz[1], replayConfig(tape))
			// a.check()'s status-bar rule only holds in a pane wide enough
			// to keep the "? keys" hint; the width rules hold everywhere.
			wide := sz[0] >= 80
			if wide {
				a.check("boot")
			}
			unicodeCheckWidths(a, "boot")
			turns := 0
			for i, in := range inputs {
				where := fmt.Sprintf("turn %d", i+1)
				a.term.Paste(in) // SendText would re-encode the graphemes as keys
				a.key(uv.KeyEnter, 0)
				turns++
				if !a.waitDone(turns, 60*time.Second) {
					t.Fatalf("%s: turn never finished:\n%s", where, a.text())
				}
				if wide {
					a.check(where)
				}
				unicodeCheckWidths(a, where)
				if composerRow(a.lines()) < 0 {
					t.Fatalf("%s: composer lost:\n%s", where, a.text())
				}
				if t.Failed() {
					return
				}
			}
		})
	}
}

// TestUnicodeComposerCursor types wide, combining, ZWJ and RTL drafts
// into the composer and asserts the virtual cursor sits in the cell
// right after the last cell of the draft.
func TestUnicodeComposerCursor(t *testing.T) {
	t.Parallel()
	drafts := map[string]string{
		"cjk":        "日本語のテキスト",
		"emoji":      "hi 🚀 there",
		"zwj_family": "family 👨‍👩‍👧‍👦",
		"combining":  "café naïve é",
		"rtl":        "مرحبا بالعالم",
		"mixed":      "日本語 🚀 café مرحبا ok",
	}
	for name, draft := range drafts {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			a := start(t, 80, 24)
			a.term.Paste(draft)
			a.waitUntil(func(s string) bool { return strings.Contains(s, draft) }, "draft "+name+" in the composer")
			a.settled()
			unicodeCheckWidths(a, "draft "+name)

			snap := a.term.Snapshot()
			row := composerRow(a.lines())
			if row < 0 {
				t.Fatalf("no composer row:\n%s", a.text())
			}
			cursor := -1
			for x, c := range snap.Cells[row] {
				if c.Style.Attrs&uv.AttrReverse != 0 {
					cursor = x
					break
				}
			}
			if cursor < 0 {
				t.Fatalf("no reverse-video cursor cell in the composer row:\n%s", a.text())
			}
			// Everything before the cursor is the prompt plus the draft,
			// cell for cell: the cursor is after the LAST cell, not after
			// the last rune and not inside a wide character.
			before := unicodeRow(snap.Cells[row][:cursor])
			if want := "> " + draft; before != want {
				t.Fatalf("cursor at column %d, cells before it are %q, want %q:\n%s", cursor, before, want, a.text())
			}
			// ...and the cursor is past the whole character: the cell before
			// it is never the first half of a wide one.
			if c := snap.Cells[row][cursor-1]; c.Width == 2 {
				t.Fatalf("cursor at column %d splits the wide character %q before it:\n%s", cursor, c.Content, a.text())
			}
		})
	}
}

// TestUnicodeWideCharAtRightEdge pushes a run of full-width characters
// with no break opportunity through a narrow pane: the wrap must never
// leave half a character in the last column.
func TestUnicodeWideCharAtRightEdge(t *testing.T) {
	t.Parallel()
	// An odd pane width: a run of width-2 characters cannot fill it, so
	// a naive wrap puts half a character in the last column.
	a := start(t, 41, 20)
	a.term.Paste(strings.Repeat("日本語", 30))
	a.key(uv.KeyEnter, 0)
	a.waitFor("echo:")
	a.settled()
	unicodeCheckWidths(a, "wide run at 41 columns")
	if composerRow(a.lines()) < 0 {
		t.Fatalf("composer lost under a wide-character run:\n%s", a.text())
	}
}
