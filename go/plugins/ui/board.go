package ui

// The attention board: the work around me from the memory graph, drawn
// at the top of the screen in three columns by whose turn it is. Rows
// come out of the transcript like the todo panel's. The "attention"
// service decides what goes where; this file only draws it.
//
// Form carries the state: the column is whose turn, the bar is how
// long it has been their turn (log scale, a week fills it), the
// spinner is a session on it right now. A row whose facts changed
// since the last read is bright until the next one — changes animate,
// existence does not.

import (
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/andreylukin/bough/plugins/attention"
)

// boardSource is the "attention" service.
type boardSource interface {
	Board() attention.Board
	Sticky() bool
}

// boardMsg is a fresh board read.
type boardMsg struct{ b attention.Board }

// boardTickMsg asks for the next read.
type boardTickMsg struct{}

const (
	boardEvery = 5 * time.Second
	boardMinW  = 90 // narrower than this: one column, NEEDS ME only
)

// boardMax is how many items a column shows before "+N more": the
// board may take a third of the screen, two rows per item, header
// and column titles off the top. Never fewer than two.
func (m *model) boardMax() int { return max(2, (m.height/3-2)/2) }

// boardState is the model's half.
type boardState struct {
	on      bool
	loaded  bool
	b       attention.Board
	facts   map[string]string // key -> status+detail at the last read
	changed map[string]bool   // keys whose facts changed at the last read
}

// loadBoard reads the board off the ui goroutine.
func (m *model) loadBoard(cfg *uiCfg) tea.Cmd {
	src := cfg.board
	if src == nil {
		return nil
	}
	return func() tea.Msg { return boardMsg{src.Board()} }
}

func boardTick() tea.Cmd {
	return tea.Tick(boardEvery, func(time.Time) tea.Msg { return boardTickMsg{} })
}

// takeBoard installs a read and marks what changed since the last one.
func (m *model) takeBoard(b attention.Board) {
	facts := map[string]string{}
	changed := map[string]bool{}
	for _, col := range [][]attention.Item{b.Me, b.Motion, b.Others} {
		for _, it := range col {
			f := it.Status + "|" + it.Detail + "|" + it.Session
			facts[it.Key] = f
			if m.board.loaded && m.board.facts[it.Key] != f {
				changed[it.Key] = true
			}
		}
	}
	m.board.b, m.board.facts, m.board.changed, m.board.loaded = b, facts, changed, true
	m.layoutComposer()
}

// boardMotion reports rows with a live session: they keep the spinner ticking.
func (m *model) boardMotion() bool { return m.board.on && len(m.board.b.Motion) > 0 }

// boardRows is the board as screen rows, nothing when it is off, not
// yet read, or the screen is owned by a picker.
func (m *model) boardRows(cfg *uiCfg) []string {
	if !m.board.on || cfg.board == nil || m.picking || m.mp.open || m.rw.open {
		return nil
	}
	th := cfg.theme
	w := max(m.width, 20)
	b := m.board.b
	now := time.Now()
	head := " current work · /current-work hides"
	if !m.board.loaded {
		head += " · reading the graph…"
	} else if b.Collected.IsZero() {
		head += " · never collected"
	} else {
		age := now.Sub(b.Collected)
		head += " · collected " + b.Collected.Format("15:04")
		if age > 24*time.Hour {
			head += " · STALE"
		}
	}
	rule := func(s string) string {
		n := w - lipgloss.Width(s) - 2
		if n < 0 {
			n = 0
		}
		return th["dim"].Render("┌" + s + " " + strings.Repeat("─", n))
	}
	out := []string{rule(head)}
	if !m.board.loaded {
		return out
	}
	if b.Err != "" && b.Empty() {
		return append(out, " "+th["dim"].Render(b.Err))
	}
	type col struct {
		title string
		style lipgloss.Style
		items []attention.Item
	}
	cols := []col{
		{"NEEDS ME", th["focus"], b.Me},
		{"IN MOTION", th["accent"], b.Motion},
		{"WAITING ON OTHERS", th["dim"], b.Others},
	}
	if w < boardMinW {
		cols = cols[:1]
		// One column: motion and others fold into the count line below.
	}
	cw := (w - 1) / len(cols)
	var rendered [][]string
	height := 0
	for _, c := range cols {
		lines := []string{c.style.Render(c.title) + th["dim"].Render(fmt.Sprintf(" %d", len(c.items)))}
		lines = append(lines, m.boardColumn(cfg, c.items, c.style, cw-1, now)...)
		rendered = append(rendered, lines)
		height = max(height, len(lines))
	}
	if len(cols) == 1 {
		rendered[0] = append(rendered[0], th["dim"].Render(fmt.Sprintf("in motion %d · waiting on others %d", len(b.Motion), len(b.Others))))
		height = len(rendered[0])
	}
	for r := 0; r < height; r++ {
		var row strings.Builder
		row.WriteString(" ")
		for _, lines := range rendered {
			cell := ""
			if r < len(lines) {
				cell = lines[r]
			}
			row.WriteString(cell)
			if pad := cw - lipgloss.Width(cell); pad > 0 {
				row.WriteString(strings.Repeat(" ", pad))
			}
		}
		out = append(out, strings.TrimRight(row.String(), " "))
	}
	return out
}

// boardColumn renders one column's items: bar, name, status; then a
// dim detail line. Stacks show their count. Past boardMax, "+N more".
func (m *model) boardColumn(cfg *uiCfg, items []attention.Item, style lipgloss.Style, width int, now time.Time) []string {
	th := cfg.theme
	var out []string
	for i, it := range items {
		if i == m.boardMax() {
			out = append(out, th["dim"].Render(fmt.Sprintf("+%d more", len(items)-i)))
			break
		}
		mark := m.boardBar(cfg, it, style, now)
		// The title is the human line; the key, age and what it asks
		// go under it, most important first, so a narrow column cuts
		// the tail and not the name.
		name := it.Title
		if it.Count > 0 {
			name = it.Key + th["dim"].Render(fmt.Sprintf(" ×%d", it.Count))
		}
		status := ""
		switch {
		case strings.HasPrefix(it.Status, "ci failing"):
			// A stack says how many of its rows fail, unless all do.
			n := strings.TrimSpace(strings.TrimPrefix(it.Status, "ci failing"))
			if n == fmt.Sprintf("×%d", it.Count) {
				n = ""
			}
			status = th["error"].Render(strings.TrimRight(" ✕ "+n, " "))
		case it.Status == "ci green":
			status = th["accent"].Render(" ✓")
		}
		room := width - lipgloss.Width(mark) - 1 - lipgloss.Width(status)
		nameStyle := lipgloss.NewStyle()
		if m.board.changed[it.Key] {
			nameStyle = th["focus"]
		}
		first := mark + " " + nameStyle.Render(line(name, max(room, 8))) + status
		out = append(out, first)
		detail := shortAge(now.Sub(it.Since)) + " · " + it.Detail
		if it.Count == 0 && it.Title != it.Key {
			detail = attention.ShortKey(it.Key) + " · " + detail
		}
		out = append(out, "  "+th["dim"].Render(line(detail, max(width-2, 8))))
	}
	return out
}

// boardBar is the row's leading mark: the spinner for a session on
// it, else the eight-cell age bar in the column's colour.
func (m *model) boardBar(cfg *uiCfg, it attention.Item, style lipgloss.Style, now time.Time) string {
	th := cfg.theme
	if it.Session != "" {
		return th["accent"].Render(m.spin.View()) + strings.Repeat(" ", 7)
	}
	n := attention.Age(it.Since, now)
	return style.Render(strings.Repeat("▮", n)) + th["dim"].Render(strings.Repeat("▯", 8-n))
}

// shortAge is a duration the width of a chip: 3h, 2d, 5w.
func shortAge(d time.Duration) string {
	switch {
	case d < time.Hour:
		return fmt.Sprintf("%dm", max(int(d.Minutes()), 0))
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	case d < 21*24*time.Hour:
		return fmt.Sprintf("%dd", int(d.Hours()/24))
	default:
		return fmt.Sprintf("%dw", int(d.Hours()/24/7))
	}
}
