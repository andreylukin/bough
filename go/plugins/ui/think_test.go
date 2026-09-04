package ui

import (
	"strings"
	"testing"
)

// stubEfforter is a provider whose thinking level can be changed.
type stubEfforter struct{ level string }

func (s *stubEfforter) Effort() string { return s.level }
func (s *stubEfforter) SetEffort(l string) error {
	s.level = l
	return nil
}

// shift+tab at rest walks the levels, provider default first, and says
// what it landed on.
func TestCycleThinking(t *testing.T) {
	m := testModel(t)
	e := &stubEfforter{}
	cfg := m.cfg.Load()
	cfg.effort = e
	m.cfg.Store(cfg)

	want := []string{"off", "low", "medium", "high", "xhigh", "default"}
	for _, level := range want {
		note := m.cycleThinking()
		if !strings.Contains(note, level) {
			t.Fatalf("note %q does not name %q", note, level)
		}
	}
	if e.level != "" {
		t.Fatalf("the cycle did not return to the provider default, got %q", e.level)
	}
}

// A provider that cannot reason says so rather than silently doing
// nothing.
func TestCycleThinkingWithoutSupport(t *testing.T) {
	m := testModel(t)
	if note := m.cycleThinking(); !strings.Contains(note, "no thinking level") {
		t.Fatalf("note = %q", note)
	}
}

// The chip appears only once the level has been changed: an untouched
// session keeps its status bar clean.
func TestThinkChipOnlyWhenSet(t *testing.T) {
	cfg := &uiCfg{}
	if got := thinkChip(cfg); got != "" {
		t.Fatalf("chip without a provider: %q", got)
	}
	e := &stubEfforter{}
	cfg.effort = e
	if got := thinkChip(cfg); got != "" {
		t.Fatalf("chip at the provider default: %q", got)
	}
	e.level = "high"
	if got := thinkChip(cfg); got != "think high" {
		t.Fatalf("chip = %q", got)
	}
}

// Reasoning streams into ONE collapsed block that grows, and the final
// text replaces it instead of stacking a second copy.
func TestThinkingBlockStreamsAndSettles(t *testing.T) {
	m := testModel(t)
	m.addEvent(Event{Kind: "thinking-delta", Text: "let me "})
	m.addEvent(Event{Kind: "thinking-delta", Text: "check the tests"})
	if n := len(m.blocks); n != 1 {
		t.Fatalf("deltas made %d blocks, want 1", n)
	}
	b := m.blocks[0]
	if b.kind != "thinking" || !b.live || !b.collapsed || b.text != "let me check the tests" {
		t.Fatalf("live thinking block = %+v", b)
	}
	if head := m.header(&m.blocks[0], m.cfg.Load().theme); !strings.Contains(head, "thinking…") {
		t.Fatalf("live header should say it is still arriving: %q", head)
	}

	m.addEvent(Event{Kind: "thinking", Text: "let me check the tests, then answer"})
	if n := len(m.blocks); n != 1 {
		t.Fatalf("the final text stacked a second block (%d)", n)
	}
	if b := m.blocks[0]; b.live || b.text != "let me check the tests, then answer" {
		t.Fatalf("settled block = %+v", b)
	}

	// The reply that follows is its own block, not an append to the
	// thinking one.
	m.addEvent(Event{Kind: "assistant-delta", Text: "the tests pass"})
	if n := len(m.blocks); n != 2 || m.blocks[1].kind != "assistant" {
		t.Fatalf("blocks = %+v", m.blocks)
	}
}
