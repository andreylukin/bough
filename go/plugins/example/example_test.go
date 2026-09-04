package example

// The example plugin is documentation, so it is tested like anything
// else: a walkthrough whose code does not run teaches the wrong thing.

import (
	"testing"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/commands"
)

func TestCountsWordsAboveMinLength(t *testing.T) {
	c := &counter{minLength: 4}
	got := c.Count("the quick brown fox jumps over the lazy dog the fox")
	for _, short := range []string{"the", "fox", "dog"} {
		if _, ok := got[short]; ok {
			t.Errorf("%q is below min_length and should have been skipped: %v", short, got)
		}
	}
	if got["quick"] != 1 || got["jumps"] != 1 {
		t.Errorf("long words should be counted: %v", got)
	}
}

func TestCountIsCaseFoldedAndPunctuationSplit(t *testing.T) {
	c := &counter{minLength: 1}
	got := c.Count("Ship, ship; SHIP! don't")
	if got["ship"] != 3 {
		t.Errorf("case and punctuation should not split a word: %v", got)
	}
	if got["don't"] != 1 {
		t.Errorf("an apostrophe belongs to the word: %v", got)
	}
}

func TestRenderIsStable(t *testing.T) {
	out := render(map[string]int{"b": 2, "a": 2, "c": 9})
	want := "   9  c\n   2  a\n   2  b"
	if out != want {
		t.Errorf("render should sort by count then alphabetically:\n%q", out)
	}
	if render(map[string]int{}) != "no words" {
		t.Error("an empty count should say so")
	}
}

// Config is validated at mount: a bad value fails this row and leaves
// the rest of the tree running.
func TestBadConfigFailsTheRow(t *testing.T) {
	for _, bad := range []any{0, -1, "four", 1.5} {
		ctx := kernel.NewContext()
		if err := (plugin{}).Apply(ctx, map[string]any{"min_length": bad}); err == nil {
			t.Errorf("min_length %v (%T) should be rejected", bad, bad)
		}
	}
}

func TestMountsWithNoOptionalServices(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("the plugin must mount without codemode or commands: %v", err)
	}
	if _, err := kernel.Get[*counter](ctx, "wordcount"); err != nil {
		t.Fatalf("the service should be provided anyway: %v", err)
	}
}

// With codemode and commands mounted it registers into both, and
// unmounting takes both registrations away again.
func TestRegistersAndCleansUp(t *testing.T) {
	ctx := kernel.NewContext()
	tools := &fakeTools{fns: map[string]any{}}
	ctx.Provide("codemode", tools)
	reg := commands.NewRegistry()
	ctx.Provide("commands", reg)

	if err := (plugin{}).Apply(ctx, map[string]any{"min_length": 4}); err != nil {
		t.Fatal(err)
	}
	if tools.fns["wordcount"] == nil {
		t.Error("the codemode tool should be registered")
	}
	if _, err := reg.Run("wordcount", "alpha alpha beta"); err != nil {
		t.Errorf("the command should be registered: %v", err)
	}

	ctx.Unmount()
	if tools.fns["wordcount"] != nil {
		t.Error("unmount should remove the codemode tool")
	}
	if _, err := reg.Run("wordcount", "x"); err == nil {
		t.Error("unmount should remove the command")
	}
}

type fakeTools struct{ fns map[string]any }

func (f *fakeTools) RegisterTool(name string, fn any) { f.fns[name] = fn }
