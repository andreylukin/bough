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
	"fmt"
	"github.com/charmbracelet/x/ansi"
)

type modelPicker struct {
	open    bool
	pick    int
	target  string // "" the agent's model, "small" the cheap one
	current string
	// all is every model of every registered provider; rows is what
	// query leaves of it. OpenRouter alone has 361, so the pane is a
	// search box with a list under it, not a list you scroll.
	all   []string
	rows  []string
	query string
}

// filter narrows all by the query (case-insensitive substring, every
// word must match) and puts the cursor on the current row when it
// survives, else at the top.
func (p *modelPicker) filter() {
	words := strings.Fields(strings.ToLower(p.query))
	p.rows = p.rows[:0]
	for _, r := range p.all {
		low := strings.ToLower(r)
		ok := true
		for _, w := range words {
			if !strings.Contains(low, w) {
				ok = false
				break
			}
		}
		if ok {
			p.rows = append(p.rows, r)
		}
	}
	p.pick = 0
	for i, r := range p.rows {
		if r == p.current {
			p.pick = i
		}
	}
}

// openModelPicker shows the picker, cursor on the current choice.
func (m *model) openModelPicker(target, current string, rows []string) {
	m.mp = modelPicker{open: true, target: target, current: current, all: rows}
	m.mp.filter() // puts the cursor on the current row
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
		choice, cur, target := m.mp.rows[m.mp.pick], m.mp.current, m.mp.target
		m.mp = modelPicker{}
		if choice == cur {
			return m, nil
		}
		if target != "" {
			return m, m.dispatch("/model " + target + " " + choice)
		}
		return m, m.dispatch("/model " + choice)
	case "tab":
		// One picker, either model: tab re-opens it on the other row.
		next := "small"
		if m.mp.target == "small" {
			next = ""
		}
		m.mp = modelPicker{}
		return m, m.dispatch(strings.TrimSpace("/model " + next))
	case "esc":
		m.mp = modelPicker{}
	case "backspace":
		if q := []rune(m.mp.query); len(q) > 0 {
			m.mp.query = string(q[:len(q)-1])
			m.mp.filter()
		}
	case "ctrl+u":
		m.mp.query = ""
		m.mp.filter()
	case "space":
		// bubbletea names this key rather than reporting the rune, so
		// the branch below would drop it and "openrouter astra" would
		// filter as one word.
		m.mp.query += " "
		m.mp.filter()
	default:
		// Anything else that is a single character types into the
		// query. 361 OpenRouter models is not a list to scroll.
		if r := []rune(key); len(r) == 1 {
			m.mp.query += key
			m.mp.filter()
		}
	}
	return m, nil
}

// modelPickerView renders the full-screen choice list, the current
// row marked, the selected row in the focus style.
func (m *model) modelPickerView(cfg *uiCfg) string {
	th := cfg.theme
	what := "· pick a model"
	if m.mp.target == "small" {
		what = "· pick the small model (session names, memory, status line, autocomplete)"
	}
	// The search box. Every provider's whole catalogue is behind it,
	// so the count says how much the query left.
	count := fmt.Sprintf("%d of %d", len(m.mp.rows), len(m.mp.all))
	query := m.mp.query
	if query == "" {
		query = th["dim"].Render("type to search")
	}
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render(what),
		"",
		th["accent"].Render("search ") + query + th["dim"].Render("  "+count),
		"",
	}
	if len(m.mp.rows) == 0 {
		lines = append(lines, th["dim"].Render("  nothing matches — backspace to widen, esc to leave"))
	}
	// A window around the cursor: the unfiltered list is every model of
	// every provider, which does not fit and should not have to.
	room := max(m.height-7, 3)
	first := 0
	if len(m.mp.rows) > room && m.mp.pick >= room {
		first = min(m.mp.pick-room+1, len(m.mp.rows)-room)
	}
	if first > 0 {
		lines = append(lines, th["dim"].Render(fmt.Sprintf("  ↑ %d more above", first)))
	}
	for i := first; i < len(m.mp.rows) && i-first < room; i++ {
		r := m.mp.rows[i]
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
	if rest := len(m.mp.rows) - first - room; rest > 0 {
		lines = append(lines, th["dim"].Render(fmt.Sprintf("  ↓ %d more below", rest)))
	}
	hints := th["dim"].Render("type to search · ↑/↓ select · enter switch · esc back")
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
