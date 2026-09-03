package ui

// Keymap resolution, input editing, inspector overlay, scrolling.

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestQuitOnDefaultCtrlC(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	if hasQuit(d.press(keyCtrl('c'))) {
		t.Error("a single ctrl+c should only arm the quit")
	}
	if !strings.Contains(d.plain(), quitHint) {
		t.Errorf("first ctrl+c should show %q:\n%s", quitHint, d.plain())
	}
	if !hasQuit(d.press(keyCtrl('c'))) {
		t.Error("second ctrl+c should quit with the default keymap")
	}
}

func TestKeymapRebindQuit(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"quit": "ctrl+q"}, nil))
	d.press(keyCtrl('q'))
	if !strings.Contains(d.plain(), "press ctrl+q again to quit") {
		t.Errorf("hint should name the rebound key:\n%s", d.plain())
	}
	if !hasQuit(d.press(keyCtrl('q'))) {
		t.Error("ctrl+q twice should quit after rebind")
	}
	d.press(keyCtrl('c'))
	if hasQuit(d.press(keyCtrl('c'))) {
		t.Error("old quit key ctrl+c should be inert after rebind")
	}
}

func TestKeymapRebindCollapseToggle(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"collapse_toggle": "ctrl+g"}, nil))
	d.event("result", nLines(20))
	d.press(keyEnter()) // old key: enter no longer toggles, and empty submit is a no-op
	if !d.m.blocks[0].collapsed {
		t.Fatal("old enter binding should be inert after rebind")
	}
	d.press(keyCtrl('g'))
	if d.m.blocks[0].collapsed {
		t.Error("ctrl+g should toggle collapse after rebind")
	}
}

func TestKeymapEmptyKeyRejected(t *testing.T) {
	t.Parallel()
	keys := defaultKeymap()
	if err := applyKeymap(keys, map[string]string{"quit": "  "}); err == nil {
		t.Error("empty key for an action should fail loud")
	}
}

// --- input editing ---

func TestTypingFillsComposer(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("hi bough")
	if got := d.m.input.Value(); got != "hi bough" {
		t.Errorf("input value = %q, want %q", got, "hi bough")
	}
	if !strings.Contains(d.plain(), "hi bough") {
		t.Error("typed text not visible in the frame")
	}
}

func TestBackspaceEdits(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("abc")
	d.feed(tea.KeyPressMsg{Code: tea.KeyBackspace})
	if got := d.m.input.Value(); got != "ab" {
		t.Errorf("after backspace input = %q, want %q", got, "ab")
	}
}

func TestClearInputKey(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("scratch this")
	d.press(keyCtrl('l'))
	if got := d.m.input.Value(); got != "" {
		t.Errorf("ctrl+l should clear the composer, got %q", got)
	}
}

func TestEnterSendsLine(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("run tests")
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != "run tests" {
		t.Fatalf("send calls = %v, want [run tests]", d.sent)
	}
	if d.m.input.Value() != "" {
		t.Error("composer should reset after send")
	}
	if len(d.m.blocks) != 1 || d.m.blocks[0].kind != "user" || d.m.blocks[0].text != "run tests" {
		t.Errorf("user block not appended: %+v", d.m.blocks)
	}
	if !d.m.running {
		t.Error("model should be running after send")
	}
}

func TestEnterTrimsWhitespace(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("  padded  ")
	d.press(keyEnter())
	if len(d.sent) != 1 || d.sent[0] != "padded" {
		t.Errorf("send calls = %v, want [padded]", d.sent)
	}
}

func TestEmptyEnterIsNoop(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("   ")
	d.press(keyEnter())
	if len(d.sent) != 0 || len(d.m.blocks) != 0 || d.m.running {
		t.Errorf("blank enter should send nothing (sent=%v blocks=%d running=%v)",
			d.sent, len(d.m.blocks), d.m.running)
	}
}

// --- inspector overlay ---

func TestInspectorOpens(t *testing.T) {
	t.Parallel()
	h := histWith("/home/x/.bough/history/sess-42.jsonl", "first line", "second line")
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	d.press(keyCtrl('o'))
	if !d.m.inspecting {
		t.Fatal("ctrl+o should open the inspector")
	}
	p := d.plain()
	if !strings.Contains(p, "sess-42.jsonl") {
		t.Errorf("inspector missing history path:\n%s", p)
	}
	if !strings.Contains(p, "first line") || !strings.Contains(p, "second line") {
		t.Errorf("inspector missing entries:\n%s", p)
	}
	if !strings.Contains(p, "03:04:05") {
		t.Errorf("inspector missing entry timestamps:\n%s", p)
	}
}

func TestInspectorClosesOnSecondPress(t *testing.T) {
	t.Parallel()
	h := histWith("/tmp/s.jsonl", "one")
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	d.press(keyCtrl('o'))
	d.press(keyCtrl('o'))
	if d.m.inspecting {
		t.Error("second ctrl+o should close the inspector")
	}
}

func TestInspectorScrollsOverlayNotTranscript(t *testing.T) {
	t.Parallel()
	texts := make([]string, 100)
	for i := range texts {
		texts[i] = "entry"
	}
	h := histWith("/tmp/long.jsonl", texts...)
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	d.press(keyCtrl('o'))
	before := d.m.overlay.YOffset()
	vpBefore := d.m.vp.YOffset()
	d.press(keyUp())
	d.press(keyUp())
	if got := d.m.overlay.YOffset(); got != before-2 {
		t.Errorf("overlay YOffset = %d, want %d", got, before-2)
	}
	if d.m.vp.YOffset() != vpBefore {
		t.Error("transcript should not scroll while inspecting")
	}
	d.press(keyDown())
	if got := d.m.overlay.YOffset(); got != before-1 {
		t.Errorf("overlay YOffset after down = %d, want %d", got, before-1)
	}
}

func TestInspectorEnterDoesNotSend(t *testing.T) {
	t.Parallel()
	h := histWith("/tmp/s.jsonl", "one")
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	d.typeStr("queued")
	d.press(keyCtrl('o'))
	d.press(keyEnter())
	if len(d.sent) != 0 {
		t.Errorf("enter while inspecting must not send, sent=%v", d.sent)
	}
}

func TestInspectorEmptyHistory(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, fakeHist{path: "/tmp/empty.jsonl"}))
	d.press(keyCtrl('o'))
	if p := d.plain(); !strings.Contains(p, "(no entries yet)") {
		t.Errorf("empty history inspector missing placeholder:\n%s", p)
	}
}

func TestInspectorWithoutHistoryFlashes(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t) // no history service
	d.press(keyCtrl('o'))
	if d.m.inspecting {
		t.Error("inspector must not open without a history service")
	}
	if p := d.plain(); !strings.Contains(p, "no history service mounted") {
		t.Errorf("status bar should flash the missing-service note:\n%s", p)
	}
}

func TestInspectorRebound(t *testing.T) {
	t.Parallel()
	h := histWith("/tmp/s.jsonl", "one")
	d := newDrv(t, 80, 24, cfgWith(t, nil, map[string]string{"history_inspect": "ctrl+g"}, h))
	d.press(keyCtrl('o'))
	if d.m.inspecting {
		t.Error("old inspector key should be inert after rebind")
	}
	d.press(keyCtrl('g'))
	if !d.m.inspecting {
		t.Error("ctrl+g should open the inspector after rebind")
	}
}

// --- scrolling ---

// fillTranscript adds enough blocks to overflow the 22-line viewport.
func fillTranscript(d *drv, n int) {
	for range n {
		d.event("assistant", "message") // not "user": those would be recallable prompts
	}
}

func TestArrowKeysScroll(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 40)
	if !d.m.vp.AtBottom() {
		t.Fatal("transcript should start pinned to bottom")
	}
	bottom := d.m.vp.YOffset()
	d.press(keyUp())
	if got := d.m.vp.YOffset(); got != bottom-1 {
		t.Errorf("up should scroll 1 line, YOffset %d -> %d", bottom, got)
	}
	d.press(keyDown())
	if got := d.m.vp.YOffset(); got != bottom {
		t.Errorf("down should scroll back, YOffset = %d want %d", got, bottom)
	}
}

func TestPageKeysScroll(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 60)
	bottom := d.m.vp.YOffset()
	d.press(keyPgUp())
	up := d.m.vp.YOffset()
	if up >= bottom {
		t.Errorf("pgup should scroll up a page, YOffset %d -> %d", bottom, up)
	}
	d.press(keyPgDown())
	if got := d.m.vp.YOffset(); got != bottom {
		t.Errorf("pgdown should return to bottom, YOffset = %d want %d", got, bottom)
	}
}

func TestWheelScroll(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 40)
	bottom := d.m.vp.YOffset()
	d.feed(tea.MouseWheelMsg{Button: tea.MouseWheelUp})
	if got := d.m.vp.YOffset(); got >= bottom {
		t.Errorf("wheel up should scroll up, YOffset %d -> %d", bottom, got)
	}
	d.feed(tea.MouseWheelMsg{Button: tea.MouseWheelDown})
	if got := d.m.vp.YOffset(); got != bottom {
		t.Errorf("wheel down should return to bottom, YOffset = %d want %d", got, bottom)
	}
}

func TestLongTranscriptScrollback(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("user", "the very first message")
	fillTranscript(d, 200)
	if strings.Contains(d.plain(), "the very first message") {
		t.Fatal("first message should have scrolled out of view")
	}
	for i := 0; i < 50 && !d.m.vp.AtTop(); i++ {
		d.press(keyPgUp())
	}
	if !d.m.vp.AtTop() {
		t.Fatal("repeated pgup should reach the top")
	}
	if !strings.Contains(d.plain(), "the very first message") {
		t.Errorf("scrollback to top should show the first message:\n%s", d.plain())
	}
}

func TestNewEventWhileScrolledUpShowsCue(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 40)
	d.press(keyPgUp())
	if p := d.plain(); !strings.Contains(p, "scrolled ↑") {
		t.Errorf("scrolled-up state should be visible in the status bar:\n%s", p)
	}
	d.event("assistant", "fresh reply")
	if d.m.vp.AtBottom() {
		t.Error("a new event must not yank a scrolled-up reader to the bottom")
	}
	p := d.plain()
	if !strings.Contains(p, "↓ new output") || !strings.Contains(p, "scrolled ↑") {
		t.Errorf("new output below should be cued:\n%s", p)
	}
	for i := 0; i < 50 && !d.m.vp.AtBottom(); i++ {
		d.press(keyPgDown())
	}
	p = d.plain()
	if strings.Contains(p, "↓ new output") || strings.Contains(p, "scrolled ↑") {
		t.Errorf("cue should clear at the bottom:\n%s", p)
	}
	if !strings.Contains(p, "fresh reply") {
		t.Error("fresh reply should be visible at the bottom")
	}
}

func TestNewEventAtBottomStaysPinned(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 40)
	d.event("assistant", "fresh reply")
	if !d.m.vp.AtBottom() || !strings.Contains(d.plain(), "fresh reply") {
		t.Error("a reader at the bottom should follow new output")
	}
}

// ctrl+t pins the latest todo list above the composer; again unpins;
// without a list it only flashes a hint.
func TestTodoTogglePinsList(t *testing.T) {
	d := defaultDrv(t)
	d.press(tea.KeyPressMsg{Code: 't', Mod: tea.ModCtrl})
	if d.m.todoPinned || !strings.Contains(d.plain(), "no todo list yet") {
		t.Fatalf("ctrl+t without todos should flash a hint:\n%s", d.plain())
	}
	d.event("todo", "[ ] 1. write tests\n[x] 2. read code")
	d.event("assistant", "filler one")
	d.event("assistant", "filler two")
	d.press(tea.KeyPressMsg{Code: 't', Mod: tea.ModCtrl})
	p := d.plain()
	if !d.m.todoPinned || !strings.Contains(p, "todo · ctrl+t hides") {
		t.Fatalf("ctrl+t should pin the todo panel:\n%s", p)
	}
	lines := strings.Split(p, "\n")
	hdr := -1
	for i, l := range lines {
		if strings.Contains(l, "ctrl+t hides") {
			hdr = i
		}
	}
	if hdr < 0 || !strings.Contains(lines[hdr+1], "write tests") || !strings.Contains(lines[hdr+2], "read code") {
		t.Fatalf("panel rows should sit under the header at the pane bottom:\n%s", p)
	}
	d.event("todo", "[x] 1. write tests\n[x] 2. read code\n[ ] 3. ship")
	if !strings.Contains(d.plain(), "3. ship") {
		t.Fatalf("pinned panel should track todo events:\n%s", d.plain())
	}
	d.press(tea.KeyPressMsg{Code: 't', Mod: tea.ModCtrl})
	if d.m.todoPinned || strings.Contains(d.plain(), "ctrl+t hides") {
		t.Fatal("second ctrl+t should unpin")
	}
}
