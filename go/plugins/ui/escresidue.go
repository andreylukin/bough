package ui

import (
	"time"

	tea "charm.land/bubbletea/v2"
)

// A slow link can split a mouse or focus report ("\x1b[<65;40;12M",
// "\x1b[I") across reads further apart than the terminal reader's 50 ms
// esc timeout. The reader then hands us a lone Esc followed by the rest
// as typed keys: the Esc clears the draft and the residue lands in the
// composer. Bubble Tea does not expose the timeout, so hold an Esc
// briefly, swallow what follows if it spells a report, and replay the
// keys unchanged otherwise.

const escHoldFor = 250 * time.Millisecond

// escHoldMsg is the hold's expiry; at is when the tick fired.
type escHoldMsg struct {
	gen int
	at  time.Time
}

// A tick delivered late means Update was stalled (a slow terminal blocks
// rendering) and the report's next bytes may be queued behind it on
// Bubble Tea's unbuffered channel: re-arm rather than release, up to
// escHoldMax from the Esc.
const (
	escLate    = 20 * time.Millisecond
	escHoldMax = time.Second
)

// isReportPrefix reports whether s (the keys after Esc) can still become
// a mouse (SGR) or focus report, and whether it already is a whole one.
func isReportPrefix(s string) (prefix, whole bool) {
	if s == "" || s[0] != '[' {
		return false, false
	}
	if len(s) == 1 {
		return true, false
	}
	if s[1] == 'I' || s[1] == 'O' {
		return len(s) == 2, len(s) == 2
	}
	if s[1] != '<' {
		return false, false
	}
	fields, digits := 0, 0
	for i := 2; i < len(s); i++ {
		switch c := s[i]; {
		case c >= '0' && c <= '9':
			digits++
		case c == ';' && digits > 0 && fields < 2:
			fields, digits = fields+1, 0
		case (c == 'M' || c == 'm') && digits > 0 && fields == 2 && i == len(s)-1:
			return true, true
		default:
			return false, false
		}
	}
	return true, false
}

// escFilter runs before handleKey. It returns true when the key was held
// or swallowed.
func (m *model) escFilter(msg tea.KeyPressMsg) (bool, tea.Cmd) {
	if m.escHold == nil {
		if msg.Code != tea.KeyEscape || msg.Mod != 0 {
			return false, nil
		}
		m.escHold = []tea.KeyPressMsg{msg}
		m.escSince = time.Now()
		return true, m.escTimer()
	}
	m.escHold = append(m.escHold, msg)
	var s string
	for _, k := range m.escHold[1:] {
		if k.Text == "" || k.Mod&^tea.ModShift != 0 {
			s = "\x00" // not a plain printable key: cannot be residue
			break
		}
		s += k.Text
	}
	prefix, whole := isReportPrefix(s)
	if whole {
		m.escHold = nil
		m.escGen++
		return true, nil
	}
	if prefix {
		return true, m.escTimer()
	}
	return true, m.escRelease()
}

func (m *model) escTimer() tea.Cmd {
	m.escGen++
	gen := m.escGen
	return tea.Tick(escHoldFor, func(at time.Time) tea.Msg { return escHoldMsg{gen, at} })
}

// escRelease replays the held keys through handleKey, in order.
func (m *model) escRelease() tea.Cmd {
	held := m.escHold
	m.escHold = nil
	m.escGen++
	var cmds []tea.Cmd
	var mm tea.Model = *m
	for _, k := range held {
		var c tea.Cmd
		mm, c = mm.(model).handleKey(k)
		cmds = append(cmds, c)
	}
	*m = mm.(model)
	return tea.Sequence(cmds...)
}
