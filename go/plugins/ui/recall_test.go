package ui

// Prompt recall the way Claude Code does it: history outlives the
// session, Up/Down walk visual rows before they walk history, ctrl+p
// and ctrl+n are the same keys, and esc does not lose a draft.

import (
	"strings"
	"testing"
)

// pastDrv is a driver whose launcher offers earlier sessions' prompts.
func pastDrv(t *testing.T, past ...string) *drv {
	t.Helper()
	cfg := cfgWith(t, nil, nil, nil)
	cfg.past = func() []string { return past } // newest first, as RecentPrompts returns
	return newDrv(t, 80, 24, cfg)
}

// The whole point: a brand-new session in a directory recalls what was
// typed there before. Every launch starts a new session now, so
// session-only history meant an Up arrow that never did anything.
func TestRecallReachesEarlierSessions(t *testing.T) {
	t.Parallel()
	d := pastDrv(t, "newest from before", "older from before")
	d.press(keyUp())
	if got := d.m.input.Value(); got != "newest from before" {
		t.Fatalf("first up should land on the most recent past prompt, got %q", got)
	}
	d.press(keyUp())
	if got := d.m.input.Value(); got != "older from before" {
		t.Fatalf("second up walks further back, got %q", got)
	}
}

// This session's prompts are newer than any past one, so Up meets them
// first.
func TestThisSessionOutranksThePast(t *testing.T) {
	t.Parallel()
	d := pastDrv(t, "from before")
	d.say("typed just now")
	d.press(keyUp())
	if got := d.m.input.Value(); got != "typed just now" {
		t.Fatalf("this session's prompt comes first, got %q", got)
	}
	d.press(keyUp())
	if got := d.m.input.Value(); got != "from before" {
		t.Fatalf("then the past, got %q", got)
	}
}

// The past is read once, not on every keystroke.
func TestPastPromptsReadOnce(t *testing.T) {
	t.Parallel()
	calls := 0
	cfg := cfgWith(t, nil, nil, nil)
	cfg.past = func() []string { calls++; return []string{"a", "b"} }
	d := newDrv(t, 80, 24, cfg)
	for range 4 {
		d.press(keyUp())
	}
	if calls != 1 {
		t.Fatalf("prompt history read %d times, want 1", calls)
	}
}

// Claude Code's rule: a draft spanning more than one VISUAL row moves
// the cursor first. A single long line soft-wraps, so judging by
// logical line sent a wrapped paragraph straight to history with the
// cursor several rows down.
func TestUpWalksWrappedRowsBeforeHistory(t *testing.T) {
	t.Parallel()
	d := pastDrv(t, "from before")
	d.typeStr(strings.Repeat("word ", 60)) // one logical line, many rows
	if d.m.input.LineCount() != 1 {
		t.Fatalf("precondition: one logical line, got %d", d.m.input.LineCount())
	}
	if d.m.input.LineInfo().Height < 2 {
		t.Fatalf("precondition: the draft should wrap, height %d", d.m.input.LineInfo().Height)
	}
	d.press(keyUp())
	if v := d.m.input.Value(); !strings.HasPrefix(v, "word ") {
		t.Fatalf("up inside a wrapped line must move the cursor, not recall; got %q", v)
	}
	// Walk to the top row, then one more press recalls.
	for range d.m.input.LineInfo().Height {
		d.press(keyUp())
	}
	if got := d.m.input.Value(); got != "from before" {
		t.Fatalf("up from the first visual row should recall, got %q", got)
	}
}

func TestCtrlPAndCtrlNAreUpAndDown(t *testing.T) {
	t.Parallel()
	d := pastDrv(t, "one")
	d.press(keyCtrl('p'))
	if got := d.m.input.Value(); got != "one" {
		t.Fatalf("ctrl+p should recall like up, got %q", got)
	}
	d.press(keyCtrl('n'))
	if got := d.m.input.Value(); got != "" {
		t.Fatalf("ctrl+n should come back to the draft like down, got %q", got)
	}
}

// esc clears a draft; Up brings it back, so clearing is undoable.
func TestEscClearedDraftIsRecallable(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.typeStr("half-written thought")
	d.press(keyEsc())
	if d.m.input.Value() != "" {
		t.Fatal("esc should clear the draft")
	}
	d.press(keyUp())
	if got := d.m.input.Value(); got != "half-written thought" {
		t.Fatalf("up should bring back the cleared draft, got %q", got)
	}
}

// Escaping out of a recalled prompt must not append it again: it is
// already in history.
func TestEscOnRecalledPromptAddsNothing(t *testing.T) {
	t.Parallel()
	d := pastDrv(t, "from before")
	d.press(keyUp())
	d.press(keyEsc())
	if n := len(d.m.comp.dropped); n != 0 {
		t.Fatalf("a recalled prompt should not be re-recorded, got %d dropped", n)
	}
	if got := d.m.prompts(); len(got) != 1 {
		t.Fatalf("history should still hold one prompt, got %v", got)
	}
}
