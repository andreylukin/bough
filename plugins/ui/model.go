package ui

import (
	"strings"

	"charm.land/bubbles/v2/textinput"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

var (
	labelStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("6"))
	youStyle    = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("5"))
	codeStyle   = lipgloss.NewStyle().Faint(true)
	errorStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("1"))
	doneStyle   = lipgloss.NewStyle().Faint(true)
	resultStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(lipgloss.Color("8")).
			Padding(0, 1)
)

// eventMsg carries a loop event into the tea event loop.
type eventMsg Event

// model is the one transcript-plus-composer model used by tui and web.
type model struct {
	vp     viewport.Model
	input  textinput.Model
	events <-chan Event
	inputs chan<- string
	blocks []string
	width  int
}

func newModel(width, height int, inputs chan<- string, events <-chan Event) model {
	ti := textinput.New()
	ti.Prompt = "> "
	ti.Placeholder = "say something"
	ti.SetVirtualCursor(true)
	ti.Focus()

	vp := viewport.New()
	m := model{vp: vp, input: ti, events: events, inputs: inputs}
	m.resize(width, height)
	return m
}

func (m *model) resize(w, h int) {
	m.width = w
	m.vp.SetWidth(w)
	if h > 2 {
		m.vp.SetHeight(h - 2)
	}
	m.input.SetWidth(w - len(m.input.Prompt))
	m.refresh()
}

func (m *model) refresh() {
	m.vp.SetContent(strings.Join(m.blocks, "\n"))
	m.vp.GotoBottom()
}

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

	case eventMsg:
		m.blocks = append(m.blocks, m.render(Event(msg)))
		m.refresh()
		return m, m.waitEvent()

	case tea.KeyPressMsg:
		switch msg.String() {
		case "ctrl+c", "ctrl+d":
			return m, tea.Quit
		case "enter":
			line := strings.TrimSpace(m.input.Value())
			if line == "" {
				return m, nil
			}
			m.input.Reset()
			m.blocks = append(m.blocks, youStyle.Render("you ")+line)
			m.refresh()
			inputs := m.inputs
			return m, func() tea.Msg {
				inputs <- line
				return nil
			}
		}
	}

	var cmds []tea.Cmd
	var cmd tea.Cmd
	m.input, cmd = m.input.Update(msg)
	cmds = append(cmds, cmd)
	m.vp, cmd = m.vp.Update(msg)
	cmds = append(cmds, cmd)
	return m, tea.Batch(cmds...)
}

// render turns a loop event into a styled transcript block.
func (m model) render(ev Event) string {
	switch ev.Kind {
	case "assistant":
		return labelStyle.Render("assistant ") + ev.Text
	case "code":
		return labelStyle.Render("code") + "\n" + codeStyle.Render(ev.Text)
	case "result":
		w := m.width - 4
		if w < 10 {
			w = 10
		}
		return labelStyle.Render("result") + "\n" +
			resultStyle.Width(w).Render(ev.Text)
	case "error":
		return errorStyle.Render("error " + ev.Text)
	case "done":
		return doneStyle.Render("── done ──")
	default:
		return labelStyle.Render(ev.Kind+" ") + ev.Text
	}
}

func (m model) View() tea.View {
	v := tea.NewView(m.vp.View() + "\n" + m.input.View())
	v.AltScreen = true
	return v
}
