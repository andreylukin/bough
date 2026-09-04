package llm

// A provider that cannot run says so before a turn is taken, and says
// what to do about it. This is the first thing a new user hits.

import (
	"strings"
	"testing"
)

func TestMissingKeyNamesTheFix(t *testing.T) {
	err := MissingKey("llm-anthropic", "ANTHROPIC_API_KEY")
	got := err.Error()
	for _, want := range []string{
		"llm-anthropic",     // which row
		"ANTHROPIC_API_KEY", // which variable
		"~/.bough/env",      // where it goes
		"/model",            // how to use something else
	} {
		if !strings.Contains(got, want) {
			t.Errorf("the missing-key error should mention %q:\n%s", want, got)
		}
	}
}

// Every shipped provider answers Ready, and answers it without a
// network call: this runs at mount, on every launch.
func TestProvidersReportReadiness(t *testing.T) {
	for _, tc := range []struct {
		name string
		env  string
		p    Ready
	}{
		{"llm-anthropic", "ANTHROPIC_API_KEY", &anthropicLLM{}},
		{"llm-openai", "OPENAI_API_KEY", &openaiLLM{}},
		{"llm-openrouter", "OPENROUTER_API_KEY", &openrouterLLM{}},
		{"llm-cerebras", "CEREBRAS_API_KEY", &cerebrasLLM{}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv(tc.env, "")
			err := tc.p.Ready()
			if err == nil {
				t.Fatalf("%s with no %s should not report ready", tc.name, tc.env)
			}
			if !strings.Contains(err.Error(), tc.env) {
				t.Errorf("error should name %s: %v", tc.env, err)
			}
			// Idempotent: the ui asks at mount, the loop asks again on
			// the first call.
			if second := tc.p.Ready(); second == nil || second.Error() != err.Error() {
				t.Errorf("Ready must be stable, got %v then %v", err, second)
			}
		})
	}
}

func TestReadyPassesWithAKey(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", "sk-test-not-used")
	if err := (&openrouterLLM{}).Ready(); err != nil {
		t.Fatalf("a configured provider is ready: %v", err)
	}
}
