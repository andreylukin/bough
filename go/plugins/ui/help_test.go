package ui

// /help and /keys as a transient panel over the transcript.

import (
	"fmt"
	"strings"
	"testing"
	"testing/synctest"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/commands"
)

// /help opens a scrollable panel, built-ins first; esc closes it and
// nothing reaches the transcript or history.
func TestHelpPanel(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		r := reg(t, "help", "model", "keys")
		for i := range 40 {
			if err := r.Register(commands.CommandInfo{Name: fmt.Sprintf("sk%02d", i), Kind: "skill"},
				func(string) (string, error) { return "", nil }); err != nil {
				t.Fatal(err)
			}
		}
		h := &recordingHist{}
		cfg := cfgWith(t, nil, nil, nil)
		cfg.cmds, cfg.hlog = r, h
		d := newDrv(t, 100, 24, cfg)
		d.typeStr("/help")
		d.press(keyEnter())
		rows := d.m.keysRows()
		if !d.m.helpOpen || len(rows) < 2 || !strings.HasPrefix(rows[1], "/help") {
			t.Fatalf("/help should open the panel with built-ins first: %q", rows)
		}
		if !strings.Contains(rows[len(rows)-1], "more") {
			t.Errorf("a long list should end in a more row: %q", rows)
		}
		d.press(tea.KeyPressMsg{Code: tea.KeyPgDown})
		if d.m.panelTop == 0 || !d.m.helpOpen {
			t.Errorf("pgdown should scroll the open panel, top=%d", d.m.panelTop)
		}
		d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
		if d.m.helpOpen || len(d.m.blocks) != 0 || len(h.appended) != 0 {
			t.Errorf("esc should close it leaving no trace: blocks=%v hist=%v", d.m.blocks, h.appended)
		}
	})
}

// The quit key arms on the first press: /keys says so.
func TestKeysQuitNeedsTwoPresses(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	text := keysText(d.m.cfg.Load())
	if !strings.Contains(text, "ctrl+c ×2") || !strings.Contains(text, "quit (esc cancels a turn)") {
		t.Errorf("/keys should say ctrl+c ×2:\n%s", text)
	}
}
