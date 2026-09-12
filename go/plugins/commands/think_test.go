package commands

import (
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

// fakeEffort is the llm row's Efforter seam.
type fakeEffort struct {
	level string
	fail  error
}

func (f *fakeEffort) Effort() string { return f.level }
func (f *fakeEffort) SetEffort(l string) error {
	if f.fail != nil {
		return f.fail
	}
	f.level = l
	return nil
}

func thinkCtx(t *testing.T, e any) *kernel.Context {
	t.Helper()
	ctx := kernel.NewContext()
	if e != nil {
		ctx.Provide("llm", e)
	}
	return ctx
}

func TestThinkShowsCurrentLevelAndChoices(t *testing.T) {
	t.Parallel()
	out, err := runThink(thinkCtx(t, &fakeEffort{level: "high"}), "")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "thinking: high") {
		t.Errorf("bare /think should report the level, got %q", out)
	}
	for _, lvl := range []string{"off", "low", "medium", "high", "xhigh"} {
		if !strings.Contains(out, lvl) {
			t.Errorf("bare /think should list %q: %s", lvl, out)
		}
	}
}

// A provider that has never been told a level reports the provider's
// own default rather than an empty string.
func TestThinkNamesTheDefaultWhenUnset(t *testing.T) {
	t.Parallel()
	out, err := runThink(thinkCtx(t, &fakeEffort{}), "")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "default") {
		t.Errorf("an unset level should say so, got %q", out)
	}
}

func TestThinkSetsTheLevel(t *testing.T) {
	t.Parallel()
	e := &fakeEffort{level: "low"}
	out, err := runThink(thinkCtx(t, e), "  XHigh ")
	if err != nil {
		t.Fatal(err)
	}
	if e.level != "xhigh" {
		t.Errorf("level = %q, want xhigh (case and space tolerant)", e.level)
	}
	if !strings.Contains(out, "xhigh") {
		t.Errorf("output should confirm the new level, got %q", out)
	}
}

func TestThinkRejectsAnUnknownLevelAndSaysWhatExists(t *testing.T) {
	t.Parallel()
	e := &fakeEffort{level: "low"}
	_, err := runThink(thinkCtx(t, e), "ludicrous")
	if err == nil {
		t.Fatal("an unknown level must error")
	}
	if !strings.Contains(err.Error(), "xhigh") {
		t.Errorf("the error should list the levels, got %v", err)
	}
	if e.level != "low" {
		t.Errorf("a rejected level must not change the row, got %q", e.level)
	}
}

// An llm row without the seam (a provider with no reasoning levels)
// must explain itself, not look broken.
func TestThinkWithoutTheSeamExplains(t *testing.T) {
	t.Parallel()
	_, err := runThink(thinkCtx(t, nil), "high")
	if err == nil || !strings.Contains(err.Error(), "reasoning levels") {
		t.Errorf("want a clear explanation, got %v", err)
	}
}
