package vtreal

// Open (expanded) code and result boxes with hostile content: an
// unbroken 180-dash rule, a 300-character URL, a 200-hex hash, 120 CJK
// characters, tabs, \r progress bars and ANSI SGR from a tool. Each
// box is opened by clicking its header, the way a user does, and then
// the pane is judged on the cell grid: no row wider than the pane, the
// rounded border closed on every row of the box, and the composer
// still there. Sizes 40, 80 and 120 columns, because the box is
// wrapped to the pane and the narrow pane is where the wrap breaks.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
)

// openBoxesCase is one turn of the generated tape: what the user types
// and what the tool "printed" back.
type openBoxesCase struct {
	name   string
	input  string
	cmd    string
	output string
	want   string // a fragment of the output that must reach the open box
}

func openBoxesCases() []openBoxesCase {
	cjk := strings.Repeat("漢字", 60) // 120 CJK runes, 240 cells
	return []openBoxesCase{
		{"rule", "draw a rule", "make rule", strings.Repeat("-", 180) + "\n", "----------"},
		{"url", "show the url", "cat url.txt",
			"https://example.com/" + strings.Repeat("abcdefghij/", 25) + "end\n", "abcdefghij"},
		{"hash", "show the hash", "sha", strings.Repeat("0123456789abcdef", 12) + "12345678\n", "0123456789abcdef"},
		{"cjk", "show the cjk line", "cat cjk.txt", cjk + "\n", "漢字"},
		{"tabs", "show the table", "cat table.tsv",
			"name\tsize\tmode\n\t\t\ta\t1\trw\nbbbb\t22222\trwx\n", "mode"},
		{"cr", "run the progress bar", "download",
			"  0% [          ]\r 50% [=====     ]\r100% [==========]\ndone\n", "done"},
		{"sgr", "run the colored tool", "lint",
			"\x1b[31merror\x1b[0m: \x1b[1mbold\x1b[22m and \x1b[4munderlined\x1b[0m tail\n" +
				"\x1b[32m" + strings.Repeat("=", 150) + "\x1b[0m\n", "underlined"},
	}
}

// openBoxesTape writes a history tape for the cases: one turn each, a
// js block that "ran" the command and the recorded hostile output.
func openBoxesTape(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "open-boxes.jsonl")
	var sb strings.Builder
	seq := 0
	at := time.Date(2026, 9, 10, 10, 0, 0, 0, time.UTC)
	write := func(kind string, data map[string]any) {
		seq++
		b, err := json.Marshal(map[string]any{
			"seq":  seq,
			"at":   at.Add(time.Duration(seq) * time.Second).Format(time.RFC3339),
			"kind": kind,
			"data": data,
		})
		if err != nil {
			t.Fatal(err)
		}
		sb.Write(b)
		sb.WriteByte('\n')
	}
	write("meta", map[string]any{"cwd": "/tmp/demo"})
	for _, c := range openBoxesCases() {
		code := fmt.Sprintf("console.log(tools.bash(%q))\n", c.cmd)
		write("input", map[string]any{"text": c.input})
		write("assistant", map[string]any{"text": "```js\n" + code + "```"})
		write("code", map[string]any{"text": code})
		write("result", map[string]any{"code": code, "text": c.output})
		write("assistant", map[string]any{"text": "```stop\nThat is " + c.name + ".\n```"})
		write("done", map[string]any{"text": ""})
	}
	if err := os.WriteFile(path, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

// openBoxesBorders is the rounded border lipgloss draws around a box.
var openBoxesBorders = map[rune]rune{'╭': '╮', '│': '│', '╰': '╯'}

// openBoxesCheck judges a settled screen: every row fits the pane in
// display cells, every box row closes its border, and the composer and
// status bar are still on the last rows. Every failure prints the
// screen.
func openBoxesCheck(a *app, where string) {
	a.t.Helper()
	a.check(where) // width in runes, composer, status bar, no crash text
	s := a.settled()
	// SGR from a tool must be styling or gone, never text: an escape
	// that reaches the grid as characters is a leak.
	for _, leak := range []string{"\x1b", "[0m", "[31m", "[1m", "[4m"} {
		if strings.Contains(s, leak) {
			a.t.Errorf("%s: escape sequence %q printed as text:\n%s", where, leak, s)
		}
	}
	for i, l := range strings.Split(s, "\n") {
		if w := ansi.StringWidth(l); w > a.cols {
			a.t.Errorf("%s: row %d is %d cells wide in a %d-column pane:\n%s", where, i, w, a.cols, s)
		}
		r := []rune(strings.TrimLeft(l, " "))
		if len(r) == 0 {
			continue
		}
		want, isBox := openBoxesBorders[r[0]]
		if !isBox {
			continue
		}
		if got := r[len(r)-1]; got != want {
			a.t.Errorf("%s: box row %d opens with %q but ends with %q (border not closed):\n%s",
				where, i, string(r[0]), string(got), s)
		}
	}
}

// openBoxesExpand clicks the last header on screen whose row matches
// glyph+tag ("▸ Ran", "▸ result") and waits for it to open.
func openBoxesExpand(a *app, tag string) {
	a.t.Helper()
	s := a.settled()
	row := -1
	for i, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "▸ "+tag) {
			row = i
		}
	}
	if row < 0 {
		a.t.Fatalf("no collapsed %q header on screen:\n%s", tag, s)
	}
	a.click(2, row)
	a.waitUntil(func(s string) bool { return strings.Contains(s, "▾ "+tag) },
		fmt.Sprintf("%q block open after a click on its header", tag))
	// An open box draws a border; a box wrapped wider than the pane
	// loses its top edge to the wrap.
	a.waitUntil(func(s string) bool { return strings.Contains(s, "╭") },
		fmt.Sprintf("a box top border after opening %q", tag))
}

// TestOpenBoxes drives one replayed session per pane width, opening
// the code and result box of every hostile turn.
func TestOpenBoxes(t *testing.T) {
	t.Parallel()
	tape := openBoxesTape(t)
	cases := openBoxesCases()
	for _, cols := range []int{40, 80, 120} {
		t.Run(fmt.Sprintf("%dcols", cols), func(t *testing.T) {
			t.Parallel()
			a := startCfg(t, cols, 30, replayConfig(tape))
			openBoxesCheck(a, "boot")
			for i, c := range cases {
				where := fmt.Sprintf("%s (turn %d)", c.name, i+1)
				a.typeText(c.input)
				a.key(uv.KeyEnter, 0)
				if !a.waitDone(i+1, 60*time.Second) {
					t.Fatalf("%s: turn never finished:\n%s", where, a.text())
				}
				openBoxesCheck(a, where+" collapsed")
				openBoxesExpand(a, "Ran")
				openBoxesCheck(a, where+" code box open")
				openBoxesExpand(a, "result")
				openBoxesCheck(a, where+" result box open")
				if s := a.settled(); !strings.Contains(s, c.want) {
					t.Errorf("%s: open result box does not show %q:\n%s", where, c.want, s)
				}
				if t.Failed() {
					return
				}
			}
		})
	}
}
