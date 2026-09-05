package ui

// Stopping things: the quit key (ctrl+c by default) cancels the turn
// in flight, and when idle arms a two-press quit; esc cancels a turn
// too (a non-empty composer clears first); ctrl+d on an idle, empty composer
// quits outright. The turn cancel goes through the loop's "cancel"
// service — the loop records and renders the "cancelled" row, the UI
// only reports that it asked.

import (
	"fmt"
	"path/filepath"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
)

// quitWindow is how long a first quit press stays armed.
const quitWindow = 3 * time.Second

const quitHint = "press ctrl+c again to quit"

// stopState is the quit-key arming.
type stopState struct {
	armedAt time.Time
	// escAt arms the second half of a double-esc, the way armedAt arms
	// the second ctrl+c.
	escAt time.Time
	now   func() time.Time // test seam; nil = time.Now
}

func (s *stopState) clock() time.Time {
	if s.now != nil {
		return s.now()
	}
	return time.Now()
}

// stopKey handles the quit key, esc and ctrl+d. It runs after the
// palette and a pending ask have had their say (they own esc), so
// every branch here is a real "stop this" intent. Reports whether the
// key was consumed.
func (m *model) stopKey(key string, cfg *uiCfg) (bool, tea.Cmd) {
	quit := cfg.action[key] == "quit"
	if !quit {
		m.stop.armedAt = time.Time{} // any other key disarms
	}
	if key != "esc" {
		m.stop.escAt = time.Time{} // ... and the same for the double-esc
	}
	switch {
	case quit:
		return true, m.quitPress(cfg.keys["quit"])
	case key == "esc" && !m.inspecting:
		return m.escPress()
	case key == "ctrl+d" && !m.running && m.input.Value() == "":
		return true, tea.Quit
	}
	return false, nil
}

// escWindow is how long a first esc stays armed for the second.
const escWindow = 3 * time.Second

const (
	escClearHint  = "press esc again to clear the draft"
	escRewindHint = "press esc again to rewind"
)

// escPress is one press of esc, following Claude Code: a single esc
// interrupts the turn in flight, and a DOUBLE esc either clears the
// draft — saved to history, so Up brings it back — or, on an empty
// composer, opens the rewind.
//
// bough used to clear the draft on a single esc and cancel only when
// the composer was empty. That protected a running turn from a stray
// esc, but it meant esc did two unrelated things depending on what you
// had typed, and it left no gesture for rewinding at all. The
// protection is still there in a better form: while a turn runs, esc
// only ever cancels, and clearing a draft now takes two presses.
//
// bough's rewind is /tree, which lists the turns and forks the session
// at one — the same job as Claude Code's rewind menu.
func (m *model) escPress() (bool, tea.Cmd) {
	if m.running {
		m.cancelTurn()
		m.stop.escAt = time.Time{}
		return true, nil
	}
	now := m.stop.clock()
	if m.stop.escAt.IsZero() || now.Sub(m.stop.escAt) > escWindow {
		// First press: arm, and say what a second one would do.
		m.stop.escAt = now
		if m.input.Value() != "" {
			m.flash = escClearHint
		} else {
			m.flash = escRewindHint
		}
		return true, nil
	}
	m.stop.escAt = time.Time{}
	if m.input.Value() == "" {
		// The rewind MENU, not a printed list: Claude Code opens a
		// list you walk with the arrows and pick from, and reading a
		// static dump of the turns is not the same thing.
		//
		// Enter in it forks the session, which needs /tree. A tree
		// built without a commands row has neither, and dispatching
		// into a nil registry panicked the whole ui — found by the
		// frame property test rather than by anyone pressing esc twice.
		if !m.hasCommand("tree") {
			m.flash = "no /tree command mounted: nothing to rewind to"
			return true, nil
		}
		m.flash = ""
		m.openRewind() // says why when there is nothing to show
		return true, nil
	}
	// A recalled prompt is already in history; only something typed is
	// worth saving back.
	if m.comp.recall < 0 {
		m.dropDraft(m.input.Value()) // Up brings it back
	}
	m.comp.recall = -1
	m.input.Reset()
	m.syncPalette()
	m.layoutComposer()
	m.flash = "draft cleared · ↑ brings it back"
	return true, nil
}

// quitPress is one press of the quit key (or the quit chord, or the
// palette's quit row): it cancels a running turn; idle, a first press
// arms and a second within quitWindow quits. label names the key in
// the arming hint.
func (m *model) quitPress(label string) tea.Cmd {
	if m.running {
		m.cancelTurn()
		return nil
	}
	now := m.stop.clock()
	if !m.stop.armedAt.IsZero() && now.Sub(m.stop.armedAt) <= quitWindow {
		return tea.Quit
	}
	m.stop.armedAt = now
	m.flash = strings.Replace(quitHint, "ctrl+c", label, 1)
	return nil
}

// cancelTurn asks the loop to abort the turn in flight. The spinner
// keeps going until the loop's cancelled/done events land.
func (m *model) cancelTurn() {
	c := m.cfg.Load().cancel
	if c == nil {
		m.flash = "no cancel service mounted"
		return
	}
	c()
	m.flash = "cancelling…"
}

// exitLine is the one line printed after the TUI closes: how to get
// back into this session. Empty when there is no session file.
func exitLine(h historyView) string {
	if h == nil || h.Path() == "" {
		return ""
	}
	id := strings.TrimSuffix(filepath.Base(h.Path()), ".jsonl")
	return fmt.Sprintf("session %s · resume with: bough -r %s", id, id)
}

// hasCommand reports whether a command of that name is registered.
func (m *model) hasCommand(name string) bool {
	cmds := m.cfg.Load().cmds
	if cmds == nil {
		return false
	}
	for _, in := range cmds.List() {
		if in.Name == name {
			return true
		}
	}
	return false
}
