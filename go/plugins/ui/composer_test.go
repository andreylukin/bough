package ui

// Multi-line composer: newline keys, paste, prompt recall, home/end,
// wrapping, and the mouse never writing into it (composer.go).

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func keyShiftEnter() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeyEnter, Mod: tea.ModShift} }
func keyAltEnter() tea.KeyPressMsg   { return tea.KeyPressMsg{Code: tea.KeyEnter, Mod: tea.ModAlt} }
func keyHome() tea.KeyPressMsg       { return tea.KeyPressMsg{Code: tea.KeyHome} }
func keyEnd() tea.KeyPressMsg        { return tea.KeyPressMsg{Code: tea.KeyEnd} }

// say submits one prompt and ends its turn.
func (d *drv) say(line string) {
	d.typeStr(line)
	d.press(keyEnter())
	d.event("done", "")
}

func TestShiftEnterInsertsNewlineEnterSubmits(t *testing.T) {
	t.Parallel()
	for _, nl := range []tea.KeyPressMsg{keyShiftEnter(), keyAltEnter(), keyCtrl('j')} {
		d := defaultDrv(t)
		d.typeStr("first")
		d.feed(nl)
		d.typeStr("second")
		if got := d.m.input.Value(); got != "first\nsecond" {
			t.Fatalf("%s: value = %q, want two lines", nl, got)
		}
		if d.m.input.Height() != 2 || d.m.vp.Height() != 24-1-2 {
			t.Errorf("%s: composer %d rows, transcript %d rows; want 2 and 21", nl, d.m.input.Height(), d.m.vp.Height())
		}
		d.press(keyEnter())
		if len(d.sent) != 1 || d.sent[0] != "first\nsecond" {
			t.Errorf("%s: sent %q, want the two-line prompt", nl, d.sent)
		}
		if d.m.input.Height() != 1 || d.m.vp.Height() != 22 {
			t.Errorf("%s: layout should shrink back after submit", nl)
		}
	}
}

func TestPasteKeepsNewlines(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: "line a\nline b\nline c"})
	if got := d.m.input.Value(); got != "line a\nline b\nline c" {
		t.Fatalf("pasted value = %q", got)
	}
	if d.m.input.Height() != 3 {
		t.Errorf("composer should grow to 3 rows, got %d", d.m.input.Height())
	}
}

func TestComposerHeightCaps(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: strings.Repeat("x\n", 30)})
	if d.m.input.Height() != composerMaxLines {
		t.Errorf("composer height = %d, want cap %d", d.m.input.Height(), composerMaxLines)
	}
	if d.m.vp.Height() != 24-1-composerMaxLines {
		t.Errorf("transcript height = %d", d.m.vp.Height())
	}
}

func TestUpDownRecallPrompts(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.say("first prompt")
	d.say("second prompt")
	d.typeStr("dr")
	d.press(keyUp())
	if got := d.m.input.Value(); got != "second prompt" {
		t.Fatalf("up should recall the newest prompt, got %q", got)
	}
	d.press(keyUp())
	if got := d.m.input.Value(); got != "first prompt" {
		t.Fatalf("second up should recall the older prompt, got %q", got)
	}
	d.press(keyUp())
	if got := d.m.input.Value(); got != "first prompt" {
		t.Fatalf("up past the oldest should stay, got %q", got)
	}
	d.press(keyDown())
	if got := d.m.input.Value(); got != "second prompt" {
		t.Fatalf("down should go forward, got %q", got)
	}
	d.press(keyDown())
	if got := d.m.input.Value(); got != "dr" {
		t.Fatalf("down past the newest should restore the draft, got %q", got)
	}
	d.press(keyEnter())
	if len(d.sent) != 3 || d.sent[2] != "dr" {
		t.Errorf("sent = %q", d.sent)
	}
}

func TestRecallReplayedHistory(t *testing.T) {
	t.Parallel()
	h := histWith("/tmp/s.jsonl", "from last time")
	h.entries[0].Kind = "input"
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, h))
	d.press(keyUp())
	if got := d.m.input.Value(); got != "from last time" {
		t.Errorf("resumed session's inputs should be recallable, got %q", got)
	}
}

func TestUpInsideDraftMovesCursor(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.say("older")
	d.typeStr("a")
	d.feed(keyShiftEnter())
	d.typeStr("b")
	d.press(keyUp()) // line 1 -> line 0, no recall
	if d.m.input.Line() != 0 || d.m.input.Value() != "a\nb" {
		t.Fatalf("up inside a draft should move the cursor: line %d value %q", d.m.input.Line(), d.m.input.Value())
	}
	d.press(keyUp()) // on the first line: recall
	if got := d.m.input.Value(); got != "older" {
		t.Errorf("up on the first line should recall, got %q", got)
	}
}

func TestUpWithNothingToRecallScrolls(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 40)
	bottom := d.m.vp.YOffset()
	d.press(keyUp())
	if got := d.m.vp.YOffset(); got != bottom-1 {
		t.Errorf("no prompts: up should fall through to scroll, YOffset %d -> %d", bottom, got)
	}
}

func TestHomeEndJumpTranscriptWhenComposerEmpty(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	fillTranscript(d, 40)
	d.press(keyHome())
	if d.m.vp.YOffset() != 0 {
		t.Errorf("home should jump to the top, YOffset = %d", d.m.vp.YOffset())
	}
	d.press(keyEnd())
	if !d.m.vp.AtBottom() {
		t.Error("end should jump to the bottom")
	}
	d.press(keyHome())
	d.typeStr("abc")
	d.press(keyEnd())
	if d.m.vp.YOffset() != 0 {
		t.Error("end with a draft edits the line, not the transcript")
	}
	d.press(keyHome())
	d.typeStr("X")
	if got := d.m.input.Value(); got != "Xabc" {
		t.Errorf("home with a draft should go to line start, got %q", got)
	}
}

func TestEmacsEditingKeys(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("hello world")
	d.press(keyCtrl('w'))
	if got := d.m.input.Value(); got != "hello " {
		t.Fatalf("ctrl+w: %q", got)
	}
	d.press(keyCtrl('u'))
	if got := d.m.input.Value(); got != "" {
		t.Fatalf("ctrl+u: %q", got)
	}
	d.typeStr("abc")
	d.press(keyCtrl('a'))
	d.press(keyCtrl('k'))
	if got := d.m.input.Value(); got != "" {
		t.Fatalf("ctrl+a then ctrl+k: %q", got)
	}
	d.typeStr("xy")
	d.press(keyCtrl('a'))
	d.press(keyCtrl('e'))
	d.typeStr("z")
	if got := d.m.input.Value(); got != "xyz" {
		t.Fatalf("ctrl+e: %q", got)
	}
}

func TestLongPromptWrapsInTranscript(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	long := strings.Repeat("word ", 30) + "TAILEND"
	d.say(long)
	if !strings.Contains(d.plain(), "TAILEND") {
		t.Errorf("long prompt should wrap, not clip at the right edge:\n%s", d.plain())
	}
}

func TestMouseNeverWritesComposer(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("result", nLines(10))
	row := frameRow(d, "▸ result (10 lines)")
	d.feed(tea.MouseClickMsg{X: 3, Y: row, Button: tea.MouseLeft})
	d.feed(tea.MouseReleaseMsg{X: 3, Y: row, Button: tea.MouseLeft})
	if d.m.blocks[0].collapsed {
		t.Error("click should expand the block")
	}
	d.feed(tea.MouseClickMsg{X: 3, Y: row, Button: tea.MouseLeft})
	d.feed(tea.MouseMotionMsg{X: 8, Y: row, Button: tea.MouseLeft})
	d.feed(tea.MouseReleaseMsg{X: 8, Y: row, Button: tea.MouseLeft})
	if d.m.blocks[0].collapsed {
		t.Error("a drag is a selection, not a click: the block must stay as it was")
	}
	if got := d.m.input.Value(); got != "" {
		t.Errorf("mouse activity wrote into the composer: %q", got)
	}
}

// A long single line soft-wraps; the composer must grow by VISUAL rows
// (sizing by logical lines left the first row scrolled out of view).
func TestComposerGrowsForWrappedLine(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.feed(tea.PasteMsg{Content: strings.Repeat("word ", 40)}) // ~200 cells on an 80-wide screen
	if got := d.m.input.Height(); got < 3 {
		t.Fatalf("composer height = %d for a 3-row wrapped line", got)
	}
	if d.m.vp.Height() != 24-1-d.m.input.Height() {
		t.Errorf("transcript height %d does not account for the composer's %d rows", d.m.vp.Height(), d.m.input.Height())
	}
	if !strings.Contains(d.m.input.View(), "word word") {
		t.Fatalf("first row not visible:\n%s", d.m.input.View())
	}
}
