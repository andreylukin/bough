package ui

import (
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

// collapseAt: result blocks longer than this many lines start
// collapsed (head + a "... N more lines" tail).
const collapseAt = 12
const collapseHead = 8

// block is one semantic transcript unit, stored structurally and
// styled at render time from the current theme.
type block struct {
	kind      string // user, assistant, code, result, error, done
	text      string
	collapsed bool
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
	width      int
	height     int
	running    bool // a turn is in flight (input sent, no done/error yet)
	inspecting bool // history overlay open
	flash      string
	md         *glamour.TermRenderer
	mdCache    map[string]string // assistant markdown render cache (cleared on resize)
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

	m := model{vp: vp, overlay: ov, input: ti, spin: sp, send: send, events: events, cfg: cfg, mdCache: map[string]string{}}
	m.resize(width, height)
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
}

// refresh restyles every block from the current theme and pins the
// transcript to the bottom.
func (m *model) refresh() {
	cfg := m.cfg.Load()
	parts := make([]string, 0, len(m.blocks))
	for i := range m.blocks {
		parts = append(parts, m.render(&m.blocks[i], cfg))
	}
	m.vp.SetContent(strings.Join(parts, "\n"))
	m.vp.GotoBottom()
}

// markdown renders assistant text via glamour, falling back to the
// raw text on any error.
func (m *model) markdown(text string) string {
	if cached, ok := m.mdCache[text]; ok {
		return cached
	}
	if m.md == nil {
		w := m.width - 2
		if w < 20 {
			w = 20
		}
		r, err := glamour.NewTermRenderer(
			glamour.WithStandardStyle("dark"),
			glamour.WithWordWrap(w),
			glamour.WithEmoji(),
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
		return m.box(b.text, "js", th["code"], th["border"], th["dim"])
	case "result":
		text := b.text
		tag := "result"
		if lines := strings.Split(text, "\n"); b.collapsed && len(lines) > collapseHead {
			hidden := len(lines) - collapseHead
			text = strings.Join(lines[:collapseHead], "\n") +
				"\n" + th["dim"].Render(fmt.Sprintf("… %d more lines (%s)", hidden, cfg.keys["collapse_toggle"]))
			tag = "result +"
		}
		return m.box(text, tag, th["result"], th["border"], th["dim"])
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

// box renders text in a rounded border with a language/kind tag
// spliced into the top border.
func (m *model) box(text, tag string, content, border, dim lipgloss.Style) string {
	w := m.width - 4
	if w < 10 {
		w = 10
	}
	boxed := content.
		Border(lipgloss.RoundedBorder()).
		BorderForeground(border.GetForeground()).
		Padding(0, 1).
		Width(w).
		Render(strings.TrimRight(text, "\n"))
	lines := strings.SplitN(boxed, "\n", 2)
	if len(lines) == 2 {
		// Rebuild the top border with the tag spliced in, at exactly
		// the rendered box width.
		bw := lipgloss.Width(lines[0])
		top := dim.Render("╭─ ") + dim.Italic(true).Render(tag) + " " +
			border.Render(strings.Repeat("─", max(0, bw-lipgloss.Width(tag)-5))+"╮")
		return top + "\n" + lines[1]
	}
	return boxed
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
	return m.waitEvent()
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

	case tea.KeyPressMsg:
		return m.handleKey(msg)
	}

	var cmds []tea.Cmd
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	cmds = append(cmds, cmd)
	if m.inspecting {
		m.overlay, cmd = m.overlay.Update(msg)
	} else {
		m.vp, cmd = m.vp.Update(msg)
	}
	cmds = append(cmds, cmd)
	return m, tea.Batch(cmds...)
}

// addEvent appends the semantic block for a loop event.
func (m *model) addEvent(ev Event) {
	switch ev.Kind {
	case "done":
		m.running = false
		m.blocks = append(m.blocks, block{kind: "done"})
	case "error":
		m.running = false
		m.blocks = append(m.blocks, block{kind: "error", text: ev.Text})
	case "result":
		collapsed := strings.Count(ev.Text, "\n")+1 > collapseAt
		m.blocks = append(m.blocks, block{kind: "result", text: ev.Text, collapsed: collapsed})
	default: // assistant, code, anything future
		m.blocks = append(m.blocks, block{kind: ev.Kind, text: ev.Text})
	}
	m.refresh()
}

// handleKey resolves every binding through the keymap service; only
// enter (submit — not a remappable action) is fixed.
func (m model) handleKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	cfg := m.cfg.Load()
	m.flash = ""
	key := msg.String()

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
		return m, nil
	case "collapse_toggle":
		for i := len(m.blocks) - 1; i >= 0; i-- {
			if m.blocks[i].kind == "result" {
				m.blocks[i].collapsed = !m.blocks[i].collapsed
				m.refresh()
				break
			}
		}
		return m, nil
	case "history_inspect":
		if cfg.hist == nil {
			m.flash = "no history service mounted"
			return m, nil
		}
		m.inspecting = !m.inspecting
		if m.inspecting {
			m.overlay.SetContent(historyDump(cfg))
			m.overlay.GotoBottom()
		}
		return m, nil
	}

	if key == "enter" && !m.inspecting {
		line := strings.TrimSpace(m.input.Value())
		if line == "" {
			return m, nil
		}
		m.input.Reset()
		m.blocks = append(m.blocks, block{kind: "user", text: line})
		m.refresh()
		m.running = true
		send := m.send
		return m, tea.Batch(
			func() tea.Msg { send(line); return nil },
			m.spin.Tick,
		)
	}

	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
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

// historyDump renders the raw history entries for the inspect overlay.
func historyDump(cfg *uiCfg) string {
	th := cfg.theme
	entries := cfg.hist.Entries()
	var sb strings.Builder
	sb.WriteString(th["accent"].Render("history") + " " + th["dim"].Render(cfg.hist.Path()) + "\n\n")
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		preview := strings.SplitN(text, "\n", 2)[0]
		if len(preview) > 80 {
			preview = preview[:80] + "…"
		}
		sb.WriteString(fmt.Sprintf("%s %s %s %s\n",
			th["dim"].Render(fmt.Sprintf("%4d", e.Seq)),
			th["dim"].Render(e.At.Format("15:04:05")),
			th["accent"].Render(fmt.Sprintf("%-9s", e.Kind)),
			preview))
	}
	if len(entries) == 0 {
		sb.WriteString(th["dim"].Render("(no entries yet)"))
	}
	return sb.String()
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
	body := m.vp.View()
	if m.inspecting {
		body = m.overlay.View()
	}
	v := tea.NewView(body + "\n" + m.statusBar(cfg) + "\n" + m.input.View())
	v.AltScreen = true
	v.MouseMode = tea.MouseModeCellMotion // wheel scroll
	return v
}
