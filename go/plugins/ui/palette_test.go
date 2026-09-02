package ui

// The "/" palette: open/close, the three-tier filter and its
// stability, key handling, overlay drawing, dispatch through the
// commands service, UIAction effects, and history recording.

import (
	"fmt"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/exp/teatest/v2"

	"github.com/andreylukin/bough/plugins/commands"
	"github.com/andreylukin/bough/plugins/history"
)

// reg builds a real registry whose commands answer "<name> ran <args>".
func reg(t *testing.T, names ...string) *commands.Registry {
	t.Helper()
	r := commands.NewRegistry()
	for _, n := range names {
		n := n
		err := r.Register(commands.CommandInfo{Name: n, Usage: "", Summary: "do " + n},
			func(args string) (string, error) {
				return strings.TrimSpace(n + " ran " + args), nil
			})
		if err != nil {
			t.Fatal(err)
		}
	}
	return r
}

// drvCmds is a driver with a commands service mounted.
func drvCmds(t *testing.T, r commandsView) *drv {
	t.Helper()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = r
	return newDrv(t, 80, 24, cfg)
}

func items(names ...string) []paletteItem {
	out := make([]paletteItem, len(names))
	for i, n := range names {
		out[i] = paletteItem{name: n, usage: "/" + n, summary: "do " + n}
	}
	return out
}

func names(items []paletteItem) []string {
	out := make([]string, len(items))
	for i, it := range items {
		out[i] = it.name
	}
	return out
}

func eq(t *testing.T, got, want []string, why string) {
	t.Helper()
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Errorf("%s: got %v, want %v", why, got, want)
	}
}

// --- open/close ---

func TestSlashAtLineStartOpensPalette(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha", "beta"))
	d.typeStr("/")
	if !d.m.pal.open {
		t.Fatal("a / at line start should open the palette")
	}
	p := d.plain()
	if !strings.Contains(p, "/alpha") || !strings.Contains(p, "/beta") {
		t.Errorf("open palette should list every command:\n%s", p)
	}
}

func TestSlashMidLineDoesNotOpen(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("a/")
	if d.m.pal.open {
		t.Error("a / not at line start must not open the palette")
	}
}

func TestSlashAtWordStartOpensAndCompletesInPlace(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha", "beta"))
	d.typeStr("look at /al")
	if !d.m.pal.open {
		t.Fatal("a / at word start should open the palette")
	}
	p := d.plain()
	if !strings.Contains(p, "/alpha") || strings.Contains(p, "/beta") {
		t.Errorf("mid-text palette should filter on the word:\n%s", p)
	}
	d.press(keyEnter())
	if got := d.m.input.Value(); got != "look at /alpha " {
		t.Fatalf("enter mid-text should complete in place, got %q", got)
	}
	if d.m.pal.open {
		t.Error("completing mid-text should close the palette")
	}
	if len(d.sent) != 0 {
		t.Errorf("nothing should be submitted, sent=%v", d.sent)
	}
}

func TestSlashWithoutCommandsServiceIsPlainText(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t) // no commands service
	d.typeStr("/help")
	if d.m.pal.open {
		t.Fatal("no commands service: the palette must not open")
	}
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != "/help" {
		t.Errorf("no commands service: / lines go to the loop, sent=%v", d.sent)
	}
}

func TestBackspacePastSlashCloses(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/a")
	d.feed(tea.KeyPressMsg{Code: tea.KeyBackspace})
	if !d.m.pal.open {
		t.Fatal("backspacing back to / should keep the palette open")
	}
	d.feed(tea.KeyPressMsg{Code: tea.KeyBackspace})
	if d.m.pal.open {
		t.Error("backspacing past the / should close the palette")
	}
}

func TestEscClosesAndTypingReopens(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/")
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if d.m.pal.open {
		t.Fatal("esc should close the palette")
	}
	if d.m.input.Value() != "" {
		t.Fatalf("esc on a lone /query should clear the composer, got %q", d.m.input.Value())
	}
	d.typeStr("/a")
	if !d.m.pal.open {
		t.Error("typing after esc should reopen the palette")
	}
	// A draft with more than the /word keeps its text.
	d.typeStr("lpha now")
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if got := d.m.input.Value(); got != "/alpha now" {
		t.Errorf("esc must not drop a draft with args, got %q", got)
	}
}

// --- ordering: builtins before skills, /help first on a bare "/" ---

func TestFilterBuiltinsBeforeSkillsHelpFirst(t *testing.T) {
	t.Parallel()
	all := []paletteItem{
		{name: "zeta", skill: true}, {name: "aardvark", skill: true},
		{name: "quit"}, {name: "help"}, {name: "clear"},
	}
	eq(t, names(paletteFilter(all, "")), []string{"help", "clear", "quit", "aardvark", "zeta"},
		"bare /: help pinned, builtins alphabetical, skills last")
	// With a query the tiers still hold, builtins above skills inside one.
	all = append(all, paletteItem{name: "aquit", skill: true}, paletteItem{name: "acquit"})
	eq(t, names(paletteFilter(all, "quit")), []string{"quit", "acquit", "aquit"},
		"prefix tier first; then substring tier with the builtin above the skill")
}

func TestEnterOnBareSlashRunsHelpNotASkill(t *testing.T) {
	t.Parallel()
	r := reg(t, "help")
	if err := r.Register(commands.CommandInfo{Name: "agent", Kind: "skill", Summary: "skill: x"},
		func(string) (string, error) { return "", commands.SubmitAction("/agent") }); err != nil {
		t.Fatal(err)
	}
	d := drvCmds(t, r)
	d.typeStr("/")
	d.press(keyEnter())
	if len(d.sent) != 0 {
		t.Fatalf("enter on a bare / must not submit a skill, sent=%v", d.sent)
	}
	if p := d.plain(); !strings.Contains(p, "❯ /help") {
		t.Errorf("enter on a bare / should run /help:\n%s", p)
	}
}

func TestSkillRowsWearDimName(t *testing.T) {
	t.Parallel()
	th := defaultTheme()
	sk := paletteRow(paletteItem{name: "agent", skill: true, summary: "s"}, false, 40, 8, th)
	bi := paletteRow(paletteItem{name: "quit", summary: "s"}, false, 40, 8, th)
	if strings.HasPrefix(sk, "  /agent") {
		t.Errorf("skill row name should be styled (dim), got %q", sk)
	}
	if !strings.HasPrefix(bi, "  /quit") {
		t.Errorf("builtin row name should be plain, got %q", bi)
	}
}

// --- summaries ellipsize at a word boundary ---

func TestSummaryEllipsizedNotCutMidWord(t *testing.T) {
	t.Parallel()
	it := paletteItem{name: "x", summary: "alpha beta gamma delta epsilon zeta eta theta"}
	row := stripANSI(paletteRow(it, false, 30, 4, defaultTheme()))
	if !strings.HasSuffix(strings.TrimRight(row, " "), "…") || strings.Contains(row, "gam ") {
		t.Errorf("summary should end in … at a word boundary: %q", row)
	}
	sel := stripANSI(paletteRow(it, true, 30, 4, defaultTheme()))
	if !strings.Contains(sel, "…") {
		t.Errorf("selected row should ellipsize too: %q", sel)
	}
}

// --- fuzzy accept echo, tab cycling ---

func TestFuzzyAcceptEchoesRealCommand(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "sessions", "clear"))
	d.typeStr("/sesion") // subsequence match
	d.press(keyEnter())
	if d.m.blocks[0].text != "/sessions (from /sesion)" {
		t.Errorf("fuzzy accept echo = %q", d.m.blocks[0].text)
	}
	if d.m.blocks[1].text != "sessions ran" {
		t.Errorf("fuzzy accept should run the match, got %q", d.m.blocks[1].text)
	}
	// A prefix accept is a plain completion: no annotation.
	d.typeStr("/cl")
	d.press(keyEnter())
	if d.m.blocks[2].text != "/clear" {
		t.Errorf("prefix accept echo = %q", d.m.blocks[2].text)
	}
}

func TestTabCyclesMatches(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "clear", "collapse", "cost", "quit"))
	d.typeStr("/c")
	d.press(keyTab())
	if got := d.m.input.Value(); got != "/clear " {
		t.Fatalf("first tab = %q", got)
	}
	d.press(keyTab())
	if got := d.m.input.Value(); got != "/collapse " {
		t.Fatalf("second tab should move to the next match, got %q", got)
	}
	if p := d.plain(); !strings.Contains(p, "/cost") {
		t.Errorf("the list should keep every match of the original query while cycling:\n%s", p)
	}
	d.press(keyTab())
	d.press(keyTab())
	if got := d.m.input.Value(); got != "/clear " {
		t.Fatalf("cycling should wrap, got %q", got)
	}
	d.typeStr("x") // an edit ends the cycle
	if d.m.pal.cycling {
		t.Error("typing should end the tab cycle")
	}
	d.press(keyEnter())
	if got := d.m.blocks[0].text; got != "/clear x" {
		t.Errorf("enter after the cycle dispatches the draft, got %q", got)
	}
}

// --- /keys and "?" ---

func TestKeysCommandAndQuestionMark(t *testing.T) {
	t.Parallel()
	r := commands.NewRegistry()
	if err := r.Register(commands.CommandInfo{Name: "keys"},
		func(string) (string, error) { return "", commands.ActionKeys }); err != nil {
		t.Fatal(err)
	}
	d := drvCmds(t, r)
	d.typeStr("?")
	if d.m.input.Value() != "" {
		t.Fatalf("? on an empty composer must not type, got %q", d.m.input.Value())
	}
	p := d.plain()
	for _, want := range []string{"ctrl+c", "quit", "ctrl+o", "inspect history", "esc", "tab"} {
		if !strings.Contains(p, want) {
			t.Errorf("? should show the keymap with %q:\n%s", want, p)
		}
	}
	d.typeStr("what?")
	if got := d.m.input.Value(); got != "what?" {
		t.Errorf("? mid-text is a character, got %q", got)
	}
	d.press(keyCtrl('l'))
	d.typeStr("/keys")
	d.press(keyEnter())
	if last := d.m.blocks[len(d.m.blocks)-1]; last.kind != "system" || !strings.HasPrefix(last.text, "keys\n") {
		t.Errorf("/keys should print the keymap block, got %+v", last)
	}
}

// --- filter (pure) ---

func TestFilterTiersPrefixSubstringSubsequence(t *testing.T) {
	t.Parallel()
	all := items("monarch", "dormant", "drift", "wiki")
	eq(t, names(paletteFilter(all, "")), []string{"dormant", "drift", "monarch", "wiki"},
		"empty query lists all, alphabetical")
	eq(t, names(paletteFilter(all, "mnr")), []string{"monarch"}, "subsequence: letters in order, gaps allowed")
	eq(t, names(paletteFilter(all, "drt")), []string{"dormant", "drift"}, "subsequence keeps both d-words")
	// "or" is a substring of dormant but only a subsequence of monarch
	// (o…r), so dormant ranks first: substring tier before subsequence.
	eq(t, names(paletteFilter(all, "or")), []string{"dormant", "monarch"}, "substring before subsequence")
	if got := paletteFilter(all, "zzz"); len(got) != 0 {
		t.Errorf("no letters, no rows: %v", names(got))
	}
	// Out-of-order letters are NOT a match: fzf, not a bag of chars.
	for _, it := range paletteFilter(all, "rnm") {
		if it.name == "monarch" {
			t.Error("out-of-order rnm must not match monarch")
		}
	}
}

func TestFilterStableAsQueryGrows(t *testing.T) {
	t.Parallel()
	all := items("focus", "dormant", "drift", "quit")
	eq(t, names(paletteFilter(all, "d")), []string{"dormant", "drift"}, "d: both prefix")
	// dr: drift stays a prefix; dormant DEMOTES to the subsequence
	// tier (d…r…) rather than dropping.
	eq(t, names(paletteFilter(all, "dr")), []string{"drift", "dormant"}, "dr: dormant demotes, not drops")
	eq(t, names(paletteFilter(all, "dri")), []string{"drift"}, "dri: dormant has no i after the r")
}

// --- keys ---

func TestSelectionWrapsBothEnds(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "a1", "b2", "c3"))
	d.typeStr("/")
	if d.m.pal.selected != 0 {
		t.Fatalf("selection starts at 0, got %d", d.m.pal.selected)
	}
	d.press(keyUp())
	if d.m.pal.selected != 2 {
		t.Errorf("up from the top should wrap to the bottom, got %d", d.m.pal.selected)
	}
	d.press(keyDown())
	if d.m.pal.selected != 0 {
		t.Errorf("down from the bottom should wrap to the top, got %d", d.m.pal.selected)
	}
}

func TestTabCompletesAndStaysOpen(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "clear", "collapse"))
	d.typeStr("/cl")
	d.press(keyTab())
	if got := d.m.input.Value(); got != "/clear " {
		t.Fatalf("tab should rewrite the composer to \"/clear \", got %q", got)
	}
	if !d.m.pal.open {
		t.Error("tab must leave the palette open")
	}
}

func TestEnterDispatchesSelected(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha", "beta"))
	d.typeStr("/")
	d.press(keyDown()) // select beta
	d.press(keyEnter())
	if len(d.sent) != 0 {
		t.Fatalf("a dispatched / line must never reach the loop, sent=%v", d.sent)
	}
	if d.m.input.Value() != "" || d.m.pal.open {
		t.Error("dispatch should reset the composer and close the palette")
	}
	kinds := []string{}
	for _, b := range d.m.blocks {
		kinds = append(kinds, b.kind)
	}
	eq(t, kinds, []string{"command", "system"}, "dispatch renders a command echo + system block")
	if d.m.blocks[1].text != "beta ran" {
		t.Errorf("system block = %q, want the selected command's output", d.m.blocks[1].text)
	}
	if p := d.plain(); !strings.Contains(p, "❯ /beta") || !strings.Contains(p, "beta ran") {
		t.Errorf("frame missing dispatch echo/output:\n%s", p)
	}
}

func TestFullDraftWithArgsDispatches(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/alpha now please")
	// The query "alpha now please" matches nothing: the palette is
	// open but empty, so Enter falls through to the composer, and the
	// full draft dispatches with its args.
	if rows := d.m.paletteRows(); len(rows) != 0 {
		t.Fatalf("args in the query should empty the palette, got %d rows", len(rows))
	}
	d.press(keyEnter())
	if len(d.sent) != 0 {
		t.Fatalf("/ line must not reach the loop, sent=%v", d.sent)
	}
	if got := d.m.blocks[1].text; got != "alpha ran now please" {
		t.Errorf("args should reach the command, output %q", got)
	}
}

func TestEmptyPaletteEatsOnlyEsc(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha"))
	d.typeStr("/zzz")
	if !d.m.pal.open {
		t.Fatal("palette stays open while the draft starts with /")
	}
	d.press(keyEnter()) // NOT swallowed: falls through and dispatches
	if p := d.plain(); !strings.Contains(p, "unknown command: /zzz (try /help)") {
		t.Errorf("unknown command should render the canonical miss:\n%s", p)
	}
	d.typeStr("/zzz")
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if d.m.pal.open {
		t.Error("esc should close even an empty palette")
	}
}

func TestOtherKeysFallThroughAndRefilter(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha", "beta"))
	d.typeStr("/be")
	if got := d.m.input.Value(); got != "/be" {
		t.Fatalf("printable keys must land in the composer, got %q", got)
	}
	p := d.plain()
	if !strings.Contains(p, "/beta") {
		t.Errorf("palette should refilter as the query grows:\n%s", p)
	}
	if strings.Contains(p, "/alpha") {
		t.Errorf("alpha does not match \"be\":\n%s", p)
	}
}

// --- drawing ---

func TestOverlaySizedToContent(t *testing.T) {
	t.Parallel()
	nm := make([]string, 15)
	for i := range nm {
		nm[i] = fmt.Sprintf("c%02d", i)
	}
	d := drvCmds(t, reg(t, nm...))
	d.typeStr("/")
	if rows := d.m.paletteRows(); len(rows) != palMaxRows {
		t.Errorf("15 items cap at %d rows, got %d", palMaxRows, len(rows))
	}
	d2 := drvCmds(t, reg(t, "a1", "b2", "c3"))
	d2.typeStr("/")
	if rows := d2.m.paletteRows(); len(rows) != 3 {
		t.Errorf("3 items draw 3 rows — never reserved blanks — got %d", len(rows))
	}
}

func TestWindowSlidesToKeepSelectionVisible(t *testing.T) {
	t.Parallel()
	nm := make([]string, 15)
	for i := range nm {
		nm[i] = fmt.Sprintf("c%02d", i)
	}
	d := drvCmds(t, reg(t, nm...))
	d.typeStr("/")
	for i := 0; i < 12; i++ {
		d.press(keyDown())
	}
	if d.m.pal.selected != 12 {
		t.Fatalf("selected = %d, want 12", d.m.pal.selected)
	}
	rows := d.m.paletteRows()
	if len(rows) != palMaxRows {
		t.Fatalf("window is %d rows, want %d", len(rows), palMaxRows)
	}
	joined := stripANSI(strings.Join(rows, "\n"))
	if !strings.Contains(joined, "> /c12") {
		t.Errorf("selection marker should be on c12:\n%s", joined)
	}
	if !strings.Contains(joined, "/c03") || strings.Contains(joined, "/c02") {
		t.Errorf("window should show c03..c12:\n%s", joined)
	}
	// paletteWindow itself (the pure formula ported from old bough:
	// first = clamp(sel-(rows-1), 0, n-rows)): it slides at most one
	// row per keystroke — never a page — and the selection is always
	// inside [first, first+rows).
	first, rows2 := paletteWindow(15, 12, 10)
	if first != 3 || rows2 != 10 {
		t.Errorf("window(15,12,10) = (%d,%d), want (3,10)", first, rows2)
	}
	prev := -1
	for sel := 14; sel >= 0; sel-- { // walk the selection up the list
		f, r := paletteWindow(15, sel, 10)
		if sel < f || sel >= f+r {
			t.Errorf("sel=%d fell outside window [%d,%d)", sel, f, f+r)
		}
		if prev >= 0 && prev-f > 1 {
			t.Errorf("window re-paged: first %d -> %d for one keystroke", prev, f)
		}
		prev = f
	}
}

func TestUsageColumnShared(t *testing.T) {
	t.Parallel()
	r := commands.NewRegistry()
	must := func(err error) {
		if err != nil {
			t.Fatal(err)
		}
	}
	must(r.Register(commands.CommandInfo{Name: "go", Usage: "<pkg> <flags...>", Summary: "SUMA"},
		func(string) (string, error) { return "x", nil }))
	must(r.Register(commands.CommandInfo{Name: "quit", Usage: "", Summary: "SUMB"},
		func(string) (string, error) { return "x", nil }))
	d := drvCmds(t, r)
	d.typeStr("/")
	rows := d.m.paletteRows()
	if len(rows) != 2 {
		t.Fatalf("want 2 rows, got %d", len(rows))
	}
	a := strings.Index(stripANSI(rows[0]), "SUM")
	b := strings.Index(stripANSI(rows[1]), "SUM")
	if a < 0 || a != b {
		t.Errorf("summaries must share one column: %d vs %d\n%q\n%q",
			a, b, stripANSI(rows[0]), stripANSI(rows[1]))
	}
}

func TestSelectedRowWearsSelectStyle(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, map[string]string{"select": "0:99"}, nil, nil)
	cfg.cmds = reg(t, "alpha", "beta")
	d := newDrv(t, 80, 24, cfg)
	d.typeStr("/")
	rows := d.m.paletteRows()
	if !strings.Contains(rows[0], "48;5;99") {
		t.Errorf("selected row should carry the select background:\n%q", rows[0])
	}
	if strings.Contains(rows[1], "48;5;99") {
		t.Errorf("unselected rows carry no selection background:\n%q", rows[1])
	}
}

// --- modes ---

func TestPaletteInertWhileInspecting(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, histWith("/tmp/s.jsonl", "one"))
	cfg.cmds = reg(t, "alpha")
	d := newDrv(t, 80, 24, cfg)
	d.typeStr("/")
	if !d.m.pal.open {
		t.Fatal("precondition: palette open")
	}
	d.press(keyCtrl('o'))
	if d.m.pal.open || len(d.m.paletteRows()) != 0 {
		t.Error("the palette is inert under the inspector")
	}
	d.press(keyCtrl('o')) // closing the inspector brings it back
	if !d.m.pal.open {
		t.Error("closing the inspector should re-derive the palette from the draft")
	}
}

func TestPaletteInertWhilePicking(t *testing.T) {
	t.Parallel()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = reg(t, "alpha")
	cfg.picker = true
	cfg.sessions = []history.SessionInfo{{ID: "s1", ModTime: time.Now()}}
	d := newDrv(t, 80, 24, cfg)
	d.typeStr("/")
	if d.m.pal.open {
		t.Error("the palette must not open over the session picker")
	}
}

// --- mouse ---

func TestMouseClickSelectsAndAccepts(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "alpha", "beta", "gamma"))
	d.typeStr("/")
	top := d.m.vp.Height() - 3 // three rows, bottom-anchored above the status bar
	d.feed(tea.MouseClickMsg{X: 0, Y: top + 1, Button: tea.MouseLeft})
	if len(d.m.blocks) != 2 || d.m.blocks[1].text != "beta ran" {
		t.Fatalf("clicking row 2 should dispatch beta, blocks=%+v", d.m.blocks)
	}
	if d.m.pal.open {
		t.Error("accept closes the palette")
	}
}

// --- dispatch: UI actions, errors, history ---

// uiActionReg registers one command per UIAction.
func uiActionReg(t *testing.T, acts map[string]commands.UIAction) *commands.Registry {
	t.Helper()
	r := commands.NewRegistry()
	for name, act := range acts {
		act := act
		err := r.Register(commands.CommandInfo{Name: name, Summary: "ui: " + string(act)},
			func(string) (string, error) { return "", act })
		if err != nil {
			t.Fatal(err)
		}
	}
	return r
}

func TestUIActionClearCollapseExpand(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, uiActionReg(t, map[string]commands.UIAction{
		"clear": commands.ActionClear, "collapse": commands.ActionCollapse, "expand": commands.ActionExpand,
	}))
	d.event("result", nLines(10))
	if !d.m.blocks[0].collapsed {
		t.Fatal("precondition: result starts collapsed")
	}
	d.typeStr("/expand")
	d.press(keyEnter())
	if d.m.blocks[0].collapsed {
		t.Error("/expand should expand every block")
	}
	d.typeStr("/collapse")
	d.press(keyEnter())
	if !d.m.blocks[0].collapsed {
		t.Error("/collapse should collapse every block")
	}
	d.typeStr("/clear")
	d.press(keyEnter())
	if len(d.m.blocks) != 0 {
		t.Errorf("/clear should empty the visible transcript, %d blocks left", len(d.m.blocks))
	}
}

func TestUIActionQuit(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, uiActionReg(t, map[string]commands.UIAction{"quit": commands.ActionQuit}))
	d.typeStr("/quit")
	if !hasQuit(d.press(keyEnter())) {
		t.Error("/quit should quit the program")
	}
}

func TestUIActionOpenPicker(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, uiActionReg(t, map[string]commands.UIAction{"sessions": commands.ActionOpenPicker}))
	d.typeStr("/sessions")
	d.press(keyEnter())
	if !d.m.picking {
		t.Error("open-picker should enter the session picker")
	}
}

func TestUnknownUIActionFailsLoud(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, uiActionReg(t, map[string]commands.UIAction{"warp": commands.UIAction("warp-core")}))
	d.typeStr("/warp")
	d.press(keyEnter())
	if p := d.plain(); !strings.Contains(p, `unknown ui action "warp-core"`) {
		t.Errorf("an unknown ui action is an error block:\n%s", p)
	}
}

func TestEmptyOutputEchoesCommandName(t *testing.T) {
	t.Parallel()
	r := commands.NewRegistry()
	if err := r.Register(commands.CommandInfo{Name: "noop", Summary: "does nothing"},
		func(string) (string, error) { return "", nil }); err != nil {
		t.Fatal(err)
	}
	d := drvCmds(t, r)
	d.typeStr("/noop")
	d.press(keyEnter())
	if d.m.blocks[1].kind != "system" || d.m.blocks[1].text != "/noop" {
		t.Errorf("M27: empty output echoes the name, got %+v", d.m.blocks[1])
	}
}

// fakeLog records history appends.
type fakeLog struct{ kinds, texts []string }

func (f *fakeLog) Append(kind string, data map[string]any) history.Entry {
	f.kinds = append(f.kinds, kind)
	text, _ := data["text"].(string)
	f.texts = append(f.texts, text)
	return history.Entry{}
}

func TestDispatchRecordsCommandAndSystemEntries(t *testing.T) {
	t.Parallel()
	fl := &fakeLog{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = reg(t, "alpha")
	cfg.hlog = fl
	d := newDrv(t, 80, 24, cfg)
	d.typeStr("/alpha now")
	d.press(keyEnter())
	eq(t, fl.kinds, []string{"command", "system"}, "dispatch records command + system — never input")
	eq(t, fl.texts, []string{"/alpha now", "alpha ran now"}, "recorded texts")
}

func TestReplayCommandEntry(t *testing.T) {
	t.Parallel()
	h := fakeHist{path: "/tmp/x.jsonl", entries: []history.Entry{
		{Seq: 1, Kind: "command", Data: map[string]any{"text": "/help"}},
	}}
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "command" {
		t.Fatalf("command entry should replay as a command block, got %+v", d.m.blocks)
	}
	if p := d.plain(); !strings.Contains(p, "❯ /help") {
		t.Errorf("replayed command echo missing:\n%s", p)
	}
}

// --- full program (teatest) ---

func TestProgramSlashDispatch(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 16)
	t.Cleanup(func() { close(events) })
	var cfgp atomic.Pointer[uiCfg]
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cmds = reg(t, "alpha", "beta")
	cfgp.Store(cfg)
	sent := make(chan string, 1)
	m := newModel(80, 24, func(line string) { sent <- line }, events, &cfgp)
	tm := teatest.NewTestModel(t, m, teatest.WithInitialTermSize(80, 24))

	tm.Type("/be")
	tm.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForOutput(t, tm, "❯ /beta", "beta ran")
	select {
	case l := <-sent:
		t.Fatalf("dispatched line reached the loop: %q", l)
	default:
	}
	tm.Send(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	tm.WaitFinished(t, teatest.WithFinalTimeout(4*time.Second))
}
