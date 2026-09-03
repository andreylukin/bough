package ui

// Status bar, spinner lifecycle, theme restyling, resize reflow.

import (
	"strings"
	"testing"

	"charm.land/lipgloss/v2"
	"pgregory.net/rapid"

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
	if p := d.plain(); !strings.Contains(p, "↑1.2k ↓300 · $0.0042 · ? keys") {
		t.Errorf("status bar missing the priced usage:\n%s", p)
	}
	cfg.usage = fakeUsage{llm.Usage{InputTokens: 1200, OutputTokens: 300}}
	if p := d.plain(); !strings.Contains(p, "↑1.2k ↓300 · ? keys") {
		t.Errorf("status bar missing the token usage:\n%s", p)
	}
}

type fakeUsage struct{ u llm.Usage }

func (f fakeUsage) Usage() llm.Usage { return f.u }

// fakeLimit is a usage service that also knows the context window
// (the cost row's shape).
type fakeLimit struct {
	fakeUsage
	limit int
}

func (f fakeLimit) ContextLimit() int { return f.limit }

type fakeModel string

func (f fakeModel) Model() string { return string(f) }

// footerCfg is the idle bar's full truth: a priced tally, a last
// request at 34% of a known window, a named model.
func footerCfg(t *testing.T) *uiCfg {
	cfg := cfgWith(t, nil, nil, nil)
	cfg.usage = fakeLimit{fakeUsage{llm.Usage{InputTokens: 12_300, OutputTokens: 3_400, LastInputTokens: 357_000, Cost: 0.052, Priced: true}}, 1_050_000}
	cfg.limit = cfg.usage.(fakeLimit)
	cfg.modeler = fakeModel("gpt-5.6-luna")
	return cfg
}

// bar is the status row of the frame; it must be exactly one row of
// exactly the terminal's width.
func bar(t *testing.T, d *drv, w int) string {
	t.Helper()
	line := d.m.statusBar(d.cfgp.Load())
	if strings.Contains(line, "\n") {
		t.Fatalf("status bar wrapped at width %d:\n%q", w, stripANSI(line))
	}
	if got := lipgloss.Width(line); got != w {
		t.Fatalf("status bar width = %d, want %d:\n%q", got, w, stripANSI(line))
	}
	return stripANSI(line)
}

// A narrow pane drops parts from the left — tokens, then the model,
// then the context — and "? keys" is the floor.
func TestStatusBarTruncationTiers(t *testing.T) {
	t.Parallel()
	cases := []struct {
		w    int
		want string
	}{
		{120, "↑12.3k ↓3.4k · $0.052 · 34% ctx · gpt-5.6-luna · ? keys"},
		{80, "↑12.3k ↓3.4k · $0.052 · 34% ctx · gpt-5.6-luna · ? keys"},
		{50, "$0.052 · 34% ctx · gpt-5.6-luna · ? keys"},
		{40, "$0.052 · 34% ctx · ? keys"},
		{30, "$0.052 · ? keys"},
		{20, "? keys"},
	}
	for _, c := range cases {
		d := newDrv(t, c.w, 24, footerCfg(t))
		got := bar(t, d, c.w)
		if !strings.HasSuffix(strings.TrimRight(got, " "), c.want) {
			t.Errorf("width %d: bar = %q, want the right side %q", c.w, got, c.want)
		}
		if c.w >= 50 && strings.Count(got, "gpt-5.6-luna") != 1 {
			t.Errorf("width %d: the model shows once: %q", c.w, got)
		}
		if strings.Contains(got, "…") {
			t.Errorf("width %d: a tier that fits is never truncated: %q", c.w, got)
		}
	}
}

// The real app's left identity is "bough · <model>" from the row
// config; once the llm names its model on the right the left is bare
// "bough", so the spec's 80-column bar fits a long Anthropic id.
func TestStatusBarModelShowsOnce(t *testing.T) {
	t.Parallel()
	const mdl = "claude-sonnet-4-6-20251114"
	cases := []struct {
		w    int
		want string
	}{
		{120, "↑12.3k ↓3.4k · $0.052 · 34% ctx · " + mdl + " · ? keys"},
		{80, "↑12.3k ↓3.4k · $0.052 · 34% ctx · " + mdl + " · ? keys"},
		{64, "$0.052 · 34% ctx · " + mdl + " · ? keys"},
		{30, "$0.052 · ? keys"},
	}
	for _, c := range cases {
		cfg := footerCfg(t)
		cfg.status = "bough · " + mdl
		cfg.modeler = fakeModel(mdl)
		d := newDrv(t, c.w, 24, cfg)
		got := bar(t, d, c.w)
		if !strings.HasPrefix(got, " bough ") {
			t.Errorf("width %d: the left is bare bough: %q", c.w, got)
		}
		if !strings.HasSuffix(strings.TrimRight(got, " "), c.want) {
			t.Errorf("width %d: bar = %q, want the right side %q", c.w, got, c.want)
		}
		if n := strings.Count(got, mdl); n > 1 {
			t.Errorf("width %d: the model shows %d times: %q", c.w, n, got)
		}
	}
	// No Modeler: the row's model stays on the left, the only place
	// that names it.
	cfg := footerCfg(t)
	cfg.status, cfg.modeler = "bough · "+mdl, nil
	d := newDrv(t, 100, 24, cfg)
	if got := bar(t, d, 100); !strings.HasPrefix(got, " bough · "+mdl) {
		t.Errorf("no Modeler: the left keeps the row's model: %q", got)
	}
}

func TestStatusBarUnpricedNoModelNoLimit(t *testing.T) {
	t.Parallel()
	cfg := footerCfg(t)
	u := cfg.usage.(fakeLimit)
	u.u.Priced = false
	cfg.usage, cfg.limit = u, u
	d := newDrv(t, 100, 24, cfg)
	if got := bar(t, d, 100); !strings.Contains(got, "↑12.3k ↓3.4k · 34% ctx · gpt-5.6-luna · ? keys") || strings.Contains(got, "$") {
		t.Errorf("unpriced: no dollar figure: %q", got)
	}

	cfg = footerCfg(t)
	cfg.modeler = nil
	d = newDrv(t, 100, 24, cfg)
	if got := bar(t, d, 100); !strings.Contains(got, "↑12.3k ↓3.4k · $0.052 · 34% ctx · ? keys") {
		t.Errorf("no Modeler: no model name: %q", got)
	}

	cfg = footerCfg(t)
	cfg.limit = nil
	d = newDrv(t, 100, 24, cfg)
	if got := bar(t, d, 100); !strings.Contains(got, "↑12.3k ↓3.4k · $0.052 · gpt-5.6-luna · ? keys") || strings.Contains(got, "ctx") {
		t.Errorf("no context limit: no percentage: %q", got)
	}

	cfg = footerCfg(t)
	cfg.limit = fakeLimit{limit: 0}
	d = newDrv(t, 100, 24, cfg)
	if got := bar(t, d, 100); strings.Contains(got, "ctx") {
		t.Errorf("unknown limit (0): no percentage: %q", got)
	}

	cfg = footerCfg(t)
	cfg.usage = fakeUsage{}
	cfg.limit = nil
	d = newDrv(t, 100, 24, cfg)
	if got := bar(t, d, 100); !strings.HasSuffix(strings.TrimRight(got, " "), "gpt-5.6-luna · ? keys") || strings.Contains(got, "↑") {
		t.Errorf("nothing used yet: just the model: %q", got)
	}
}

// The bar is one row of exactly the terminal's width whatever the
// width and whatever it has to say.
func TestPropStatusBarFitsWidth(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		w := rapid.IntRange(20, 200).Draw(rt, "w")
		cfg := cfgWith(t, nil, nil, nil)
		u := llm.Usage{
			InputTokens:     rapid.IntRange(0, 5_000_000).Draw(rt, "in"),
			OutputTokens:    rapid.IntRange(0, 5_000_000).Draw(rt, "out"),
			LastInputTokens: rapid.IntRange(0, 2_000_000).Draw(rt, "last"),
			Cost:            rapid.Float64Range(0, 500).Draw(rt, "cost"),
			Priced:          rapid.Bool().Draw(rt, "priced"),
		}
		lim := fakeLimit{fakeUsage{u}, rapid.SampledFrom([]int{0, 200_000, 1_000_000, 1_050_000}).Draw(rt, "limit")}
		cfg.usage = lim
		if rapid.Bool().Draw(rt, "haslimit") {
			cfg.limit = lim
		}
		if rapid.Bool().Draw(rt, "hasmodel") {
			cfg.modeler = fakeModel(rapid.StringMatching(`[a-z0-9./-]{0,40}`).Draw(rt, "model"))
		}
		d := newDrv(t, w, 24, cfg)
		if rapid.Bool().Draw(rt, "running") {
			d.typeStr("go")
			d.press(keyEnter())
		}
		line := d.m.statusBar(cfg)
		if strings.Contains(line, "\n") {
			rt.Fatalf("width %d: bar wrapped: %q", w, stripANSI(line))
		}
		if got := lipgloss.Width(line); got != w {
			rt.Fatalf("width %d: bar is %d wide: %q", w, got, stripANSI(line))
		}
		if !strings.Contains(stripANSI(line), "? keys") && w >= 40 {
			rt.Fatalf("width %d: keys hint lost: %q", w, stripANSI(line))
		}
	})
}

// The welcome names the model and, when known, its context window.
func TestWelcomeNamesModelAndContext(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, footerCfg(t))
	if p := d.plain(); !strings.Contains(p, "model gpt-5.6-luna · 1.05M context") {
		t.Errorf("welcome missing the model line:\n%s", p)
	}
	cfg := footerCfg(t)
	cfg.limit = nil
	d = newDrv(t, 80, 24, cfg)
	if p := d.plain(); !strings.Contains(p, "model gpt-5.6-luna") || strings.Contains(p, "context") {
		t.Errorf("unknown limit: the model alone:\n%s", p)
	}
	if p := defaultDrv(t).plain(); strings.Contains(p, "model ") {
		t.Errorf("no Modeler: no model line:\n%s", p)
	}
	for n, want := range map[int]string{200_000: "200k", 1_000_000: "1M", 1_050_000: "1.05M", 128_000: "128k", 1_047_576: "1.05M"} {
		if got := ctxAbbrev(n); got != want {
			t.Errorf("ctxAbbrev(%d) = %q, want %q", n, got, want)
		}
	}
}

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
