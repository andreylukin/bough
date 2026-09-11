package vtreal

// Very wide panes (300x20, 400x60): boxes and dividers span the pane,
// markdown tables use the width, the status bar's right side is whole
// and flush right, and no row is wider than the pane.

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

// widePaneCols are the tape's table columns; its cells are long enough
// that the table is ~250 cells wide when nothing wraps.
var widePaneCols = []string{"alpha_column", "bravo_column", "charlie_column", "delta_column", "echo_column"}

func TestWidePane(t *testing.T) {
	t.Parallel()
	tape, _ := filepath.Abs("testdata/replay/widepane.jsonl")
	for _, sz := range [][2]int{{300, 20}, {400, 60}} {
		t.Run(fmt.Sprintf("%dx%d", sz[0], sz[1]), func(t *testing.T) {
			t.Parallel()
			// Paced: at delay 0 the ~110 word deltas overflow the ui's
			// event buffer, which drops (broadcaster.publish in
			// plugins/ui/ui.go) — the final assistant/done events with
			// them, and the live block never settles.
			cfg := strings.Replace(replayConfig(tape), "config: {file: "+fmt.Sprintf("%q", tape)+"}",
				"config: {file: "+fmt.Sprintf("%q", tape)+", delay_ms: 5}", 1)
			a := startCfg(t, sz[0], sz[1], cfg)
			a.typeText("show me a wide table")
			a.key(uv.KeyEnter, 0)
			if !a.waitDone(1, 30*time.Second) {
				t.Fatalf("turn never finished:\n%s", a.text())
			}
			// "done" lands in history before the reply finishes streaming.
			a.waitUntil(func(s string) bool { return !strings.Contains(s, "▌") }, "reply to finish streaming")
			a.check("after turn")
			t.Run("TestWidePaneCellsFitCols", func(t *testing.T) { widePaneCellsFit(t, a) })
			t.Run("TestWidePaneStatusBarRight", func(t *testing.T) { widePaneStatusBar(t, a) })
			t.Run("TestWidePaneDividerSpans", func(t *testing.T) { widePaneDivider(t, a) })
			t.Run("TestWidePaneTableUsesWidth", func(t *testing.T) { widePaneTable(t, a) })
			t.Run("TestWidePaneBoxSpans", func(t *testing.T) { widePaneBox(t, a) })
		})
	}
}

// The raw cell grid, not the trimmed text: no row holds more than cols cells.
func widePaneCellsFit(t *testing.T, a *app) {
	snap := a.term.Snapshot()
	if len(snap.Cells) != a.rows {
		t.Errorf("%d rows in a %d-row pane:\n%s", len(snap.Cells), a.rows, a.text())
	}
	for y, row := range snap.Cells {
		if len(row) > a.cols {
			t.Errorf("row %d has %d cells in a %d-column pane:\n%s", y, len(row), a.cols, a.text())
		}
	}
}

// Neither replay nor llm-echo reports usage or a model, so the widest
// candidate reachable here is the bare "? keys": it must be whole,
// flush right (one pad space), and nothing on the bar truncated.
func widePaneStatusBar(t *testing.T, a *app) {
	s := a.settled()
	var bar string
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "? keys") {
			bar = l
		}
	}
	if !strings.HasSuffix(bar, "? keys") || strings.Contains(bar, "…") {
		t.Errorf("status bar not whole in a %d-column pane: %q\n%s", a.cols, bar, s)
	}
	if w := len([]rune(bar)); w != a.cols-1 {
		t.Errorf("status bar right side ends at %d, want %d:\n%s", w, a.cols-1, s)
	}
}

// The voice-change rule between blocks is drawn the full pane width.
func widePaneDivider(t *testing.T, a *app) {
	s := a.settled()
	for _, l := range strings.Split(s, "\n") {
		if l != "" && strings.Trim(l, "─") == "" && len([]rune(l)) == a.cols {
			return
		}
	}
	t.Errorf("no full-width ─ divider (%d cells):\n%s", a.cols, s)
}

// The table header lands on one row, wider than a 200-column pane
// could hold without wrapping, and inside the pane.
func widePaneTable(t *testing.T, a *app) {
	s := a.settled()
	for _, l := range strings.Split(s, "\n") {
		all := true
		for _, c := range widePaneCols {
			all = all && strings.Contains(l, c)
		}
		if !all {
			continue
		}
		if w := len([]rune(l)); w < 200 || w > a.cols {
			t.Errorf("table header is %d cells wide, want 200..%d:\n%s", w, a.cols, s)
		}
		return
	}
	t.Errorf("table header not on one row (wrapped or missing):\n%s", s)
}

// Expanding the collapsed result shows its box: the pane minus the
// 4-cell margin box() in plugins/ui/model.go leaves, both borders.
func widePaneBox(t *testing.T, a *app) {
	s := a.settled()
	for y, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "▸") && !strings.Contains(s, "╭") {
			a.click(2, y)
			s = a.settled()
		}
	}
	top, bottom := -1, -1
	for _, l := range strings.Split(s, "\n") {
		l = strings.TrimSpace(l)
		if strings.HasPrefix(l, "╭") {
			top = len([]rune(l))
		}
		if strings.HasPrefix(l, "╰") {
			bottom = len([]rune(l))
		}
	}
	if top != a.cols-4 || bottom != a.cols-4 {
		t.Errorf("box borders %d/%d cells wide, want %d:\n%s", top, bottom, a.cols-4, s)
	}
	a.check("box open")
}
