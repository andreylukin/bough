package main

// Choosing a provider for a first run: the embedded default must not
// insist on a key the user does not have when they are holding one
// that works.

import (
	"testing"

	"github.com/andreylukin/bough/kernel"
)

func defaultRows() []kernel.Row {
	return []kernel.Row{
		{ID: "llm", Plugin: "llm-anthropic", Config: map[string]any{"model": "claude-sonnet-5"}},
		{ID: "loop", Plugin: "loop"},
	}
}

// The env this test controls; each case starts from all-unset.
func clearKeys(t *testing.T) {
	t.Helper()
	for _, c := range providerChoices {
		t.Setenv(c.env, "")
	}
}

func TestPicksTheProviderWhoseKeyIsSet(t *testing.T) {
	clearKeys(t)
	t.Setenv("OPENROUTER_API_KEY", "sk-or-test")
	rows, why := pickProvider(defaultRows())
	if rows[0].Plugin != "llm-openrouter" {
		t.Fatalf("row should have swapped to openrouter, got %q", rows[0].Plugin)
	}
	if m, _ := rows[0].Config["model"].(string); m != "anthropic/claude-sonnet-5" {
		t.Errorf("a swapped provider needs its own model, got %q", m)
	}
	if why == "" {
		t.Error("the swap should be explained on stderr")
	}
}

// The default's own key present: nothing to do.
func TestKeepsTheDefaultWhenItsKeyIsPresent(t *testing.T) {
	clearKeys(t)
	t.Setenv("ANTHROPIC_API_KEY", "sk-ant-test")
	t.Setenv("OPENROUTER_API_KEY", "sk-or-test")
	rows, why := pickProvider(defaultRows())
	if rows[0].Plugin != "llm-anthropic" || why != "" {
		t.Fatalf("the configured provider works; leave it (%q, %q)", rows[0].Plugin, why)
	}
}

// No key at all: leave the row alone, so the notice names the provider
// the config actually asked for.
func TestNoKeysLeavesTheRowAlone(t *testing.T) {
	clearKeys(t)
	rows, why := pickProvider(defaultRows())
	if rows[0].Plugin != "llm-anthropic" || why != "" {
		t.Fatalf("with no keys the default stands (%q, %q)", rows[0].Plugin, why)
	}
}

// Preference order is the list's order, not the environment's.
func TestPreferenceOrder(t *testing.T) {
	clearKeys(t)
	t.Setenv("CEREBRAS_API_KEY", "sk-cb")
	t.Setenv("OPENAI_API_KEY", "sk-oa")
	rows, _ := pickProvider(defaultRows())
	if rows[0].Plugin != "llm-openai" {
		t.Fatalf("openai outranks cerebras, got %q", rows[0].Plugin)
	}
}

// The caller's rows must not be mutated: load() may be called again on
// a hot reload, and the embedded bytes are shared.
func TestPickDoesNotMutateInput(t *testing.T) {
	clearKeys(t)
	t.Setenv("OPENROUTER_API_KEY", "sk-or-test")
	in := defaultRows()
	pickProvider(in)
	if in[0].Plugin != "llm-anthropic" {
		t.Fatalf("input rows were mutated: %q", in[0].Plugin)
	}
	if m, _ := in[0].Config["model"].(string); m != "claude-sonnet-5" {
		t.Fatalf("input config was mutated: %q", m)
	}
}

func TestNoLLMRowIsNotAnError(t *testing.T) {
	clearKeys(t)
	t.Setenv("OPENROUTER_API_KEY", "sk-or-test")
	rows, why := pickProvider([]kernel.Row{{ID: "loop", Plugin: "loop"}})
	if len(rows) != 1 || why != "" {
		t.Fatalf("a tree with no llm row is left as it is (%v, %q)", rows, why)
	}
}
