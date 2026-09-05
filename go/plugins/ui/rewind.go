package ui

// The rewind picker: double esc on an empty composer opens a list of
// this session's turns, newest last, and Enter forks the session at
// one. Claude Code's Esc+Esc opens the same menu; bough printed a
// static list instead, which you could read but not walk.
//
// Picking a row goes back to the point BEFORE that prompt, which is
// what Claude Code's menu offers and the only reading that makes the
// labels mean anything: to undo "update the readme" you pick "update
// the readme". history.Fork keeps the turn you name, so the row for
// turn i forks at turn i-1; the first row has no earlier turn, and the
// point before it is a fresh session (/new).
//
// It moves the CONVERSATION only. Putting files back is /undo, one
// turn at a time, so each row says what its turn wrote rather than
// implying the code travels with it.

import (
	"fmt"
	"strconv"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/history"
)

// rewindRow is one turn on the menu.
type rewindRow struct {
	seq   int64
	text  string   // the typed prompt, first line
	files []string // what the turn wrote
}

// rewindPicker is the open menu; pick indexes rows, and rows+1 (the
// last position) is "(current)", which changes nothing.
type rewindPicker struct {
	open bool
	pick int
	rows []rewindRow
}

// rewindTurns folds history entries into the rows the menu shows: an
// "input" entry opens a turn and the next "done" closes it, carrying
// the files that turn wrote.
//
// A narrower fold than the one /tree uses, because this only has to
// DISPLAY turns — no checkpoints, no undo bookkeeping.
func rewindTurns(entries []history.Entry) []rewindRow {
	var rows []rewindRow
	for _, e := range entries {
		switch e.Kind {
		case "input":
			text := strings.TrimSpace(history.Prompt(e))
			if text == "" || strings.HasPrefix(text, "[background job] ") {
				continue // nobody typed that one
			}
			rows = append(rows, rewindRow{seq: e.Seq, text: firstLine(text)})
		case "done":
			if len(rows) > 0 {
				rows[len(rows)-1].files = strList(e.Data["files"])
			}
		}
	}
	return rows
}

// firstLine is text up to its first newline.
func firstLine(text string) string {
	if i := strings.IndexByte(text, '\n'); i >= 0 {
		return strings.TrimSpace(text[:i])
	}
	return text
}

// openRewind shows the menu with the cursor on "(current)", the way
// Claude Code opens it: at the present, walking back from there.
func (m *model) openRewind() bool {
	cfg := m.cfg.Load()
	if cfg.hist == nil {
		m.flash = "no history in this session: nothing to rewind to"
		return false
	}
	rows := rewindTurns(cfg.hist.Entries())
	if len(rows) == 0 {
		m.flash = "no turns yet: nothing to rewind to"
		return false
	}
	m.rw = rewindPicker{open: true, rows: rows, pick: len(rows)}
	return true
}

// handleRewindKey drives the menu. The quit binding backs out rather
// than quitting, like the other pickers.
func (m model) handleRewindKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	key := msg.String()
	if m.cfg.Load().action[key] == "quit" {
		key = "esc"
	}
	switch key {
	case "up", "ctrl+p":
		if m.rw.pick > 0 {
			m.rw.pick--
		}
	case "down", "ctrl+n":
		if m.rw.pick < len(m.rw.rows) {
			m.rw.pick++
		}
	case "enter":
		pick, rows := m.rw.pick, m.rw.rows
		m.rw = rewindPicker{}
		switch {
		case pick >= len(rows):
			return m, nil // "(current)": nothing to do
		case pick == 0:
			// Before the first prompt is a session with no turns.
			return m, m.dispatch("/new")
		default:
			return m, m.dispatch("/tree " + strconv.FormatInt(rows[pick-1].seq, 10))
		}
	case "esc":
		m.rw = rewindPicker{}
	}
	return m, nil
}

// rewindView renders the menu: the turns oldest-first with "(current)"
// last, the cursor row in the focus style, and a count of what is
// scrolled off the top.
func (m *model) rewindView(cfg *uiCfg) string {
	th := cfg.theme
	lines := []string{
		th["accent"].Render("bough") + " " + th["dim"].Render("· rewind"),
		"",
		th["dim"].Render("Go back to the conversation as it was before…"),
		"",
	}
	// Two lines per row plus the header, the trailer and "(current)".
	room := max((m.height-9)/2, 3)
	first := 0
	if len(m.rows())-room > 0 && m.rw.pick > room-1 {
		first = min(m.rw.pick-room+1, len(m.rows())-room)
	}
	if first > 0 {
		lines = append(lines, th["dim"].Render(fmt.Sprintf("  ↑ %d more above", first)))
	}
	for i := first; i < len(m.rows()) && i-first < room; i++ {
		lines = append(lines, m.rewindRowLines(i, th)...)
	}
	lines = append(lines, "", th["dim"].Render("enter goes back to before this prompt · esc cancels"))
	return strings.Join(lines, "\n")
}

// rows is the menu's rows plus the virtual "(current)" at the end.
func (m *model) rows() []rewindRow {
	return append(m.rw.rows, rewindRow{seq: -1, text: "(current)"})
}

// rewindRowLines renders one row: the prompt, then what its turn wrote.
func (m *model) rewindRowLines(i int, th theme) []string {
	rows := m.rows()
	r := rows[i]
	style, marker := th["dim"], "  "
	if i == m.rw.pick {
		style, marker = th["focus"], th["accent"].Render("❯ ")
	}
	text := r.text
	if w := m.width - 6; w > 10 && len([]rune(text)) > w {
		text = string([]rune(text)[:w-1]) + "…"
	}
	if r.seq < 0 {
		return []string{marker + style.Render(text)}
	}
	note := "no files written"
	switch n := len(r.files); {
	case n == 1:
		note = "wrote " + r.files[0]
	case n > 1:
		note = fmt.Sprintf("wrote %d files", n)
	}
	return []string{marker + style.Render(text), "    " + th["dim"].Render(note)}
}
