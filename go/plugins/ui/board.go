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
	"os/exec"
	"runtime"
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
	Detail(kind, key string) []attention.Line
}

// hoverMsg is a fetched detail for the hovered item.
type hoverMsg struct {
	key   string
	lines []attention.Line
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
	facts   map[string]string           // key -> status+detail at the last read
	changed map[string]bool             // keys whose facts changed at the last read
	hover   string                      // key of the item under the mouse ("" = none)
	detail  map[string][]attention.Line // fetched details by key
}

// boardColumns is the board's columns at the current width: three
// when it fits, NEEDS ME alone when it does not.
func (m *model) boardColumns() [][]attention.Item {
	b := m.board.b
	cols := [][]attention.Item{b.Me, b.Motion, b.Others}
	if max(m.width, 20) < boardMinW {
		return cols[:1]
	}
	return cols
}

// boardHeight is how many screen rows the board takes right now.
func (m *model) boardHeight(cfg *uiCfg) int { return len(m.boardRows(cfg)) }

// boardItemAt is the item drawn at screen position (x, y): the rows
// are header, column titles, then two per item, so the geometry is
// the same arithmetic that laid them out.
func (m *model) boardItemAt(cfg *uiCfg, x, y int) (attention.Item, bool) {
	if !m.board.on || !m.board.loaded || m.board.b.Empty() {
		return attention.Item{}, false
	}
	cols := m.boardColumns()
	cw := (max(m.width, 20) - 1) / len(cols)
	col := (x - 1) / max(cw, 1)
	row := y - 2 // header and titles
	if x < 1 || row < 0 || col < 0 || col >= len(cols) {
		return attention.Item{}, false
	}
	i := row / 2
	if i >= m.boardMax() || i >= len(cols[col]) {
		return attention.Item{}, false
	}
	return cols[col][i], true
}

// boardMouse routes a mouse event that lands on the board: a click
// opens the item's link, motion hovers it. Reports whether the event
// was the board's.
func (m *model) boardMouse(cfg *uiCfg, msg tea.Msg) (bool, tea.Cmd) {
	h := m.boardHeight(cfg)
	if h == 0 {
		return false, nil
	}
	var mouse tea.Mouse
	click := false
	switch e := msg.(type) {
	case tea.MouseClickMsg:
		mouse, click = tea.Mouse(e), e.Button == tea.MouseLeft
	case tea.MouseMotionMsg:
		mouse = tea.Mouse(e)
	case tea.MouseReleaseMsg:
		mouse = tea.Mouse(e)
	default:
		return false, nil
	}
	if mouse.Y >= h {
		m.board.hover = ""
		return false, nil
	}
	it, ok := m.boardItemAt(cfg, mouse.X, mouse.Y)
	if !ok {
		m.board.hover = ""
		return true, nil
	}
	var cmds []tea.Cmd
	if m.board.hover != it.Key {
		m.board.hover = it.Key
		if _, have := m.board.detail[it.Key]; !have && it.Count == 0 {
			// The graph's neighbourhood, off the ui goroutine.
			src, kind, key := cfg.board, it.Kind, it.Key
			cmds = append(cmds, func() tea.Msg { return hoverMsg{key, src.Detail(kind, key)} })
		}
	}
	if click && it.URL != "" {
		url := it.URL
		cmds = append(cmds, func() tea.Msg { openURL(url); return nil })
	}
	return true, tea.Batch(cmds...)
}

// takeHover stores a fetched detail.
func (m *model) takeHover(h hoverMsg) {
	if m.board.detail == nil {
		m.board.detail = map[string][]attention.Line{}
	}
	m.board.detail[h.key] = h.lines
}

// openURL hands a link to the desktop.
func openURL(url string) {
	cmd := "xdg-open"
	if runtime.GOOS == "darwin" {
		cmd = "open"
	}
	_ = exec.Command(cmd, url).Start()
}

// shiftMouse moves a mouse event up by the board's rows, so the code
// below the board keeps its own coordinates.
func shiftMouse(e tea.Mouse, h int) tea.Mouse {
	e.Y -= h
	return e
}

// hoverRows is the detail box for the hovered item, drawn over the
// transcript's top rows so nothing reflows: title, key and facts,
// the source's line, the link, a stack's members.
func (m *model) hoverRows(cfg *uiCfg) []string {
	if m.board.hover == "" {
		return nil
	}
	var it attention.Item
	found := false
	for _, col := range m.boardColumns() {
		for _, x := range col {
			if x.Key == m.board.hover {
				it, found = x, true
			}
		}
	}
	if !found {
		return nil
	}
	th := cfg.theme
	w := max(m.width, 20)
	head := " ▸ " + it.Title
	tail := it.Key
	if it.Status != "" {
		tail += " · " + it.Status
	}
	tail += " · " + shortAge(time.Since(it.Since))
	if it.URL != "" {
		tail = th["accent"].Hyperlink(it.URL).Render(tail)
	} else {
		tail = th["dim"].Render(tail)
	}
	gap := w - lipgloss.Width(head) - lipgloss.Width(tail) - 1
	if gap < 2 {
		head = line(head, max(w-lipgloss.Width(tail)-3, 10))
		gap = 2
	}
	lines := []string{th["focus"].Render(head) + strings.Repeat(" ", gap) + tail}
	row := func(label, text string) {
		lines = append(lines, "   "+th["dim"].Render(fmt.Sprintf("%-8s", label))+line(text, max(w-12, 8)))
	}
	if it.Summary != "" {
		row("source", it.Summary)
	}
	for i, mem := range it.Members {
		if i == 6 {
			row("", fmt.Sprintf("+%d more", len(it.Members)-i))
			break
		}
		row(map[bool]string{true: "stack", false: ""}[i == 0], mem)
	}
	detail, fetched := m.board.detail[it.Key]
	for _, l := range detail {
		row(l.Label, l.Text)
	}
	if !fetched && it.Count == 0 {
		row("", th["dim"].Render("reading the graph…"))
	}
	return lines
}

// overlayTop replaces the top rows of body with lines (when they fit).
func overlayTop(body string, lines []string) string {
	bl := strings.Split(body, "\n")
	if k := len(lines); len(bl) >= k {
		copy(bl[:k], lines)
		body = strings.Join(bl, "\n")
	}
	return body
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
	m.board.detail = nil
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
	if len(m.boardColumns()) == 1 {
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
		if it.Key == m.board.hover {
			nameStyle = nameStyle.Underline(true)
		}
		if it.URL != "" {
			// A real terminal link: cmd-click opens it in the browser
			// even where bough's own click handling is off.
			nameStyle = nameStyle.Hyperlink(it.URL)
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
