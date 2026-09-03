package ui

// The one-line status bar: "bough · <model>" on the left; on the
// right the flash message, or the state — "waiting for you" while an
// ask is pending, the inspector hint, else the llm's running usage
// (cost when priced, tokens otherwise, nothing when unknown) — and
// always "? keys" as the way in to the keymap. The spinner shows only
// while a turn is in flight AND not blocked on the user. The session
// file is reachable via /sessions, not the bar.

import (
	"github.com/charmbracelet/x/ansi"

	"fmt"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
)

func (m *model) statusBar(cfg *uiCfg) string {
	th := cfg.theme
	left := " " + cfg.status
	var right string
	switch {
	case m.flash != "":
		right = m.flash
	case m.inspecting && m.diving != 0:
		right = "subagent transcript · esc to close"
	case m.inspecting:
		right = "inspecting · " + cfg.keys["history_inspect"] + " to close"
	case m.pendingAsk != "":
		right = "waiting for you"
	case m.focusedSpawn() >= 0:
		right = "subagent card · enter folds · " + cfg.keys["history_inspect"] + " opens its transcript"
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
		right = m.spin.View() + " " + m.elapsed() + " · " + right
	}
	right += " "
	gap := m.width - lipgloss.Width(left) - lipgloss.Width(right)
	if gap < 1 && m.flash == "" {
		// Narrow pane: the usage/scroll cue goes before "? keys" does.
		right = "? keys "
		if m.running && m.pendingAsk == "" {
			right = m.spin.View() + " " + right
		}
		gap = m.width - lipgloss.Width(left) - lipgloss.Width(right)
	}
	if gap < 1 {
		gap = 1
	}
	// One row, always: a narrow pane truncates rather than wrapping the
	// bar onto a second row and pushing the composer off screen.
	line := ansi.Truncate(left+strings.Repeat(" ", gap)+right, m.width, "…")
	return th["status"].Width(m.width).Render(line)
}

// elapsed is the in-flight turn's age, whole seconds ("12s", "2m05s"),
// redrawn on every spinner tick.
func (m *model) elapsed() string {
	if m.turnStart.IsZero() {
		return "0s"
	}
	d := time.Since(m.turnStart).Round(time.Second)
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
}
