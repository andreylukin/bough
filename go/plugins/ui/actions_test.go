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

func TestQuitActionRowQuitsOutright(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/quit")
	d.press(keyDown()) // past the /quit-less commands: the first row is the quit action
	rows := stripANSI(strings.Join(d.m.paletteRows(), "\n"))
	if !strings.Contains(rows, "> quit ctrl+c") {
		t.Fatalf("expected the quit action row selected:\n%s", rows)
	}
	// An explicit pick quits like /quit: the enter that accepts the
	// row would otherwise disarm the one-press arming it just did.
	if !hasQuit(d.press(keyEnter())) {
		t.Fatal("enter on the quit action row should quit")
	}
}

func TestTabOnActionRowKeepsDraft(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/expand_")
	d.press(keyTab())
	if got := d.m.input.Value(); got != "/expand_" {
		t.Errorf("tab on an action row writes no fake command, draft=%q", got)
	}
	d.event("result", nLines(10))
	d.press(keyEnter())
	if d.m.blocks[0].collapsed || len(d.sent) != 0 {
		t.Errorf("enter still runs the row (collapsed=%v sent=%v)", d.m.blocks[0].collapsed, d.sent)
	}
}

// --- actions mode keeps the draft ---

func TestActionPaletteRestoresDraftOnEsc(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("some long draft")
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	if !d.m.pal.actionsOnly || d.m.input.Value() != "/" {
		t.Fatalf("ctrl+x p opens actions mode over a \"/\" (actions=%v draft=%q)", d.m.pal.actionsOnly, d.m.input.Value())
	}
	d.typeStr("exp")
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if got := d.m.input.Value(); got != "some long draft" {
		t.Errorf("esc should give the displaced draft back, got %q", got)
	}
	if d.m.pal.open || d.m.pal.actionsOnly {
		t.Error("esc closes the actions mode")
	}
	d.typeStr("!")
	if got := d.m.input.Value(); got != "some long draft!" {
		t.Errorf("typing continues the restored draft, got %q", got)
	}
}

func TestActionPaletteRestoresDraftOnAccept(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(10))
	d.typeStr("keep me")
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	d.typeStr("expand_")
	d.press(keyEnter())
	if d.m.blocks[0].collapsed {
		t.Error("the accepted row should run")
	}
	if got := d.m.input.Value(); got != "keep me" {
		t.Errorf("an accepted row gives the draft back, got %q", got)
	}
	if d.m.pal.open || d.m.pal.actionsOnly || len(d.sent) != 0 {
		t.Errorf("nothing submitted, mode closed (open=%v actions=%v sent=%v)", d.m.pal.open, d.m.pal.actionsOnly, d.sent)
	}
}

func TestActionPaletteRestoresDraftWhenSlashErased(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("keep me")
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	d.press(tea.KeyPressMsg{Code: tea.KeyBackspace})
	if got := d.m.input.Value(); got != "keep me" {
		t.Errorf("erasing the mode's \"/\" gives the draft back, got %q", got)
	}
	if d.m.pal.open || d.m.pal.actionsOnly {
		t.Error("the mode is over once its \"/\" is gone")
	}
}

func TestActionPaletteReopensAfterEscOnSlash(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/")
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape}) // the palette stays shut on "/" until the draft changes
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	if !d.m.pal.open || !d.m.pal.actionsOnly || d.m.input.Value() != "/" {
		t.Errorf("ctrl+x p opens regardless of an earlier esc (open=%v actions=%v draft=%q)", d.m.pal.open, d.m.pal.actionsOnly, d.m.input.Value())
	}
}

func TestActionPaletteUnderInspectorFlashes(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, fakeHist{}))
	d.typeStr("keep me")
	d.press(keyCtrl('o'))
	if !d.m.inspecting {
		t.Fatal("precondition: ctrl+o opens the inspector")
	}
	d.press(keyCtrl('x'))
	d.press(keyRune('p'))
	if d.m.pal.open || d.m.pal.actionsOnly || d.m.input.Value() != "keep me" {
		t.Errorf("no palette under the inspector, and no stray \"/\" (open=%v actions=%v draft=%q)", d.m.pal.open, d.m.pal.actionsOnly, d.m.input.Value())
	}
	if !strings.Contains(d.plain(), "no palette under the inspector") {
		t.Errorf("the chord should say why it did nothing:\n%s", d.plain())
	}
}

func TestActionPaletteEnterOverNoRowsSubmitsNothing(t *testing.T) {
	t.Parallel()
	for _, cmds := range []commandsView{nil, reg(t, "alpha")} {
		d := defaultDrv(t)
		if cmds != nil {
			d = drvCmds(t, cmds)
		}
		d.press(keyCtrl('x'))
		d.press(keyRune('p'))
		d.typeStr("alpha") // no such action (a command of that name is not on this list)
		d.press(keyEnter())
		if len(d.sent) != 0 || len(d.m.blocks) != 0 {
			t.Errorf("enter over no action rows submits and dispatches nothing (sent=%v blocks=%d)", d.sent, len(d.m.blocks))
		}
		if !d.m.pal.open || !d.m.pal.actionsOnly || d.m.input.Value() != "/alpha" {
			t.Errorf("the mode stays for a retry (open=%v actions=%v draft=%q)", d.m.pal.open, d.m.pal.actionsOnly, d.m.input.Value())
		}
		if !strings.Contains(d.plain(), `no action matches "alpha"`) {
			t.Errorf("should flash the miss:\n%s", d.plain())
		}
	}
}

func TestClickClearsPendingLeader(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyCtrl('x'))
	d.feed(tea.MouseClickMsg{X: 1, Y: 1, Button: tea.MouseLeft})
	if d.m.leader {
		t.Fatal("a click should drop the pending leader")
	}
	d.press(keyRune('q'))
	if got := d.m.input.Value(); got != "q" {
		t.Errorf("the next key is typed, not a chord, got %q", got)
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

func TestKeymapRejectsDuplicateKey(t *testing.T) {
	t.Parallel()
	keys := defaultKeymap()
	err := applyKeymap(keys, map[string]string{"clear_input": "ctrl+x"})
	if err == nil || !strings.Contains(err.Error(), `key "ctrl+x" bound to both "clear_input" and "leader"`) {
		t.Errorf("one key on two actions should fail loud, got %v", err)
	}
	// A swap is not a collision.
	if err := applyKeymap(defaultKeymap(), map[string]string{"quit": "ctrl+l", "clear_input": "ctrl+c"}); err != nil {
		t.Errorf("swapping two keys should pass, got %v", err)
	}
}

func TestKeymapChordUnbinds(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"chord:l": ""}, nil))
	d.press(keyCtrl('x'))
	d.press(keyRune('l'))
	if d.m.picking {
		t.Error("an unbound chord should not run its old action")
	}
	if !strings.Contains(d.plain(), "ctrl+x l: no such chord") {
		t.Errorf("an unbound chord is unknown:\n%s", d.plain())
	}
	if strings.Contains(keysText(d.m.cfg.Load()), "ctrl+x l") {
		t.Error("/keys should not list the unbound chord")
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
