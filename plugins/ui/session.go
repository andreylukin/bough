package ui

// Session resume: transcript replay from the "history" service, and
// the pre-chat session picker driven by the launcher's session seam
// ("sessions" + "session-picker" + "session-choose", see uiCfg).

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
)

// pickerTitleWidth caps a session title in the picker, Claude-style.
const pickerTitleWidth = 60

// replay synthesizes the transcript blocks for the history service's
// existing entries, exactly as the live session that wrote them did:
// input entries become the ❯ user line, everything else goes through
// addEvent so collapse defaults (and code de-dup) apply. A fresh
// session (no history, or no entries) is a no-op; a model that already
// has blocks never replays (no double-render).
func (m *model) replay() {
	cfg := m.cfg.Load()
	if cfg.hist == nil || len(m.blocks) > 0 {
		return
	}
	for _, e := range cfg.hist.Entries() {
		text, _ := e.Data["text"].(string)
		if e.Kind == "input" {
			m.blocks = append(m.blocks, block{id: m.nextID, kind: "user", text: text})
			m.nextID++
			continue
		}
		m.addEvent(Event{Kind: e.Kind, Text: text})
	}
	m.running = false // a replayed transcript is never mid-turn
	m.refresh()
	m.vp.GotoBottom()
}

// handlePickerKey drives the session picker: up/down move, enter
// resumes the selected session, esc starts a fresh one. The quit
// binding still works. Without a "session-choose" callback the list is
// read-only (the view says so loudly): enter does nothing and esc
// falls through to a fresh chat without choosing.
func (m model) handlePickerKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	cfg := m.cfg.Load()
	key := msg.String()
	if cfg.action[key] == "quit" {
		return m, tea.Quit
	}
	switch key {
	case "up":
		if m.pick > 0 {
			m.pick--
		}
	case "down":
		if m.pick < len(cfg.sessions)-1 {
			m.pick++
		}
	case "enter":
		if cfg.choose == nil || len(cfg.sessions) == 0 {
			return m, nil
		}
		if m.pick >= len(cfg.sessions) { // sessions swapped under us (hot reload)
			m.pick = 0
		}
		return m.leavePicker(cfg.sessions[m.pick].ID), nil
	case "esc":
		return m.leavePicker(""), nil
	}
	return m, nil
}

// leavePicker invokes the choose seam once (id "" = fresh session) and
// enters the chat view, replaying whatever the now-current history
// service holds. choose is synchronous: the launcher has swapped the
// "history" service before it returns.
func (m model) leavePicker(id string) model {
	if choose := m.cfg.Load().choose; choose != nil {
		choose(id)
	}
	m.picking = false
	m.replay()
	return m
}

// pickerView renders the full-screen session list: newest first, one
// row per session (local time, entry count, first-input title), the
// selected row in the focus style, key hints pinned to the bottom.
func (m *model) pickerView(cfg *uiCfg) string {
	th := cfg.theme
	if m.pick >= len(cfg.sessions) { // sessions swapped under us
		m.pick = 0
	}
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render("· resume a session"),
		"",
	}
	if cfg.choose == nil {
		lines = append(lines, th["error"].Render("✗ session-choose service missing — list is read-only"), "")
	}
	if len(cfg.sessions) == 0 {
		lines = append(lines, th["dim"].Render("  (no sessions)"))
	}
	for i, s := range cfg.sessions {
		marker, st := "  ", th["result"]
		if i == m.pick {
			marker, st = "▸ ", th["focus"]
		}
		row := fmt.Sprintf("%s%s  %3d entries  %s",
			marker, s.ModTime.Local().Format("2006-01-02 15:04"), s.Entries, truncateCols(s.Title, pickerTitleWidth))
		if r := []rune(row); len(r) > m.width-1 && m.width > 2 {
			row = string(r[:m.width-2]) + "…"
		}
		lines = append(lines, st.Render(row))
	}
	hints := th["dim"].Render("↑/↓ select · enter resume · esc new session")
	for len(lines) < m.height-1 {
		lines = append(lines, "")
	}
	return strings.Join(append(lines, hints), "\n")
}

// truncateCols caps s at n runes, first line only, with an ellipsis.
func truncateCols(s string, n int) string {
	s = strings.SplitN(s, "\n", 2)[0]
	if r := []rune(s); len(r) > n {
		return string(r[:n-1]) + "…"
	}
	return s
}
