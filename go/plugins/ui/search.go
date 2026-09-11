package ui

// Transcript search (the "search" action, ctrl+s): a query line in
// place of the status bar that owns the keys while it is open. Typing
// edits the query, enter / ctrl+n jump to the next older match, ctrl+p
// to the next newer one, esc closes. The composer and the running turn
// never see those keys: a query is not a steer, and esc is not a cancel.

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	xansi "github.com/charmbracelet/x/ansi"
)

type searchBar struct {
	open  bool
	query string
	hits  []int // transcript line numbers holding the query
	at    int   // index into hits of the match in view; -1 before a jump
}

// searchHits is every transcript line holding q, case-insensitively.
func (m *model) searchHits(q string) []int {
	if q == "" {
		return nil
	}
	q = strings.ToLower(q)
	var hits []int
	for i, l := range m.lines {
		if strings.Contains(strings.ToLower(xansi.Strip(l)), q) {
			hits = append(hits, i)
		}
	}
	return hits
}

// searchJump moves to the next match by step (-1 older, +1 newer),
// recomputed against the transcript as it is now: a reply still
// streaming may have grown one since the last key.
func (m *model) searchJump(step int) {
	m.srch.hits = m.searchHits(m.srch.query)
	n := len(m.srch.hits)
	if n == 0 {
		m.srch.at = -1
		return
	}
	if m.srch.at < 0 || m.srch.at >= n {
		m.srch.at = n - 1 // the newest match first
	} else {
		m.srch.at = (m.srch.at + step + n) % n
	}
	m.vp.SetYOffset(m.srch.hits[m.srch.at])
}

func (m model) handleSearchKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	key := msg.String()
	if m.cfg.Load().action[key] == "quit" {
		key = "esc"
	}
	switch key {
	case "esc":
		m.srch = searchBar{}
	case "enter", "ctrl+n":
		m.searchJump(-1)
	case "ctrl+p":
		m.searchJump(1)
	case "backspace":
		if r := []rune(m.srch.query); len(r) > 0 {
			m.srch.query = string(r[:len(r)-1])
			m.srch.at = -1
		}
	default:
		if msg.Text != "" {
			m.srch.query += msg.Text
			m.srch.at = -1
		}
	}
	return m, nil
}

// searchLine replaces the status bar while search is open.
func (m *model) searchLine(cfg *uiCfg) string {
	th := cfg.theme
	count := "enter next · esc closes"
	if m.srch.query != "" {
		count = "no matches"
		if hits := m.searchHits(m.srch.query); len(hits) > 0 {
			pos := "-"
			if m.srch.at >= 0 && m.srch.at < len(hits) {
				pos = fmt.Sprint(m.srch.at + 1)
			}
			count = fmt.Sprintf("%s/%d", pos, len(hits))
		}
	}
	return xansi.Truncate(th["accent"].Render(" search ")+sanitizeText(m.srch.query)+th["dim"].Render("  "+count), max(m.width, 1), "…")
}
