package ui

// Turn cancel, two-press quit, esc, ctrl+d, exit line (stop.go).

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/andreylukin/bough/plugins/history"
)

func keyEsc() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeyEscape} }

// cancelDrv is a driver whose cfg has a counting "cancel" service.
func cancelDrv(t *testing.T) (*drv, *int) {
	t.Helper()
	calls := 0
	cfg := cfgWith(t, nil, nil, nil)
	cfg.cancel = func() { calls++ }
	return newDrv(t, 80, 24, cfg), &calls
}

func TestCtrlCCancelsRunningTurn(t *testing.T) {
	t.Parallel()
	d, calls := cancelDrv(t)
	d.typeStr("slow thing")
	d.press(keyEnter())
	if !d.m.running {
		t.Fatal("should be running after send")
	}
	if hasQuit(d.press(keyCtrl('c'))) {
		t.Fatal("ctrl+c mid-turn must cancel, not quit")
	}
	if *calls != 1 {
		t.Fatalf("cancel service called %d times, want 1", *calls)
	}
	if !d.m.running || !strings.Contains(d.plain(), "cancelling") {
		t.Errorf("should stay running with a cancelling flash until the loop answers:\n%s", d.plain())
	}
	// The loop answers with its cancelled row and the turn's done.
	d.event("cancelled", "")
	d.event("done", "")
	if d.m.running {
		t.Error("done should end the turn")
	}
	if !strings.Contains(d.plain(), "cancelled") {
		t.Errorf("cancelled row missing:\n%s", d.plain())
	}
}

func TestEscCancelsRunningTurn(t *testing.T) {
	t.Parallel()
	d, calls := cancelDrv(t)
	d.typeStr("slow thing")
	d.press(keyEnter())
	d.press(keyEsc())
	if *calls != 1 {
		t.Fatalf("cancel service called %d times, want 1", *calls)
	}
}

// Claude Code's contract: a single esc interrupts, a DOUBLE esc clears
// the draft. One press only arms, and says what a second would do.
func TestDoubleEscClearsComposerWhenIdle(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("half a thought")

	d.press(keyEsc())
	if got := d.m.input.Value(); got != "half a thought" {
		t.Fatalf("one esc must not clear the draft, got %q", got)
	}
	if !strings.Contains(d.plain(), "press esc again to clear") {
		t.Errorf("the first press should say what a second does:\n%s", d.plain())
	}

	d.press(keyEsc())
	if got := d.m.input.Value(); got != "" {
		t.Errorf("the second esc should clear the composer, got %q", got)
	}
}

// Any other key disarms, so esc-something-esc does not clear.
func TestEscDisarmedByAnotherKey(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("keep me")
	d.press(keyEsc())
	d.typeStr("!") // any key at all
	d.press(keyEsc())
	if got := d.m.input.Value(); got == "" {
		t.Error("a key between the two escs should have disarmed the pair")
	}
}

// On an empty composer the pair opens the rewind MENU — a list you walk
// and pick from, which is what Claude Code's Esc+Esc opens.
func TestDoubleEscOnEmptyComposerOpensTheRewindMenu(t *testing.T) {
	t.Parallel()
	d := rewindDrv(t, "first thing", "second thing")
	d.press(keyEsc())
	if !strings.Contains(d.plain(), "press esc again to rewind") {
		t.Fatalf("the first press should offer the rewind:\n%s", d.plain())
	}
	d.press(keyEsc())
	p := d.plain()
	if !d.m.rw.open {
		t.Fatalf("the second esc should open the menu:\n%s", p)
	}
	for _, want := range []string{"rewind", "first thing", "second thing", "(current)", "esc cancels"} {
		if !strings.Contains(p, want) {
			t.Errorf("the menu should show %q:\n%s", want, p)
		}
	}
}

// Without a /tree command there is nothing to fork with, and the menu
// says so instead of panicking on a nil registry.
func TestDoubleEscWithoutTreeSaysSo(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyEsc())
	d.press(keyEsc())
	if d.m.rw.open {
		t.Error("no /tree command: the menu should not open")
	}
	if !strings.Contains(d.plain(), "nothing to rewind to") {
		t.Errorf("it should say why:\n%s", d.plain())
	}
}

func TestEscLeavesOpenPaletteToItself(t *testing.T) {
	t.Parallel()
	// The palette owns esc: it closes and clears a lone "/query" (the
	// draft was only ever a filter), but never touches other text.
	d := drvCmds(t, reg(t, "help"))
	d.typeStr("look at /he")
	if !d.m.pal.open {
		t.Fatal("palette should open on /")
	}
	d.press(keyEsc())
	if d.m.pal.open {
		t.Error("esc should close the palette")
	}
	if got := d.m.input.Value(); got != "look at /he" {
		t.Errorf("esc on an open palette must not clear a mixed draft, got %q", got)
	}
}

func TestCancelWithoutServiceIsLoud(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t) // no cancel service
	d.typeStr("go")
	d.press(keyEnter())
	d.press(keyCtrl('c'))
	if !strings.Contains(d.plain(), "no cancel service") {
		t.Errorf("missing service should be reported:\n%s", d.plain())
	}
}

func TestQuitArmExpires(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	now := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	d.m.stop.now = func() time.Time { return now }
	d.press(keyCtrl('c'))
	now = now.Add(quitWindow + time.Second)
	if hasQuit(d.press(keyCtrl('c'))) {
		t.Fatal("second press after the window should re-arm, not quit")
	}
	now = now.Add(time.Second)
	if !hasQuit(d.press(keyCtrl('c'))) {
		t.Error("press within the window should quit")
	}
}

func TestOtherKeyDisarmsQuit(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.press(keyCtrl('c'))
	d.typeStr("x")
	if hasQuit(d.press(keyCtrl('c'))) {
		t.Error("typing between presses should disarm the quit")
	}
}

func TestCtrlDQuitsIdleEmptyComposer(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	if !hasQuit(d.press(keyCtrl('d'))) {
		t.Error("ctrl+d on an idle, empty composer should quit")
	}
	d = defaultDrv(t)
	d.typeStr("draft")
	if hasQuit(d.press(keyCtrl('d'))) {
		t.Error("ctrl+d with a draft must not quit")
	}
	d = defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	if hasQuit(d.press(keyCtrl('d'))) {
		t.Error("ctrl+d mid-turn must not quit")
	}
}

func TestExitLine(t *testing.T) {
	t.Parallel()
	got := exitLine(fakeHist{path: "/home/u/.bough/sessions/2026-09-02-abcd.jsonl"})
	want := "session 2026-09-02-abcd · resume with: bough -r 2026-09-02-abcd"
	if got != want {
		t.Errorf("exitLine = %q, want %q", got, want)
	}
	if exitLine(nil) != "" || exitLine(fakeHist{}) != "" {
		t.Error("no session file: no line")
	}
}

// The "cancelling…" flash is for the wait; once the turn closes it
// would only hide the usage chip.
func TestCancellingFlashClearsWhenTheTurnEnds(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.m.cfg.Load().cancel = func() {}
	d.typeStr("go")
	d.press(keyEnter())
	d.press(keyEsc())
	if d.m.flash != "cancelling…" {
		t.Fatalf("flash after esc: %q", d.m.flash)
	}
	d.event("cancelled", "")
	d.event("done", "")
	if d.m.flash != "" {
		t.Fatalf("flash should clear at done, got %q", d.m.flash)
	}
}

// rewindDrv is a driver with a /tree command and a history of turns,
// which is what the rewind menu needs to open.
func rewindDrv(t *testing.T, prompts ...string) *drv {
	t.Helper()
	h := fakeHist{path: "/tmp/s.jsonl"}
	for i, p := range prompts {
		h.entries = append(h.entries,
			history.Entry{Seq: int64(i*2 + 1), Kind: "input", Data: map[string]any{"text": p}},
			history.Entry{Seq: int64(i*2 + 2), Kind: "done", Data: map[string]any{"files": []string{"a.go"}}})
	}
	cfg := cfgWith(t, nil, nil, h)
	cfg.cmds = reg(t, "tree", "new")
	return newDrv(t, 100, 30, cfg)
}
