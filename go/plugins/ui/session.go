package ui

// Session resume: transcript replay from the "history" service, and
// the session picker — pre-chat at launch, driven by the launcher's
// session seam ("sessions" + "session-picker" + "session-choose", see
// uiCfg), and mid-session from /sessions or a status-bar click, where
// the list is re-read from the history directory.

import (
	"github.com/charmbracelet/x/ansi"

	"fmt"
	"os"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

// pickerTitleWidth caps a session title in the picker, Claude-style.
const pickerTitleWidth = 60

// sessList is the mid-session picker's own list (nil = the launch
// picker, which reads cfg.sessions).
type sessList = []history.SessionInfo

// sessionID is the id of a history file path (base name sans .jsonl).
func sessionID(path string) string {
	return strings.TrimSuffix(filepath.Base(path), ".jsonl")
}

// replay synthesizes the transcript blocks for the history service's
// existing entries, exactly as the live session that wrote them did:
// input entries become the ❯ user line, everything else goes through
// addEvent so collapse defaults (and code de-dup) apply. A fresh
// session (no history, or no entries beyond the meta one) shows the
// welcome text instead; a model that already has blocks never replays
// (no double-render). A resumed transcript ends with a system row
// naming the session, its size and the last prompt, and lands on it.
func (m *model) replay() {
	cfg := m.cfg.Load()
	if len(m.blocks) > 0 {
		return
	}
	if cfg.hist == nil {
		m.welcome = true // fresh (no history service at all)
		m.noteLaunch(cfg)
		m.refresh()
		return
	}
	entries := cfg.hist.Entries()
	for _, e := range entries {
		text, _ := e.Data["text"].(string)
		switch e.Kind {
		case "meta", "undo":
			// session bookkeeping (cwd, a /undo's revert record —
			// its system row follows), nothing to render
		case "input":
			// What was TYPED, not the message that was sent: an
			// injected skill's whole SKILL.md is appended to the
			// latter, and replaying showed it as the prompt — which
			// then went into the composer on Up.
			steer, _ := e.Data["steer"].(bool)
			m.blocks = append(m.blocks, block{id: m.nextID, kind: "user", text: history.Prompt(e), steer: steer})
			m.nextID++
		case "ask":
			q, _ := e.Data["question"].(string)
			id, _ := e.Data["id"].(string)
			m.addEvent(Event{Kind: "ask", Text: q, ID: id, Options: strList(e.Data["options"])})
		case "ask/answer":
			id, _ := e.Data["id"].(string)
			for i := range m.blocks {
				if b := &m.blocks[i]; b.kind == "ask" && b.askID == id {
					b.answered, b.answer = true, text
				}
			}
			if m.pendingAsk == id {
				m.clearPendingAsk()
			}
		default:
			m.addEvent(Event{Kind: e.Kind, Text: text, Data: e.Data})
		}
	}
	m.expireAsks()                 // an ask with no answer entry replays as expired
	m.running = false              // a replayed transcript is never mid-turn
	m.welcome = len(m.blocks) == 0 // fresh session (0 entries): orient
	if len(m.blocks) > 0 {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "system", text: resumedLine(cfg.hist)})
		m.nextID++
	}
	m.noteLaunch(cfg)
	m.refresh()
	m.vp.GotoBottom()
}

// noteLaunch appends the launcher's notice (a stale dev binary) as an
// error row: it must be read, and a fresh session keeps its welcome
// text above it.
func (m *model) noteLaunch(cfg *uiCfg) {
	if cfg.notice == "" {
		return
	}
	m.blocks = append(m.blocks, block{id: m.nextID, kind: "error", text: cfg.notice})
	m.nextID++
	m.welcome = false
}

// resumedLine is the one-line system row a resumed transcript ends
// with: "resumed <id> · <n> entries · last: <first line of last prompt>".
func resumedLine(h historyView) string {
	entries := h.Entries()
	s := fmt.Sprintf("resumed %s · %d entries", sessionID(h.Path()), len(entries))
	if last := history.LastPrompt(entries); last != "" {
		s += " · last: " + truncateCols(last, pickerTitleWidth)
	}
	return s
}

// openPicker shows the picker mid-session: the list is re-read from
// the history directory (this directory's sessions first), the cursor
// on the newest.
func (m *model) openPicker() {
	m.picking = true
	m.pick = 0
	m.sessRows = m.listSessions()
	m.syncPalette()
}

// listSessions reads the session directory next to the current
// history file, this directory's sessions first. Never nil: a non-nil
// list is what marks the picker as mid-session (see leavePicker).
func (m *model) listSessions() sessList {
	rows := sessList{}
	h := m.cfg.Load().hist
	if h == nil {
		return rows
	}
	infos, err := history.List(filepath.Dir(h.Path()))
	if err != nil {
		return rows
	}
	cwd, _ := os.Getwd()
	return append(rows, history.PreferCwd(infos, cwd)...)
}

// pickerRows is the list the picker shows: its own mid-session list,
// else the launcher-provided one.
func (m *model) pickerRows(cfg *uiCfg) []history.SessionInfo {
	if m.sessRows != nil {
		return m.sessRows
	}
	return cfg.sessions
}

// currentID is the mounted session's id ("" without history).
func (m *model) currentID(cfg *uiCfg) string {
	if cfg.hist == nil {
		return ""
	}
	return sessionID(cfg.hist.Path())
}

// handlePickerKey drives the session picker: up/down move, enter
// resumes the selected session, esc starts a fresh one (at launch) or
// goes back to the chat (mid-session). The quit binding still works.
// Without a "session-choose" callback the list is read-only (the view
// says so loudly): enter does nothing and esc falls through to a
// fresh chat without choosing.
func (m model) handlePickerKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	cfg := m.cfg.Load()
	rows := m.pickerRows(cfg)
	key := msg.String()
	if cfg.action[key] == "quit" {
		if m.sessRows == nil {
			return m, tea.Quit // the launch chooser: nothing to go back to
		}
		return m.leavePicker(""), nil // in-session: back, like esc — never a one-press quit
	}
	switch key {
	case "up":
		if m.pick > 0 {
			m.pick--
		}
	case "down":
		if m.pick < len(rows)-1 {
			m.pick++
		}
	case "enter":
		if cfg.choose == nil || len(rows) == 0 {
			return m, nil
		}
		if m.pick >= len(rows) { // sessions swapped under us (hot reload)
			m.pick = 0
		}
		return m.leavePicker(rows[m.pick].ID), nil
	case "esc":
		return m.leavePicker(""), nil
	}
	return m, nil
}

// leavePicker enters the chat view. At launch (no own list) the choose
// seam is invoked once (id "" = fresh session) and the now-current
// history replays. Mid-session, id "" is "back" (nothing changes) and
// a different session swaps history through the seam and replays from
// scratch; the current session's id is a no-op resume.
func (m model) leavePicker(id string) model {
	cfg := m.cfg.Load()
	launch := m.sessRows == nil
	m.picking = false
	m.sessRows = nil
	if !launch && (id == "" || id == m.currentID(cfg)) {
		return m
	}
	if cfg.choose != nil {
		cfg.choose(id)
	}
	if id != "" {
		m.blocks = nil
		m.focusID = -1
		m.welcome = false
	}
	m.replay()
	return m
}

// resumeID is "/sessions <id>": resume that session directly, with an
// error block for an unknown id.
func (m *model) resumeID(id string) {
	found := false
	for _, s := range m.listSessions() {
		if s.ID == id {
			found = true
		}
	}
	if !found {
		m.blocks = append(m.blocks, block{id: m.nextID, kind: "error",
			text: fmt.Sprintf("no session %q (/sessions lists them)", id)})
		m.nextID++
		m.refresh()
		m.vp.GotoBottom()
		return
	}
	m.sessRows = sessList{} // mid-session semantics for leavePicker
	*m = m.leavePicker(id)
}

// pickerView renders the full-screen session list: this directory's
// sessions first, one row per session (local time, entry count,
// first-input title, working directory), the current session marked,
// the selected row in the focus style, key hints pinned to the bottom.
func (m *model) pickerView(cfg *uiCfg) string {
	th := cfg.theme
	rows := m.pickerRows(cfg)
	if m.pick >= len(rows) { // sessions swapped under us
		m.pick = 0
	}
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render("· resume a session"),
		"",
	}
	if cfg.choose == nil {
		lines = append(lines, th["error"].Render("✗ session-choose service missing — list is read-only"), "")
	}
	if len(rows) == 0 {
		lines = append(lines, th["dim"].Render("  (no sessions)"))
	}
	cur := m.currentID(cfg)
	cwd, _ := os.Getwd()
	home, _ := os.UserHomeDir()
	for i, s := range rows {
		marker, st := "  ", th["result"]
		if i == m.pick {
			marker, st = "▸ ", th["focus"]
		}
		row := fmt.Sprintf("%s%s  %3d entries  %s",
			marker, s.ModTime.Local().Format("2006-01-02 15:04"), s.Entries, truncateCols(s.Title, pickerTitleWidth))
		if s.ID == cur {
			row += " (current)"
		}
		row += "  " + shortDir(s.Cwd, cwd, home)
		if r := []rune(row); len(r) > m.width-1 && m.width > 2 {
			row = string(r[:m.width-2]) + "…"
		}
		lines = append(lines, st.Render(row))
	}
	hint := "↑/↓ select · enter resume · esc new session"
	if m.sessRows != nil {
		hint = "↑/↓ select · enter resume · esc back"
	}
	hints := th["dim"].Render(hint)
	for len(lines) < m.height-1 {
		lines = append(lines, "")
	}
	if m.height > 1 && len(lines) > m.height-1 {
		lines = lines[:m.height-1] // a short pane: the hint row still fits
	}
	lines = append(lines, hints)
	for i := range lines {
		lines[i] = ansi.Truncate(lines[i], m.width, "…")
	}
	return strings.Join(lines, "\n")
}

// shortDir renders a session's working directory: "." for this
// directory, "?" for a file predating the meta entry, ~-abbreviated
// otherwise.
func shortDir(dir, cwd, home string) string {
	switch {
	case dir == "":
		return "?"
	case dir == cwd:
		return "."
	case home != "" && strings.HasPrefix(dir, home+"/"):
		return "~" + strings.TrimPrefix(dir, home)
	}
	return dir
}

// truncateCols caps s at n runes, first line only, with an ellipsis.
func truncateCols(s string, n int) string {
	s = strings.SplitN(s, "\n", 2)[0]
	if r := []rune(s); len(r) > n {
		return string(r[:n-1]) + "…"
	}
	return s
}
