package ui

// The model picker: "/model" with no args returns a ModelPickerAction
// whose payload is the current "provider model" line plus one choice
// per line. The picker takes the whole pane like the session picker
// (up/down move, enter dispatches "/model <choice>" so the swap and
// its result land in the transcript, esc goes back), the current
// choice marked and the cursor on it.

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

type modelPicker struct {
	open    bool
	pick    int
	current string
	rows    []string
}

// openModelPicker shows the picker, cursor on the current choice.
func (m *model) openModelPicker(current string, rows []string) {
	m.mp = modelPicker{open: true, current: current, rows: rows}
	for i, r := range rows {
		if r == current {
			m.mp.pick = i
		}
	}
	m.syncPalette()
}

// handleModelPickerKey drives the picker; the quit binding goes back
// rather than quitting, like the session picker mid-session.
func (m model) handleModelPickerKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	key := msg.String()
	if m.cfg.Load().action[key] == "quit" {
		key = "esc"
	}
	switch key {
	case "up":
		if m.mp.pick > 0 {
			m.mp.pick--
		}
	case "down":
		if m.mp.pick < len(m.mp.rows)-1 {
			m.mp.pick++
		}
	case "enter":
		if len(m.mp.rows) == 0 {
			return m, nil
		}
		choice, cur := m.mp.rows[m.mp.pick], m.mp.current
		m.mp = modelPicker{}
		if choice == cur {
			return m, nil
		}
		return m, m.dispatch("/model " + choice)
	case "esc":
		m.mp = modelPicker{}
	}
	return m, nil
}

// modelPickerView renders the full-screen choice list, the current
// row marked, the selected row in the focus style.
func (m *model) modelPickerView(cfg *uiCfg) string {
	th := cfg.theme
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render("· pick a model"),
		"",
	}
	if len(m.mp.rows) == 0 {
		lines = append(lines, th["dim"].Render("  (no providers)"))
	}
	for i, r := range m.mp.rows {
		marker, st := "  ", th["result"]
		if i == m.mp.pick {
			marker, st = "▸ ", th["focus"]
		}
		row := marker + r
		if r == m.mp.current {
			row += " (current)"
		}
		lines = append(lines, st.Render(row))
	}
	hints := th["dim"].Render("↑/↓ select · enter switch · esc back")
	for len(lines) < m.height-1 {
		lines = append(lines, "")
	}
	if m.height > 1 && len(lines) > m.height-1 {
		lines = lines[:m.height-1]
	}
	lines = append(lines, hints)
	for i := range lines {
		lines[i] = ansi.Truncate(lines[i], m.width, "…")
	}
	return strings.Join(lines, "\n")
}
