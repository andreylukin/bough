package ui

// Status bar, spinner lifecycle, theme restyling, resize reflow.

import (
	"strings"
	"testing"

	"charm.land/lipgloss/v2"

	"github.com/andreylukin/bough/plugins/llm"
)

func TestStatusBarShowsIdentity(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	if p := d.plain(); !strings.Contains(p, "bough") {
		t.Errorf("status bar missing identity:\n%s", p)
	}
}

// The bar is identity · usage · "? keys" — no session file or entry
// count (that is /sessions' job).
func TestStatusBarShowsUsageAndKeysHint(t *testing.T) {
	t.Parallel()
	h := histWith("/home/x/.bough/history/2026-09-01-abc.jsonl", "a", "b", "c")
	cfg := cfgWith(t, nil, nil, h)
	d := newDrv(t, 80, 24, cfg)
	p := d.plain()
	// The bar is the second-to-last line (the composer sits below it);
	// the transcript's "resumed …" row may name the session, the bar
	// never does.
	lines := strings.Split(strings.TrimRight(p, "\n"), "\n")
	bar := lines[len(lines)-2]
	if strings.Contains(bar, "entries") || strings.Contains(bar, ".jsonl") || strings.Contains(bar, "abc") {
		t.Errorf("status bar should not show the session file:\n%s", p)
	}
	if !strings.Contains(bar, "? keys") {
		t.Errorf("status bar missing the keys hint:\n%s", p)
	}
	cfg.usage = fakeUsage{llm.Usage{InputTokens: 1200, OutputTokens: 300, Cost: 0.0042, Priced: true}}
	if p := d.plain(); !strings.Contains(p, "$0.0042 · 1.5k tok · ? keys") {
		t.Errorf("status bar missing the priced usage:\n%s", p)
	}
	cfg.usage = fakeUsage{llm.Usage{InputTokens: 1200, OutputTokens: 300}}
	if p := d.plain(); !strings.Contains(p, "1.5k tok · ? keys") {
		t.Errorf("status bar missing the token usage:\n%s", p)
	}
}

type fakeUsage struct{ u llm.Usage }

func (f fakeUsage) Usage() llm.Usage { return f.u }

// A pending ask stops the spinner and says the turn is waiting on the
// user; the ask's placeholder tells them how to answer.
func TestStatusBarWaitingOnAsk(t *testing.T) {
	t.Parallel()
	d, _ := askDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.feed(askEvent())
	p := d.plain()
	if !strings.Contains(p, "waiting for you") {
		t.Errorf("status bar should say waiting for you:\n%s", p)
	}
	if spinnerFrameIn(p) {
		t.Errorf("spinner should stop while an ask is pending:\n%s", p)
	}
	if !strings.Contains(p, "type a number or your answer") {
		t.Errorf("ask placeholder missing:\n%s", p)
	}
	d.typeStr("1")
	d.press(keyEnter())
	if p := d.plain(); strings.Contains(p, "waiting for you") || !spinnerFrameIn(p) {
		t.Errorf("answering should resume the running state:\n%s", p)
	}
}

func TestStatusBarInspectingHint(t *testing.T) {
	t.Parallel()
	h := histWith("/tmp/s.jsonl", "one")
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	d.press(keyCtrl('o'))
	if p := d.plain(); !strings.Contains(p, "inspecting · ctrl+o to close") {
		t.Errorf("status bar missing inspecting hint:\n%s", p)
	}
}

func TestStatusBarFillsWidth(t *testing.T) {
	t.Parallel()
	for _, w := range []int{60, 80, 120} {
		d := newDrv(t, w, 24, cfgWith(t, nil, nil, nil))
		lines := strings.Split(d.view(), "\n")
		if len(lines) < 2 {
			t.Fatalf("frame too short at width %d", w)
		}
		status := lines[len(lines)-2] // body, status, input
		if got := lipgloss.Width(status); got != w {
			t.Errorf("status bar width = %d, want %d", got, w)
		}
	}
}

// --- spinner ---

func TestSpinnerAppearsWhileRunning(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	if spinnerFrameIn(d.plain()) {
		t.Fatal("spinner should be hidden before any send")
	}
	d.typeStr("go")
	d.press(keyEnter())
	if !d.m.running {
		t.Fatal("model should be running after send")
	}
	if !spinnerFrameIn(d.plain()) {
		t.Errorf("spinner frame missing between send and done:\n%s", d.plain())
	}
}

func TestSpinnerClearedOnDone(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.event("done", "")
	if d.m.running {
		t.Error("done should clear running")
	}
	if spinnerFrameIn(d.plain()) {
		t.Errorf("spinner should vanish after done:\n%s", d.plain())
	}
}

// A code error mid-turn is fed back to the model; the loop closes every
// turn with "done", so the spinner runs through the error and stops at
// done (ending it at the error froze it while the model recovered).
func TestSpinnerSurvivesCodeErrorUntilDone(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.event("error", "boom")
	if !d.m.running || !spinnerFrameIn(d.plain()) {
		t.Errorf("error should not end the turn:\n%s", d.plain())
	}
	d.event("done", "")
	if d.m.running || spinnerFrameIn(d.plain()) {
		t.Errorf("spinner should vanish after done:\n%s", d.plain())
	}
}

// --- theme ---

func TestStyledOutputCarriesANSI(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "colored")
	if v := d.view(); !strings.Contains(v, "\x1b[") {
		t.Errorf("default theme render should contain ANSI escapes:\n%q", v)
	}
}

func TestThemeServiceChangesStyles(t *testing.T) {
	t.Parallel()
	base := defaultDrv(t)
	themed := newDrv(t, 80, 24, cfgWith(t, map[string]string{"user": "#ff0000:bold:italic"}, nil, nil))
	base.event("user", "same text")
	themed.event("user", "same text")
	if base.plain() != themed.plain() {
		t.Fatal("theme change must not alter the plain text")
	}
	if base.view() == themed.view() {
		t.Error("custom user style should change the styled output")
	}
}

func TestThemeErrorTokenStyled(t *testing.T) {
	t.Parallel()
	base := defaultDrv(t)
	themed := newDrv(t, 80, 24, cfgWith(t, map[string]string{"error": "#00ff00"}, nil, nil))
	base.event("error", "boom")
	themed.event("error", "boom")
	if base.view() == themed.view() {
		t.Error("custom error style should change the styled output")
	}
}

func TestThemeSwapRestylesLiveModel(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "hot reload me")
	before := d.view()
	d.cfgp.Store(cfgWith(t, map[string]string{"user": "#ffaf00:bold"}, nil, nil))
	d.event("done", "") // any event refreshes from the current cfg
	after := d.view()
	if before == after {
		t.Error("swapping the cfg pointer should restyle existing blocks")
	}
	if !strings.Contains(stripANSI(after), "hot reload me") {
		t.Error("restyle must keep the transcript text")
	}
}

// --- resize ---

func TestResizeReflowsBoxes(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("code", "let x = 1")
	d.press(keyTab()) // expand the collapsed code block to get a box
	d.press(keyEnter())
	boxWidth := func(s string) int {
		for l := range strings.SplitSeq(s, "\n") {
			if strings.Contains(l, "╭") {
				return lipgloss.Width(strings.TrimRight(l, " "))
			}
		}
		t.Fatal("code box top border not found")
		return 0
	}
	at80 := boxWidth(d.plain())
	if at80 < 70 || at80 > 80 {
		t.Errorf("box width at 80 cols = %d, want near 78", at80)
	}
	d.feed(windowSize(120, 40))
	if got := boxWidth(d.plain()); got != at80+40 { // reflowed with the terminal
		t.Errorf("box width at 120 cols = %d, want %d", got, at80+40)
	}
	if !strings.Contains(d.plain(), "let x = 1") {
		t.Error("code text must survive reflow")
	}
}

func TestResizeFrameHeights(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(windowSize(80, 24))
	h24 := len(strings.Split(d.view(), "\n"))
	d.feed(windowSize(120, 40))
	h40 := len(strings.Split(d.view(), "\n"))
	if h24 != 24 || h40 != 40 {
		t.Errorf("frame heights = %d/%d, want 24/40", h24, h40)
	}
}

func TestResizeTinyNoPanic(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(windowSize(5, 2))
	d.event("result", nLines(20))
	d.event("code", "x")
	d.event("done", "")
	_ = d.view() // must not panic
}
