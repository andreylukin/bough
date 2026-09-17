package main

import (
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// Esc SIGINTs a web session's child and the next prompt resumes it:
// the /model chosen before must come back as overrides, the last one
// winning, or the resumed turn runs bough.yml's provider (the 401).
func TestResumeReappliesChosenModel(t *testing.T) {
	p := filepath.Join(t.TempDir(), "s.jsonl")
	if err := os.WriteFile(p, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	for _, sets := range [][]any{{"llm.plugin=llm-anthropic"}, {"llm.plugin=llm-openai", "llm.model=gpt-5.4-mini"}} {
		if _, err := history.AppendFile(p, "model", map[string]any{"sets": sets}); err != nil {
			t.Fatal(err)
		}
	}
	got := resumedModelSets(p)
	want := []string{"llm.plugin=llm-anthropic", "llm.plugin=llm-openai", "llm.model=gpt-5.4-mini"}
	if !slices.Equal(got, want) {
		t.Fatalf("sets = %v, want %v", got, want)
	}
}
