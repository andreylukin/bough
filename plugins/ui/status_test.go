package ui

// Status bar, spinner lifecycle, theme restyling, resize reflow.

import (
	"strings"
	"testing"

	"charm.land/lipgloss/v2"
)

func TestStatusBarShowsIdentity(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	if p := d.plain(); !strings.Contains(p, "bough") {
		t.Errorf("status bar missing identity:\n%s", p)
	}
}

func TestStatusBarShowsEntryCountAndBasename(t *testing.T) {
	t.Parallel()
	h := histWith("/home/x/.bough/history/2026-09-01-abc.jsonl", "a", "b", "c")
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	p := d.plain()
	if !strings.Contains(p, "3 entries") {
		t.Errorf("status bar missing entry count:\n%s", p)
	}
	if !strings.Contains(p, "2026-09-01-abc.jsonl") {
		t.Errorf("status bar missing history basename:\n%s", p)
	}
	if strings.Contains(p, "/home/x/.bough") {
		t.Errorf("status bar should show basename, not full path:\n%s", p)
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

func TestSpinnerClearedOnError(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("go")
	d.press(keyEnter())
	d.event("error", "boom")
	if d.m.running {
		t.Error("error should clear running")
	}
	if spinnerFrameIn(d.plain()) {
		t.Errorf("spinner should vanish after error:\n%s", d.plain())
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
	boxWidth := func(s string) int {
		for _, l := range strings.Split(s, "\n") {
			if strings.Contains(l, "╭─ js") {
				return lipgloss.Width(strings.TrimRight(l, " "))
			}
		}
		t.Fatal("code box top border not found")
		return 0
	}
	if got := boxWidth(d.plain()); got != 76 { // width-4 at 80 cols
		t.Errorf("box width at 80 cols = %d, want 76", got)
	}
	d.feed(windowSize(120, 40))
	if got := boxWidth(d.plain()); got != 116 { // reflowed to width-4 at 120 cols
		t.Errorf("box width at 120 cols = %d, want 116", got)
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
