package ui

// The action palette (action rows in "/"), the leader key and its
// chords, and the keymap's chord validation.

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/andreylukin/bough/plugins/commands"
)

func keyRune(r rune) tea.KeyPressMsg { return tea.KeyPressMsg{Code: r, Text: string(r)} }

// --- palette action rows ---

func TestPaletteListsActionRowsWithKeys(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/")
	rows := stripANSI(strings.Join(paletteLines(paletteFilter(d.m.paletteItems(), ""), 0, 200, 100, d.m.cfg.Load().theme), "\n"))
	for _, want := range []string{"quit ctrl+c", "history_inspect ctrl+o", "collapse_all ctrl+x c", "sessions ctrl+x l", "action · "} {
		if !strings.Contains(rows, want) {
			t.Errorf("action rows should show %q:\n%s", want, rows)
		}
	}
	// An unbound action still lists, with an empty key column.
	if !strings.Contains(rows, "block_next tab") || !strings.Contains(rows, "scroll_up up") {
		t.Errorf("every keymap action should be a row:\n%s", rows)
	}
	if i, j := strings.Index(rows, "/alpha"), strings.Index(rows, "quit ctrl+c"); i < 0 || j < i {
		t.Errorf("commands rank above the action rows:\n%s", rows)
	}
}

func TestPaletteFiltersActionRows(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/expand_")
	p := d.plain()
	if !strings.Contains(p, "expand_all") || strings.Contains(p, "collapse_all") || strings.Contains(p, "/alpha") {
		t.Errorf("the query should narrow the action rows like commands:\n%s", p)
	}
}

func TestEnterOnActionRowRunsItAndSubmitsNothing(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.event("result", nLines(10))
	if !d.m.blocks[0].collapsed {
		t.Fatal("precondition: result starts collapsed")
	}
	d.typeStr("/expand_all")
	d.press(keyEnter())
	if d.m.blocks[0].collapsed {
		t.Error("enter on the expand_all row should expand the block")
	}
	if len(d.sent) != 0 {
		t.Errorf("an action row submits nothing, sent=%v", d.sent)
	}
	if d.m.input.Value() != "" || d.m.pal.open {
		t.Errorf("accepting an action drops the query and closes the palette (draft=%q open=%v)", d.m.input.Value(), d.m.pal.open)
	}
	if len(d.m.blocks) != 1 {
		t.Errorf("an action is not a dispatch: no command/system blocks, got %d blocks", len(d.m.blocks))
	}
}

func TestClickOnActionRowRunsIt(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.event("result", nLines(10))
	d.typeStr("/expand_all")
	top := d.m.vp.Height() - 1 // one row
	d.feed(tea.MouseClickMsg{X: 0, Y: top, Button: tea.MouseLeft})
	if d.m.blocks[0].collapsed || len(d.m.blocks) != 1 {
		t.Errorf("a click on the action row should run it, blocks=%+v", d.m.blocks)
	}
}

func TestActionRowsAbsentMidText(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("look at /a")
	if p := d.plain(); strings.Contains(p, "action · ") {
		t.Errorf("mid-text the palette completes a word: no action rows\n%s", p)
	}
}

func TestQuitActionRowArmsLikeTheKey(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/quit")
	d.press(keyDown()) // past the /quit-less commands: the first row is the quit action
	rows := stripANSI(strings.Join(d.m.paletteRows(), "\n"))
	if !strings.Contains(rows, "> quit ctrl+c") {
		t.Fatalf("expected the quit action row selected:\n%s", rows)
	}
	if hasQuit(d.press(keyEnter())) {
		t.Fatal("the quit action arms, never quits on one press")
	}
	if !strings.Contains(d.plain(), quitHint) {
		t.Errorf("arming should show %q:\n%s", quitHint, d.plain())
	}
	if !hasQuit(d.press(keyCtrl('c'))) {
		t.Error("ctrl+c after the armed action row should quit")
	}
}

// --- leader + chords ---

func TestLeaderPendingShowsInStatusBar(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyCtrl('x'))
	if !d.m.leader {
		t.Fatal("ctrl+x should leave a chord pending")
	}
	if !strings.Contains(d.plain(), "ctrl+x …") {
		t.Errorf("the status bar should show the pending leader:\n%s", d.plain())
	}
}

func TestChordRunsAction(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(10))
	d.press(keyCtrl('x'))
	d.press(keyRune('e'))
	if d.m.blocks[0].collapsed {
		t.Error("ctrl+x e should expand all")
	}
	if d.m.leader || d.m.input.Value() != "" {
		t.Errorf("the chord key is consumed (leader=%v draft=%q)", d.m.leader, d.m.input.Value())
	}
	d.press(keyCtrl('x'))
	d.press(keyRune('c'))
	if !d.m.blocks[0].collapsed {
		t.Error("ctrl+x c should collapse all")
	}
	d.press(keyCtrl('x'))
	d.press(keyRune('k'))
	if last := d.m.blocks[len(d.m.blocks)-1]; last.kind != "system" || !strings.HasPrefix(last.text, "keys\n") {
		t.Errorf("ctrl+x k should print the keys block, got %+v", last)
	}
}

func TestChordOpensSessionPicker(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyCtrl('x'))
	d.press(keyRune('l'))
	if !d.m.picking {
		t.Error("ctrl+x l should open the session picker")
	}
}

func TestUnknownChordFlashes(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyCtrl('x'))
	d.press(keyRune('z'))
	if d.m.leader {
		t.Error("an unknown chord clears the pending leader")
	}
	if !strings.Contains(d.plain(), "ctrl+x z: no such chord") {
		t.Errorf("an unknown chord should flash:\n%s", d.plain())
	}
	if d.m.input.Value() != "" {
		t.Errorf("the chord key never lands in the composer, got %q", d.m.input.Value())
	}
}

func TestChordQuitArmsLikeCtrlC(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyCtrl('x'))
	if hasQuit(d.press(keyRune('q'))) {
		t.Fatal("ctrl+x q once should only arm the quit")
	}
	if !strings.Contains(d.plain(), "press ctrl+x q again to quit") {
		t.Errorf("hint should name the chord:\n%s", d.plain())
	}
	d.press(keyCtrl('x'))
	if !hasQuit(d.press(keyRune('q'))) {
		t.Error("ctrl+x q twice should quit")
	}
}

func TestChordUndoWithoutCommandFlashes(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.press(keyCtrl('x'))
	d.press(keyRune('u'))
	if !strings.Contains(d.plain(), "no /undo command") {
		t.Errorf("no undo command: the chord should say so:\n%s", d.plain())
	}
	d2 := drvCmds(t, reg(t, "undo"))
	d2.press(keyCtrl('x'))
	d2.press(keyRune('u'))
	if last := d2.m.blocks[len(d2.m.blocks)-1]; last.kind != "system" || last.text != "undo ran" {
		t.Errorf("ctrl+x u should dispatch /undo, got %+v", last)
	}
}

func TestChordOpensActionPalette(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	if !d.m.pal.open || !d.m.pal.actionsOnly {
		t.Fatalf("ctrl+x p should open the palette in actions mode (open=%v actions=%v)", d.m.pal.open, d.m.pal.actionsOnly)
	}
	rows := stripANSI(strings.Join(paletteLines(paletteFilter(d.m.paletteItems(), ""), 0, 200, 100, d.m.cfg.Load().theme), "\n"))
	if strings.Contains(rows, "/alpha") || !strings.Contains(rows, "collapse_all") {
		t.Errorf("actions mode lists the action rows alone:\n%s", rows)
	}
	d.typeStr("expand_")
	d.press(keyEnter())
	if d.m.pal.open || d.m.pal.actionsOnly || d.m.input.Value() != "" {
		t.Errorf("enter runs the row and closes the mode (open=%v actions=%v draft=%q)", d.m.pal.open, d.m.pal.actionsOnly, d.m.input.Value())
	}
	if !strings.Contains(d.plain(), "expanded") && !strings.Contains(d.plain(), "nothing to expand") {
		t.Errorf("the action should have run (flash):\n%s", d.plain())
	}
}

func TestChordWorksWithoutCommandsService(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t) // no commands: "/" is plain text, but ctrl+x p still opens the action rows
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	if !d.m.pal.open {
		t.Fatal("ctrl+x p should open the action palette without a commands service")
	}
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if d.m.pal.open || d.m.pal.actionsOnly {
		t.Error("esc closes the action palette and its mode")
	}
}

// --- keymap config ---

func TestKeymapChordsConfigurable(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"leader": "ctrl+g", "chord:x": "expand_all"}, nil))
	d.event("result", nLines(10))
	d.press(keyCtrl('g'))
	d.press(keyRune('x'))
	if d.m.blocks[0].collapsed {
		t.Error("a configured leader + chord should run its action")
	}
	d.press(keyCtrl('x')) // the old leader is now plain
	if d.m.leader {
		t.Error("ctrl+x is not the leader after rebinding")
	}
}

func TestKeymapRejectsBadChordTarget(t *testing.T) {
	t.Parallel()
	keys := defaultKeymap()
	err := applyKeymap(keys, map[string]string{"chord:z": "warp_core"})
	if err == nil || !strings.Contains(err.Error(), `unknown action "warp_core"`) {
		t.Errorf("a chord to an unknown action should fail loud, got %v", err)
	}
	if err := applyKeymap(keys, map[string]string{"chord:": "quit"}); err == nil {
		t.Error("an empty chord key should fail loud")
	}
}

func TestKeysListsChords(t *testing.T) {
	t.Parallel()
	r := commands.NewRegistry()
	if err := r.Register(commands.CommandInfo{Name: "keys"},
		func(string) (string, error) { return "", commands.ActionKeys }); err != nil {
		t.Fatal(err)
	}
	d := drvCmds(t, r)
	d.typeStr("/keys")
	d.press(keyEnter())
	text := d.m.blocks[len(d.m.blocks)-1].text
	for _, want := range []string{"chords (ctrl+x, then a key)", "ctrl+x l", "pick a session to resume", "ctrl+x q", "ctrl+x p"} {
		if !strings.Contains(text, want) {
			t.Errorf("/keys should list the chords with %q:\n%s", want, text)
		}
	}
}
