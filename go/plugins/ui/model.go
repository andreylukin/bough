package ui

import (
	"encoding/json"
	"fmt"
	"strings"
	"sync/atomic"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textinput"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2"
	"charm.land/lipgloss/v2"
)

// eventMsg carries a loop event into the tea event loop.
type eventMsg Event

// collapseAt: code and result blocks whose body is longer than this
// many lines start collapsed (header line only).
const collapseAt = 3

// block is one semantic transcript unit, stored structurally and
// styled at render time from the current theme. id is stable identity
// (blocks are append-only) so collapse/focus state survives appends.
type block struct {
	id        int
	kind      string // user, assistant, code, result, error, done
	text      string
	collapsed bool
}

// collapsible blocks get a disclosure header and can be toggled.
func (b *block) collapsible() bool {
	return b.kind == "code" || b.kind == "result"
}

// lineRange maps a rendered line span [start, end) to a block index.
type lineRange struct {
	start, end, idx int
}

// model is the one transcript-plus-composer model used by tui and web.
type model struct {
	vp      viewport.Model
	overlay viewport.Model
	input   textinput.Model
	spin    spinner.Model
	events  <-chan Event
	send    func(string)
	cfg     *atomic.Pointer[uiCfg]

	blocks     []block
	nextID     int
	focusID    int // block-cursor identity; -1 when nothing focused
	ranges     []lineRange
	width      int
	height     int
	running    bool           // a turn is in flight (input sent, no done/error yet)
	inspecting bool           // history overlay open
	ovRanges   []lineRange    // overlay line span -> entry index
	ovExpanded map[int64]bool // entry seq -> inline JSON shown
	ovEntries  []int64        // entry index -> seq, for ovRanges lookups
	picking    bool           // session picker shown instead of the chat view
	pick       int            // picker cursor index into cfg.sessions
	pal        palette        // "/" command palette (see palette.go)
	flash      string
	md         *glamour.TermRenderer
	mdCache    map[string]string // assistant markdown render cache (cleared on resize)
	bgLight    bool              // terminal background is light (tea.BackgroundColorMsg)
}

func newModel(width, height int, send func(string), events <-chan Event, cfg *atomic.Pointer[uiCfg]) model {
	ti := textinput.New()
	ti.Prompt = "> "
	ti.Placeholder = "say something"
	ti.SetVirtualCursor(true)
	ti.Focus()

	vp := viewport.New()
	vp.KeyMap = viewport.KeyMap{} // all keys resolve through the keymap service
	ov := viewport.New()
	ov.KeyMap = viewport.KeyMap{}

	sp := spinner.New()
	sp.Spinner = spinner.MiniDot

	m := model{vp: vp, overlay: ov, input: ti, spin: sp, send: send, events: events, cfg: cfg,
		focusID: -1, ovExpanded: map[int64]bool{}, mdCache: map[string]string{}}
	m.resize(width, height)
	if cfg.Load().picker {
		m.picking = true // replay happens after the pick (leavePicker)
	} else {
		m.replay()
	}
	return m
}

func (m *model) resize(w, h int) {
	m.width = w
	m.height = h
	if h > 2 {
		m.vp.SetHeight(h - 2)
		m.overlay.SetHeight(h - 2)
	}
	m.vp.SetWidth(w)
	m.overlay.SetWidth(w)
	m.input.SetWidth(w - len(m.input.Prompt))
	m.md = nil // re-wrap markdown at the new width
	m.mdCache = map[string]string{}
	m.refresh()
	if m.inspecting {
		m.refreshOverlay()
	}
}

// refresh restyles every block from the current theme, records each
// block's rendered line span for mouse hit-testing, and keeps the
// scroll position (still pinned when it was at the bottom). Parts are
// blank-squeezed so at most one blank line separates blocks.
func (m *model) refresh() {
	cfg := m.cfg.Load()
	atBottom := m.vp.AtBottom()
	parts := make([]string, 0, len(m.blocks))
	m.ranges = m.ranges[:0]
	start := 0
	for i := range m.blocks {
		part := squeezeBlanks(m.render(&m.blocks[i], cfg))
		if i == 0 {
			part = strings.TrimLeft(part, "\n")
		}
		n := strings.Count(part, "\n") + 1
		m.ranges = append(m.ranges, lineRange{start: start, end: start + n, idx: i})
		start += n
		parts = append(parts, part)
	}
	m.vp.SetContent(strings.Join(parts, "\n"))
	if atBottom {
		m.vp.GotoBottom()
	}
}

// markdown renders assistant text via glamour, falling back to the
// raw text on any error. The style follows the detected terminal
// background (dark until told otherwise); a theme service "markdown"
// entry ("dark"/"light") overrides detection.
func (m *model) markdown(text string) string {
	if cached, ok := m.mdCache[text]; ok {
		return cached
	}
	if m.md == nil {
		w := m.width - 2
		if w < 20 {
			w = 20
		}
		style := "dark"
		if m.bgLight {
			style = "light"
		}
		if s := m.cfg.Load().mdStyle; s != "" {
			style = s
		}
		r, err := glamour.NewTermRenderer(
			glamour.WithStandardStyle(style),
			glamour.WithWordWrap(w),
			glamour.WithEmoji(),
			// Hard newlines survive: "[tool output]\nhi" is two lines,
			// not one reflowed paragraph.
			glamour.WithPreservedNewLines(),
		)
		if err != nil {
			return text
		}
		m.md = r
	}
	out, err := m.md.Render(text)
	if err != nil {
		return text
	}
	out = strings.Trim(out, "\n")
	m.mdCache[text] = out
	return out
}

// squeezeBlanks trims trailing blank lines from a rendered part and
// collapses interior runs of blank lines to a single one, keeping the
// transcript to at most one blank line between blocks.
func squeezeBlanks(s string) string {
	lines := strings.Split(s, "\n")
	out := lines[:0]
	blanks := 0
	for _, l := range lines {
		if strings.TrimSpace(l) == "" {
			if blanks++; blanks > 1 {
				continue
			}
		} else {
			blanks = 0
		}
		out = append(out, l)
	}
	for len(out) > 0 && strings.TrimSpace(out[len(out)-1]) == "" {
		out = out[:len(out)-1]
	}
	return strings.Join(out, "\n")
}

// header renders the one-line disclosure header for a collapsible
// block: glyph, kind tag, body line count, first-line preview. The
// focused block's header takes the "focus" theme style.
func (m *model) header(b *block, th theme) string {
	glyph := "▸"
	if !b.collapsed {
		glyph = "▾"
	}
	tag := "result"
	if b.kind == "code" {
		tag = "code js"
	}
	n := strings.Count(b.text, "\n") + 1
	unit := "lines"
	if n == 1 {
		unit = "line"
	}
	preview := strings.SplitN(b.text, "\n", 2)[0]
	head := fmt.Sprintf("%s %s (%d %s): %s", glyph, tag, n, unit, preview)
	if r := []rune(head); len(r) > m.width-1 && m.width > 2 {
		head = string(r[:m.width-2]) + "…"
	}
	st := th["dim"]
	if b.id == m.focusID {
		st = th["focus"]
	}
	return st.Render(head)
}

// render turns one semantic block into styled lines.
func (m *model) render(b *block, cfg *uiCfg) string {
	th := cfg.theme
	switch b.kind {
	case "user":
		return "\n" + th["user"].Render("❯ "+b.text)
	case "assistant":
		head := th["accent"].Render("●") + " " + th["dim"].Render("bough")
		return head + "\n" + m.markdown(b.text)
	case "code":
		if b.collapsed {
			return m.header(b, th)
		}
		return m.header(b, th) + "\n" + m.box(b.text, th["code"], th["border"])
	case "result":
		if b.collapsed {
			return m.header(b, th)
		}
		return m.header(b, th) + "\n" + m.box(b.text, th["result"], th["border"])
	case "command":
		// The dispatched "/" line: a dim echo of what was typed, so
		// the system block below reads as its answer.
		return "\n" + th["dim"].Render("❯ "+b.text)
	case "system":
		// Plain dimmed command output: not collapsible, no ● header.
		return th["system"].Render(b.text)
	case "error":
		return th["error"].Render("✗ " + b.text)
	case "done":
		w := m.width - 2
		if w > 40 {
			w = 40
		}
		if w < 1 {
			w = 1
		}
		return th["dim"].Render(strings.Repeat("─", w))
	default:
		return th["dim"].Render(b.kind+" ") + b.text
	}
}

// box renders text in a rounded border.
func (m *model) box(text string, content, border lipgloss.Style) string {
	w := m.width - 4
	if w < 10 {
		w = 10
	}
	return content.
		Border(lipgloss.RoundedBorder()).
		BorderForeground(border.GetForeground()).
		Padding(0, 1).
		Width(w).
		Render(strings.TrimRight(text, "\n"))
}

// waitEvent blocks for the next loop event.
func (m model) waitEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.events
		if !ok {
			return nil
		}
		return eventMsg(ev)
	}
}

func (m model) Init() tea.Cmd {
	return tea.Batch(m.waitEvent(), tea.RequestBackgroundColor)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.resize(msg.Width, msg.Height)
		return m, nil

	case spinner.TickMsg:
		if !m.running {
			return m, nil
		}
		var cmd tea.Cmd
		m.spin, cmd = m.spin.Update(msg)
		return m, cmd

	case eventMsg:
		m.addEvent(Event(msg))
		return m, m.waitEvent()

	case tea.BackgroundColorMsg:
		// Re-render markdown for the actual terminal background so
		// dark-style grays never land on a light terminal.
		if light := !msg.IsDark(); light != m.bgLight {
			m.bgLight = light
			m.md = nil
			m.mdCache = map[string]string{}
			m.refresh()
		}
		return m, nil

	case tea.KeyPressMsg:
		return m.handleKey(msg)

	case tea.MouseClickMsg:
		return m, m.handleClick(msg.Mouse())
	}

	var cmds []tea.Cmd
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette() // e.g. paste can change the draft
	cmds = append(cmds, cmd)
	if m.inspecting {
		m.overlay, cmd = m.overlay.Update(msg)
	} else {
		m.vp, cmd = m.vp.Update(msg)
	}
	cmds = append(cmds, cmd)
	return m, tea.Batch(cmds...)
}

// addEvent appends the semantic block for a loop event. Whether code
// and result blocks start collapsed follows cfg.collapse: "all"
// (default) collapses every one, "large" only those over collapseAt
// lines, "none" leaves them expanded.
func (m *model) addEvent(ev Event) {
	id := m.nextID
	m.nextID++
	switch ev.Kind {
	case "done":
		m.running = false
		m.blocks = append(m.blocks, block{id: id, kind: "done"})
	case "error":
		m.running = false
		m.blocks = append(m.blocks, block{id: id, kind: "error", text: ev.Text})
	case "code", "result":
		if ev.Kind == "code" {
			m.dedupeCode(ev.Text)
		}
		// Trailing newlines don't render (box trims them); don't let
		// them skew the header's line count or the collapse default.
		text := strings.TrimRight(ev.Text, "\n")
		var collapsed bool
		switch m.cfg.Load().collapse {
		case "none":
		case "large":
			collapsed = strings.Count(text, "\n")+1 > collapseAt
		default: // "all"
			collapsed = true
		}
		m.blocks = append(m.blocks, block{id: id, kind: ev.Kind, text: text, collapsed: collapsed})
	default: // assistant, anything future
		m.blocks = append(m.blocks, block{id: id, kind: ev.Kind, text: ev.Text})
	}
	m.refresh()
	m.vp.GotoBottom() // new events pin the transcript to the bottom
}

// dedupeCode strips, from this turn's assistant blocks, the fenced
// code block whose body matches an executed code event (exact modulo
// trailing newline): the collapsible code block is the single
// rendering of executed code. An assistant block left with only
// whitespace is dropped entirely — no orphan "● bough" header.
func (m *model) dedupeCode(code string) {
	want := strings.TrimRight(code, "\n")
	for i := len(m.blocks) - 1; i >= 0; i-- {
		b := &m.blocks[i]
		switch b.kind {
		case "user", "done", "error":
			return // turn boundary
		case "assistant":
			txt, ok := stripFence(b.text, want)
			if !ok {
				continue
			}
			if strings.TrimSpace(txt) == "" {
				m.blocks = append(m.blocks[:i], m.blocks[i+1:]...)
			} else {
				b.text = txt
			}
			return
		}
	}
}

// stripFence removes the first ```-fenced block whose body equals want
// (modulo trailing newline) from text, reporting whether one matched.
func stripFence(text, want string) (string, bool) {
	lines := strings.Split(text, "\n")
	for i := 0; i < len(lines); i++ {
		if !strings.HasPrefix(strings.TrimSpace(lines[i]), "```") {
			continue
		}
		j := i + 1
		for j < len(lines) && strings.TrimSpace(lines[j]) != "```" {
			j++
		}
		if j < len(lines) && strings.TrimRight(strings.Join(lines[i+1:j], "\n"), "\n") == want {
			out := append(append([]string{}, lines[:i]...), lines[j+1:]...)
			return strings.Join(out, "\n"), true
		}
		i = j // skip past this fence (or to EOF)
	}
	return "", false
}

// handleClick maps a left click to the block (or history entry) under
// it and toggles its collapsed state. Wheel scrolling stays with the
// viewports (they handle MouseWheelMsg themselves).
func (m *model) handleClick(mouse tea.Mouse) tea.Cmd {
	if mouse.Button != tea.MouseLeft || m.picking {
		return nil
	}
	if m.inspecting {
		m.clickOverlay(mouse)
		return nil
	}
	if handled, cmd := m.clickPalette(mouse); handled {
		return cmd
	}
	if mouse.Y >= m.vp.Height() {
		return nil // status bar / composer
	}
	row := mouse.Y + m.vp.YOffset()
	for _, r := range m.ranges {
		if row >= r.start && row < r.end {
			b := &m.blocks[r.idx]
			if b.collapsible() {
				b.collapsed = !b.collapsed
				m.focusID = b.id
				m.refresh()
			}
			return nil
		}
	}
	return nil
}

// clickOverlay toggles the inline pretty-JSON view of the history
// entry under the click.
func (m *model) clickOverlay(mouse tea.Mouse) {
	if mouse.Y >= m.overlay.Height() {
		return
	}
	row := mouse.Y + m.overlay.YOffset()
	for _, r := range m.ovRanges {
		if row >= r.start && row < r.end {
			seq := m.ovEntries[r.idx]
			m.ovExpanded[seq] = !m.ovExpanded[seq]
			m.refreshOverlay()
			return
		}
	}
}

// focusables returns the indices of collapsible blocks, in order.
func (m *model) focusables() []int {
	var out []int
	for i := range m.blocks {
		if m.blocks[i].collapsible() {
			out = append(out, i)
		}
	}
	return out
}

// moveFocus steps the block cursor by delta over the collapsible
// blocks (wrapping), scrolling the focused header into view.
func (m *model) moveFocus(delta int) {
	f := m.focusables()
	if len(f) == 0 {
		return
	}
	cur := -1
	for i, idx := range f {
		if m.blocks[idx].id == m.focusID {
			cur = i
			break
		}
	}
	var next int
	if cur < 0 {
		next = 0
		if delta < 0 {
			next = len(f) - 1
		}
	} else {
		next = (cur + delta + len(f)) % len(f)
	}
	m.focusID = m.blocks[f[next]].id
	m.refresh()
	for _, r := range m.ranges {
		if r.idx == f[next] {
			m.vp.EnsureVisible(r.start, 0, 0)
			break
		}
	}
}

// toggleFocused flips the focused block's collapsed state; false when
// nothing is focused.
func (m *model) toggleFocused() bool {
	for i := range m.blocks {
		if m.blocks[i].id == m.focusID && m.blocks[i].collapsible() {
			m.blocks[i].collapsed = !m.blocks[i].collapsed
			m.refresh()
			return true
		}
	}
	return false
}

// setAllCollapsed collapses or expands every collapsible block.
func (m *model) setAllCollapsed(collapsed bool) {
	for i := range m.blocks {
		if m.blocks[i].collapsible() {
			m.blocks[i].collapsed = collapsed
		}
	}
	m.refresh()
}

// handleKey resolves every binding through the keymap service; only
// enter (submit — not a remappable action) is fixed. Enter first acts
// as collapse_toggle when a block is focused, and submits otherwise.
func (m model) handleKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	if m.picking {
		return m.handlePickerKey(msg)
	}
	cfg := m.cfg.Load()
	m.flash = ""
	key := msg.String()

	// The palette owns Up/Down/Tab/Enter/Esc while it is open, and
	// nothing else: what it passes on falls through to the keymap
	// waterfall and the composer (which re-filters).
	if m.pal.open && !m.inspecting {
		if handled, cmd := m.paletteKey(key); handled {
			return m, cmd
		}
	}

	switch cfg.action[key] {
	case "quit":
		return m, tea.Quit
	case "scroll_up":
		m.pane().ScrollUp(1)
		return m, nil
	case "scroll_down":
		m.pane().ScrollDown(1)
		return m, nil
	case "page_up":
		m.pane().PageUp()
		return m, nil
	case "page_down":
		m.pane().PageDown()
		return m, nil
	case "clear_input":
		m.input.Reset()
		m.syncPalette()
		return m, nil
	case "block_next":
		if !m.inspecting {
			m.moveFocus(1)
		}
		return m, nil
	case "block_prev":
		if !m.inspecting {
			m.moveFocus(-1)
		}
		return m, nil
	case "collapse_all":
		m.setAllCollapsed(true)
		return m, nil
	case "expand_all":
		m.setAllCollapsed(false)
		return m, nil
	case "collapse_toggle":
		if key == "enter" && strings.TrimSpace(m.input.Value()) != "" {
			break // composing: enter submits below
		}
		if !m.inspecting && m.toggleFocused() {
			return m, nil
		}
		// nothing focused: enter falls through to submit below; any
		// other bound key toggles the newest collapsible block.
		if key != "enter" {
			if f := m.focusables(); !m.inspecting && len(f) > 0 {
				i := f[len(f)-1]
				m.blocks[i].collapsed = !m.blocks[i].collapsed
				m.focusID = m.blocks[i].id
				m.refresh()
			}
			return m, nil
		}
	case "history_inspect":
		if cfg.hist == nil {
			m.flash = "no history service mounted"
			return m, nil
		}
		m.inspecting = !m.inspecting
		if m.inspecting {
			m.refreshOverlay()
			m.overlay.GotoBottom()
		}
		m.syncPalette() // the palette is inert under the inspector
		return m, nil
	}

	if key == "enter" && !m.inspecting {
		line := strings.TrimSpace(m.input.Value())
		if line == "" {
			return m, nil
		}
		// A submitted "/" line NEVER reaches the LLM: it dispatches
		// through the commands service (absent service: plain text).
		if strings.HasPrefix(line, "/") && cfg.cmds != nil {
			return m, m.dispatch(line)
		}
		m.input.Reset()
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "user", text: line})
		m.nextID++
		m.refresh()
		m.vp.GotoBottom()
		m.running = true
		send := m.send
		return m, tea.Batch(
			func() tea.Msg { send(line); return nil },
			m.spin.Tick,
		)
	}

	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	m.syncPalette()
	return m, cmd
}

// pane returns the scroll target: the overlay while inspecting,
// otherwise the transcript.
func (m *model) pane() *viewport.Model {
	if m.inspecting {
		return &m.overlay
	}
	return &m.vp
}

// refreshOverlay rebuilds the history overlay content, recording each
// entry's rendered line span for click hit-testing. Entries toggled
// open show their pretty-printed JSON inline.
func (m *model) refreshOverlay() {
	cfg := m.cfg.Load()
	th := cfg.theme
	var sb strings.Builder
	sb.WriteString(th["accent"].Render("history") + " " + th["dim"].Render(cfg.hist.Path()) + "\n\n")
	line := 2
	m.ovRanges = m.ovRanges[:0]
	m.ovEntries = m.ovEntries[:0]
	entries := cfg.hist.Entries()
	for i, e := range entries {
		text, _ := e.Data["text"].(string)
		preview := strings.SplitN(text, "\n", 2)[0]
		if len(preview) > 80 {
			preview = preview[:80] + "…"
		}
		part := fmt.Sprintf("%s %s %s %s",
			th["dim"].Render(fmt.Sprintf("%4d", e.Seq)),
			th["dim"].Render(e.At.Format("15:04:05")),
			th["accent"].Render(fmt.Sprintf("%-9s", e.Kind)),
			preview)
		if m.ovExpanded[e.Seq] {
			js, err := json.MarshalIndent(e, "     ", "  ")
			if err != nil {
				js = []byte(fmt.Sprintf("marshal: %v", err))
			}
			part += "\n     " + string(js)
		}
		n := strings.Count(part, "\n") + 1
		m.ovRanges = append(m.ovRanges, lineRange{start: line, end: line + n, idx: i})
		m.ovEntries = append(m.ovEntries, e.Seq)
		line += n
		sb.WriteString(part + "\n")
	}
	if len(entries) == 0 {
		sb.WriteString(th["dim"].Render("(no entries yet)"))
	}
	off := m.overlay.YOffset()
	m.overlay.SetContent(sb.String())
	m.overlay.SetYOffset(off)
}

// statusBar renders the one-line status: identity left, session
// state right, filled with the status style.
func (m *model) statusBar(cfg *uiCfg) string {
	th := cfg.theme
	left := " " + cfg.status
	var right string
	if m.flash != "" {
		right = m.flash
	} else {
		if cfg.hist != nil {
			n := len(cfg.hist.Entries())
			name := cfg.hist.Path()
			if i := strings.LastIndexByte(name, '/'); i >= 0 {
				name = name[i+1:]
			}
			right = fmt.Sprintf("%d entries · %s", n, name)
		}
		if m.inspecting {
			right = "inspecting · " + cfg.keys["history_inspect"] + " to close"
		}
	}
	if m.running {
		right = m.spin.View() + " " + right
	}
	right += " "
	gap := m.width - lipgloss.Width(left) - lipgloss.Width(right)
	if gap < 1 {
		gap = 1
	}
	return th["status"].Width(m.width).Render(left + strings.Repeat(" ", gap) + right)
}

func (m model) View() tea.View {
	cfg := m.cfg.Load()
	if m.picking {
		v := tea.NewView(m.pickerView(cfg))
		v.AltScreen = true
		return v
	}
	body := m.vp.View()
	if m.inspecting {
		body = m.overlay.View()
	} else if lines := m.paletteRows(); len(lines) > 0 {
		// The "/" palette: an overlay over the transcript's bottom
		// rows, directly above the composer — sized to its content,
		// never reflowing the layout under a filtering list.
		bl := strings.Split(body, "\n")
		if k := len(lines); len(bl) >= k {
			copy(bl[len(bl)-k:], lines)
			body = strings.Join(bl, "\n")
		}
	}
	v := tea.NewView(body + "\n" + m.statusBar(cfg) + "\n" + m.input.View())
	v.AltScreen = true
	v.MouseMode = tea.MouseModeCellMotion // clicks toggle blocks; wheel scrolls
	return v
}
