package ui

// The terminal's title: what a browser tab (bough web renders the TUI
// in the page, and the page takes the terminal's title), a tmux window
// or an iTerm tab shows for this session. The cmux plugin puts the
// same facts on the cmux sidebar row; here they have one line, so the
// state is a leading glyph and the rest is the session's name:
//
//	● fix the flaky test     a turn is running
//	? fix the flaky test     tools.ask is waiting on you
//	✓ fix the flaky test     the turn finished
//	■ fix the flaky test     you stopped it
//	fix the flaky test       nothing has run yet
//
// The name is the session title once there is one, else the last
// prompt's first line, else "bough". View carries it; bubbletea sends
// the sequence only when it changes.

// tabTitle is the title the terminal should show now.
func (m *model) tabTitle() string {
	name := m.title
	if name == "" {
		for i := len(m.blocks) - 1; i >= 0 && name == ""; i-- {
			if m.blocks[i].kind == "user" {
				name = firstLine(m.blocks[i].text)
			}
		}
	}
	if name == "" {
		name = "bough"
	}
	switch {
	case m.pendingAsk != "":
		return "? " + name
	case m.running:
		return "● " + name
	case m.lastEnd == "cancelled":
		return "■ " + name
	case m.lastEnd == "done":
		return "✓ " + name
	}
	return name
}
