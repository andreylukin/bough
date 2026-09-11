package vtreal

// unicode-and-rtl-in-ask-options: tools.ask with options that carry an
// emoji ZWJ sequence, stacked combining marks, RTL Arabic, CJK, and one
// option longer than the pane. The pending card must keep every row
// inside the pane on the x/vt cell grid with the option numbers in one
// column; clicking option 3's row must hand codemode exactly option 3's
// bytes (the recorded result); and at 40 columns under tmux the options
// wrap whole rather than being cut mid-grapheme.

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

// unicodeAndRTLInAskOptionsOpts are the options; each fragment in
// unicodeAndRTLInAskOptionsKeep must survive rendering whole.
var unicodeAndRTLInAskOptionsOpts = []string{
	"family 👨‍👩‍👧‍👦 and 🏳️‍🌈 flag",
	"café naïve Z͓͑͒algo",
	"مرحبا بالعالم عربي",
	"漢字かなカナ한국어 中文",
	"long " + strings.Repeat("wide 全角文字 words ", 8) + "LONGEND",
}

var unicodeAndRTLInAskOptionsKeep = []string{
	"👨‍👩‍👧‍👦", "café", "naïve", "مرحبا", "漢字かなカナ한국어", "LONGEND",
}

func unicodeAndRTLInAskOptionsTape(t *testing.T) string {
	t.Helper()
	args := []string{`"Pick one"`}
	for _, o := range unicodeAndRTLInAskOptionsOpts {
		b, _ := json.Marshal(o)
		args = append(args, string(b))
	}
	code := "const c = tools.ask(" + strings.Join(args, ", ") + ");\nconsole.log(\"you picked [\" + c + \"]\");\n"
	var sb strings.Builder
	for i, e := range []struct {
		kind string
		data map[string]any
	}{
		{"meta", map[string]any{"cwd": "/tmp/demo"}},
		{"input", map[string]any{"text": "pick one"}},
		{"assistant", map[string]any{"text": "```js\n" + code + "```"}},
		{"assistant", map[string]any{"text": "```stop\nPick locked in.\n```"}},
		{"done", map[string]any{}},
	} {
		b, _ := json.Marshal(map[string]any{"seq": i + 1, "at": "2026-09-11T10:00:00Z", "kind": e.kind, "data": e.data})
		sb.Write(append(b, '\n'))
	}
	p := filepath.Join(t.TempDir(), "unicode_rtl_ask.jsonl")
	if err := os.WriteFile(p, []byte(sb.String()), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// unicodeAndRTLInAskOptionsStart boots at 100x40 and waits for the card.
func unicodeAndRTLInAskOptionsStart(t *testing.T) *app {
	t.Helper()
	a := startCfg(t, 100, 40, askConfig(unicodeAndRTLInAskOptionsTape(t)))
	a.typeText("pick one")
	a.key(uv.KeyEnter, 0)
	a.waitFor("LONGEND")
	return a
}

// unicodeAndRTLInAskOptionsNumCols maps option number -> cell column of
// its "N." marker on the x/vt grid, and the row it sits on.
func unicodeAndRTLInAskOptionsNumCols(a *app) (cols, rows map[int]int) {
	cols, rows = map[int]int{}, map[int]int{}
	snap := a.term.Snapshot()
	for y, row := range snap.Cells {
		for x := 0; x+2 < len(row); x++ {
			c := row[x].Content
			if len(c) == 1 && c[0] >= '1' && c[0] <= '5' && row[x+1].Content == "." && row[x+2].Content == " " &&
				(x == 0 || row[x-1].Content == "" || row[x-1].Content == " ") {
				n := int(c[0] - '0')
				if _, ok := cols[n]; !ok {
					cols[n], rows[n] = x, y
				}
				break
			}
		}
	}
	return cols, rows
}

func TestUnicodeAndRTLInAskOptionsGrid(t *testing.T) {
	t.Parallel()
	a := unicodeAndRTLInAskOptionsStart(t)
	s := a.settled()
	snap := a.term.Snapshot()
	for y, row := range snap.Cells {
		w := 0
		var sb strings.Builder
		for _, c := range row {
			w += c.Width
			if c.Width > 0 {
				sb.WriteString(c.Content)
			}
		}
		if w > a.cols {
			t.Errorf("row %d spans %d cells in a %d-column pane:\n%s", y, w, a.cols, s)
		}
		if sw := ansi.StringWidth(strings.TrimRight(sb.String(), " ")); sw > a.cols {
			t.Errorf("row %d measures %d cells in a %d-column pane:\n%s", y, sw, a.cols, s)
		}
	}
	for _, k := range unicodeAndRTLInAskOptionsKeep {
		if !strings.Contains(strings.ReplaceAll(s, "\n", ""), k) {
			t.Errorf("option fragment %q not rendered whole:\n%s", k, s)
		}
	}
	cols, _ := unicodeAndRTLInAskOptionsNumCols(a)
	for n := 1; n <= 5; n++ {
		if _, ok := cols[n]; !ok {
			t.Fatalf("option number %d. not on screen:\n%s", n, s)
		}
		if cols[n] != cols[1] {
			t.Errorf("option %d. at column %d, option 1. at column %d (numbers misaligned):\n%s", n, cols[n], cols[1], s)
		}
	}
}

func TestUnicodeAndRTLInAskOptionsClickThird(t *testing.T) {
	t.Parallel()
	a := unicodeAndRTLInAskOptionsStart(t)
	s := a.settled()
	_, rows := unicodeAndRTLInAskOptionsNumCols(a)
	row, ok := rows[3]
	if !ok {
		t.Fatalf("no row for option 3:\n%s", s)
	}
	a.click(5, row)
	if !a.waitDone(1, 30*time.Second) {
		t.Fatalf("turn never finished after clicking option 3:\n%s", a.text())
	}
	res := askDuringSessionPickerEntries(a, "result")
	want := "you picked [" + unicodeAndRTLInAskOptionsOpts[2] + "]\n"
	if len(res) != 1 || res[0] != want {
		t.Errorf("codemode got %q, want exactly %q", res, want)
	}
}

func TestUnicodeAndRTLInAskOptionsNarrowResize(t *testing.T) {
	t.Parallel()
	tm, home := resizeTmuxStart(t, 100, 40, askConfig(unicodeAndRTLInAskOptionsTape(t)))
	resizeTmuxSend(tm, "pick one")
	tm.waitFor("LONGEND")
	for _, cols := range []int{40, 100} {
		tm.resize(cols, 40)
		where := fmt.Sprintf("ask pending @ %d cols", cols)
		resizeTmuxCheck(t, tm, where, cols)
		s := resizeTmuxSettled(tm, cols)
		flat := strings.ReplaceAll(s, "\n", "")
		for n := 1; n <= 5; n++ {
			if !strings.Contains(s, fmt.Sprintf("%d.", n)) {
				t.Errorf("%s: option %d. missing:\n%s", where, n, s)
			}
		}
		for _, k := range []string{"café", "naïve", "مرحبا", "漢字かなカナ한국어", "LONGEND"} {
			if !strings.Contains(flat, k) {
				t.Errorf("%s: fragment %q cut by the wrap:\n%s", where, k, s)
			}
		}
		for i, l := range strings.Split(s, "\n") {
			if w := ansi.StringWidth(l); w > cols {
				t.Errorf("%s: row %d measures %d cells in a %d-column pane:\n%s", where, i, w, cols, s)
			}
		}
		if t.Failed() {
			return
		}
	}
	resizeTmuxSend(tm, "4")
	resizeTmuxWaitDone(t, tm, home, 1)
	tm.waitFor("Pick locked in.")
}
