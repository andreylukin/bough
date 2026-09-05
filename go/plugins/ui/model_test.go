package ui

import (
	"strings"
	"sync/atomic"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func testModel(t *testing.T) model {
	t.Helper()
	var cfg atomic.Pointer[uiCfg]
	cfg.Store(newCfg(defaultTheme(), defaultKeymap(), "bough", nil))
	return newModel(80, 24, func(string) {}, nil, &cfg)
}

// testModelCollapse is testModel with the collapse mode overridden.
func testModelCollapse(t *testing.T, mode string) model {
	t.Helper()
	var cfg atomic.Pointer[uiCfg]
	c := newCfg(defaultTheme(), defaultKeymap(), "bough", nil)
	c.collapse = mode
	cfg.Store(c)
	return newModel(80, 24, func(string) {}, nil, &cfg)
}

func TestCollapseDefaults(t *testing.T) {
	long := strings.TrimSuffix(strings.Repeat("line\n", 20), "\n")

	// "all" (the default): every code/result block starts collapsed,
	// regardless of size.
	m := testModel(t)
	m.addEvent(Event{Kind: "result", Text: long})
	m.addEvent(Event{Kind: "result", Text: "one\ntwo\nthree"})
	m.addEvent(Event{Kind: "code", Text: "a\nb\nc\nd"})
	for i, b := range m.blocks {
		if !b.collapsed {
			t.Fatalf("collapse=all: block %d (%s) should start collapsed", i, b.kind)
		}
	}
	out := m.render(&m.blocks[0], m.cfg.Load())
	if !strings.Contains(out, "▸ result (20 lines): line") {
		t.Errorf("collapsed result should be a header line:\n%s", out)
	}
	if strings.Count(out, "\n") != 0 {
		t.Errorf("collapsed render should be one line:\n%s", out)
	}
	m.blocks[0].collapsed = false
	out = m.render(&m.blocks[0], m.cfg.Load())
	if !strings.Contains(out, "▾ result (20 lines)") || strings.Count(out, "line") < 20 {
		t.Errorf("expanded render should show header plus all 20 lines:\n%s", out)
	}
	if out := m.render(&m.blocks[2], m.cfg.Load()); !strings.Contains(out, "▸ code js (4 lines): a") {
		t.Errorf("code header wrong:\n%s", out)
	}

	// The assistant's prose is the story: never collapsible. A note
	// (error/system/todo) is detail — collapsible, but a ONE-LINE one
	// starts open, since its header would be longer than the line.
	m.addEvent(Event{Kind: "assistant", Text: "hi\nthere\nfriend\nagain"})
	m.addEvent(Event{Kind: "error", Text: "boom"})
	m.addEvent(Event{Kind: "error", Text: "boom\nstack\ntrace"})
	if m.blocks[3].collapsed || m.blocks[3].collapsible() {
		t.Errorf("assistant blocks are never collapsible: %+v", m.blocks[3])
	}
	if m.blocks[4].collapsed || !m.blocks[4].collapsible() {
		t.Errorf("a one-line error is collapsible but starts open: %+v", m.blocks[4])
	}
	if !m.blocks[5].collapsed {
		t.Errorf("a multi-line error starts closed under collapse: all: %+v", m.blocks[5])
	}

	// "large": only blocks over collapseAt lines start collapsed.
	m = testModelCollapse(t, "large")
	m.addEvent(Event{Kind: "result", Text: long})
	m.addEvent(Event{Kind: "result", Text: "one\ntwo\nthree"})
	m.addEvent(Event{Kind: "code", Text: "a\nb\nc\nd"})
	if !m.blocks[0].collapsed {
		t.Error("collapse=large: 20-line result should start collapsed")
	}
	if m.blocks[1].collapsed {
		t.Error("collapse=large: 3-line result should start expanded")
	}
	if !m.blocks[2].collapsed {
		t.Error("collapse=large: 4-line code block should start collapsed")
	}

	// "none": everything starts expanded.
	m = testModelCollapse(t, "none")
	m.addEvent(Event{Kind: "result", Text: long})
	m.addEvent(Event{Kind: "code", Text: "a\nb\nc\nd"})
	for i, b := range m.blocks {
		if b.collapsed {
			t.Errorf("collapse=none: block %d (%s) should start expanded", i, b.kind)
		}
	}
}

// clickAt sends a left click at terminal row y through Update.
func clickAt(m model, y int) model {
	next, _ := m.Update(tea.MouseClickMsg{X: 0, Y: y, Button: tea.MouseLeft})
	next, _ = next.(model).Update(tea.MouseReleaseMsg{X: 0, Y: y, Button: tea.MouseLeft})
	return next.(model)
}

func TestMouseClickToggles(t *testing.T) {
	m := testModel(t)
	long := strings.TrimSuffix(strings.Repeat("line\n", 10), "\n")
	m.addEvent(Event{Kind: "assistant", Text: "hello"})
	m.addEvent(Event{Kind: "result", Text: long})

	// The collapsed result is the last content line; content fits the
	// viewport so screen row == content row.
	r := m.ranges[1]
	if r.end-r.start != 1 {
		t.Fatalf("collapsed result should span 1 line, got %+v", r)
	}
	m = clickAt(m, r.start)
	if m.blocks[1].collapsed {
		t.Fatal("click on collapsed result should expand it")
	}
	if m.focusID != m.blocks[1].id {
		t.Error("click should focus the block")
	}
	// Ranges recomputed: block now spans header + boxed body.
	r = m.ranges[1]
	if r.end-r.start < 11 {
		t.Fatalf("expanded result should span header+body, got %+v", r)
	}
	// Click anywhere inside the body collapses it again.
	m = clickAt(m, r.start+3-m.vp.YOffset())
	if !m.blocks[1].collapsed {
		t.Fatal("click inside expanded block should collapse it")
	}

	// Clicks on non-collapsible blocks do nothing.
	before := m.blocks[0]
	m = clickAt(m, m.ranges[0].start-m.vp.YOffset())
	if b := m.blocks[0]; b.kind != before.kind || b.text != before.text || b.collapsed != before.collapsed {
		t.Error("click on assistant block should be a no-op")
	}
}

func TestFocusAndKeyboardToggle(t *testing.T) {
	m := testModel(t)
	long := strings.TrimSuffix(strings.Repeat("x\n", 5), "\n")
	m.addEvent(Event{Kind: "code", Text: long})
	m.addEvent(Event{Kind: "assistant", Text: "hi"})
	m.addEvent(Event{Kind: "result", Text: long})

	press := func(m model, key string) model {
		var msg tea.KeyPressMsg
		switch key {
		case "tab":
			msg = tea.KeyPressMsg{Code: tea.KeyTab}
		case "shift+tab":
			msg = tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift}
		case "enter":
			msg = tea.KeyPressMsg{Code: tea.KeyEnter}
		}
		next, _ := m.Update(msg)
		return next.(model)
	}

	m = press(m, "tab") // newest collapsible first: the result block
	if m.focusID != m.blocks[2].id {
		t.Fatalf("tab should focus the newest (result) block, focusID=%d", m.focusID)
	}
	m = press(m, "tab") // older: the code block (skips assistant)
	if m.focusID != m.blocks[0].id {
		t.Fatalf("tab should move up to the code block, focusID=%d", m.focusID)
	}
	m = press(m, "shift+tab")
	if m.focusID != m.blocks[2].id {
		t.Fatal("shift+tab should move back down to the result block")
	}
	m = press(m, "tab") // back on the code block for the toggle checks below
	if m.focusID != m.blocks[0].id {
		t.Fatal("tab should return to the code block")
	}

	wasCollapsed := m.blocks[0].collapsed
	m = press(m, "enter") // enter toggles the focused block, no submit
	if m.blocks[0].collapsed == wasCollapsed {
		t.Fatal("enter on focused block should toggle it")
	}
	if len(m.blocks) != 3 {
		t.Fatal("enter on focused block must not submit")
	}

	// Enter with typed text submits even while a block is focused.
	m.input.SetValue("hello")
	state := m.blocks[0].collapsed
	m = press(m, "enter")
	if m.blocks[0].collapsed != state {
		t.Fatal("enter while composing must not toggle the focused block")
	}
	if len(m.blocks) != 4 || m.blocks[3].kind != "user" {
		t.Fatal("enter while composing should submit")
	}

	// Identity: appends must not shift focus or collapse state.
	focused := m.focusID
	m.addEvent(Event{Kind: "result", Text: long})
	if m.focusID != focused || m.blocks[0].collapsed == wasCollapsed {
		t.Error("append shifted focus/collapse state")
	}
}

func TestHitRangesRecomputedOnResize(t *testing.T) {
	m := testModel(t)
	m.addEvent(Event{Kind: "result", Text: "only line"})
	m.blocks[0].collapsed = false // expand: header + box
	m.refresh()
	span := m.ranges[0].end - m.ranges[0].start
	if span != 4 { // header + 3 box lines
		t.Fatalf("expanded 1-line result should span 4 lines, got %d", span)
	}
	m.resize(40, 10)
	if len(m.ranges) != 1 || m.ranges[0].end-m.ranges[0].start != span {
		t.Errorf("resize should recompute ranges, got %+v", m.ranges)
	}
}

func TestParseStyle(t *testing.T) {
	good := []string{"", "5", "5:bold", "#ffaf00:bold", "250:236", "#fff:#000:faint", "1:bold:italic"}
	for _, spec := range good {
		if _, err := parseStyle(spec); err != nil {
			t.Errorf("parseStyle(%q): %v", spec, err)
		}
	}
	bad := []string{"reddish", "5:bold:blink", "#ggg", "1:2:3", "300"}
	for _, spec := range bad {
		if _, err := parseStyle(spec); err == nil {
			t.Errorf("parseStyle(%q): want error", spec)
		}
	}
}

func TestThemeAndKeymapValidation(t *testing.T) {
	th := defaultTheme()
	if err := th.apply(map[string]string{"user": "#ffaf00:bold"}); err != nil {
		t.Errorf("valid theme override: %v", err)
	}
	if err := th.apply(map[string]string{"theem": "5"}); err == nil {
		t.Error("unknown theme token should fail loud")
	}
	keys := defaultKeymap()
	if err := applyKeymap(keys, map[string]string{"history_inspect": "ctrl+g"}); err != nil {
		t.Errorf("valid keymap override: %v", err)
	}
	if keys["history_inspect"] != "ctrl+g" {
		t.Error("keymap override not applied")
	}
	if err := applyKeymap(keys, map[string]string{"histry": "ctrl+g"}); err == nil {
		t.Error("unknown keymap action should fail loud")
	}
}

// A failed code block is fed back to the model, which carries on; the
// turn is over only at "done". Ending it on the error froze the spinner
// and hid the recovery that followed.
func TestErrorEventKeepsTheTurnRunning(t *testing.T) {
	m := testModel(t)
	m.running = true
	m.addEvent(Event{Kind: "error", Text: "error: GoError: bash: exit status 126"})
	if !m.running {
		t.Fatal("a code error is not the end of the turn")
	}
	if got := m.blocks[len(m.blocks)-1].text; strings.Contains(got, "GoError") {
		t.Fatalf("runtime wrapper name shown: %q", got)
	}
	m.addEvent(Event{Kind: "done"})
	if m.running {
		t.Fatal("done ends the turn")
	}
}

// Tab with nothing focused starts at the NEWEST collapsible block, the
// one on screen, not the oldest at the top of the transcript.
func TestFocusStartsAtTheNewestBlock(t *testing.T) {
	m := testModel(t)
	// One step: three or more collapsed step rows fold into a single
	// row (fold.go), and the block cursor walks what is on screen.
	m.addEvent(Event{Kind: "code", Text: "console.log(1)"})
	m.addEvent(Event{Kind: "result", Text: "ok"})
	m.moveFocus(1)
	f := m.focusables()
	if m.focusID != m.blocks[f[len(f)-1].idx].id {
		t.Fatalf("first tab focused block id %d, want the newest %d", m.focusID, m.blocks[f[len(f)-1].idx].id)
	}
	m.moveFocus(1)
	if m.focusID != m.blocks[f[len(f)-2].idx].id {
		t.Fatalf("a second tab goes one older; got id %d", m.focusID)
	}
	m.moveFocus(-1)
	if m.focusID != m.blocks[f[len(f)-1].idx].id {
		t.Fatalf("shift+tab comes back to the newest; got id %d", m.focusID)
	}
	m2 := testModel(t)
	m2.addEvent(Event{Kind: "code", Text: "a"})
	m2.moveFocus(-1)
	if m2.focusID != m2.blocks[0].id {
		t.Fatal("shift+tab with nothing focused also starts at the newest")
	}
}
