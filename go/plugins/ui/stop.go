package ui

// Stopping things: the quit key (ctrl+c by default) cancels the turn
// in flight, and when idle arms a two-press quit; esc cancels a turn
// too (or clears the composer); ctrl+d on an idle, empty composer
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
const quitWindow = 1500 * time.Millisecond

const quitHint = "press ctrl+c again to quit"

// stopState is the quit-key arming.
type stopState struct {
	armedAt time.Time
	now     func() time.Time // test seam; nil = time.Now
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
	switch {
	case quit && m.running:
		m.cancelTurn()
		return true, nil
	case quit:
		now := m.stop.clock()
		if !m.stop.armedAt.IsZero() && now.Sub(m.stop.armedAt) <= quitWindow {
			return true, tea.Quit
		}
		m.stop.armedAt = now
		m.flash = strings.Replace(quitHint, "ctrl+c", cfg.keys["quit"], 1)
		return true, nil
	case key == "esc" && m.running && !m.inspecting:
		m.cancelTurn()
		return true, nil
	case key == "esc" && !m.inspecting && m.input.Value() != "":
		m.input.Reset()
		m.syncPalette()
		m.layoutComposer()
		return true, nil
	case key == "ctrl+d" && !m.running && m.input.Value() == "":
		return true, tea.Quit
	}
	return false, nil
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
