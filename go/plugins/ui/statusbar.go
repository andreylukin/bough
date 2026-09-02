package ui

// The one-line status bar: "bough · <model>" on the left; on the
// right the flash message, or the state — "waiting for you" while an
// ask is pending, the inspector hint, else the llm's running usage
// (cost when priced, tokens otherwise, nothing when unknown) — and
// always "? keys" as the way in to the keymap. The spinner shows only
// while a turn is in flight AND not blocked on the user. The session
// file is reachable via /sessions, not the bar.

import (
	"strings"

	"charm.land/lipgloss/v2"
)

func (m *model) statusBar(cfg *uiCfg) string {
	th := cfg.theme
	left := " " + cfg.status
	var right string
	switch {
	case m.flash != "":
		right = m.flash
	case m.inspecting:
		right = "inspecting · " + cfg.keys["history_inspect"] + " to close"
	case m.pendingAsk != "":
		right = "waiting for you"
	case m.scrollCue() != "":
		right = m.scrollCue()
	default:
		if cfg.usage != nil {
			right = cfg.usage.Usage().Short()
		}
	}
	if right != "" {
		right += " · "
	}
	right += "? keys"
	if m.running && m.pendingAsk == "" {
		right = m.spin.View() + " " + right
	}
	right += " "
	gap := m.width - lipgloss.Width(left) - lipgloss.Width(right)
	if gap < 1 {
		gap = 1
	}
	return th["status"].Width(m.width).Render(left + strings.Repeat(" ", gap) + right)
}
