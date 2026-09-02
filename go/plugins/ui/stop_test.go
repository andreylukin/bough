package ui

// Turn cancel, two-press quit, esc, ctrl+d, exit line (stop.go).

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
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

func TestEscClearsComposerWhenIdle(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("half a thought")
	d.press(keyEsc())
	if got := d.m.input.Value(); got != "" {
		t.Errorf("esc should clear the composer, got %q", got)
	}
}

func TestEscLeavesOpenPaletteToItself(t *testing.T) {
	t.Parallel()
	d := drvCmds(t, reg(t, "help"))
	d.typeStr("/he")
	if !d.m.pal.open {
		t.Fatal("palette should open on /")
	}
	d.press(keyEsc())
	if d.m.pal.open {
		t.Error("esc should close the palette")
	}
	if got := d.m.input.Value(); got != "/he" {
		t.Errorf("esc on an open palette must not clear the draft, got %q", got)
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
	now = now.Add(2 * time.Second)
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
