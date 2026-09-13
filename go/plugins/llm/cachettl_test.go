package llm

import "testing"

func TestCacheTTL(t *testing.T) {
	t.Parallel()
	for model, want := range map[string]int{
		"anthropic/claude-opus-5": 300, "openai/gpt-6-astra": 1800, "~openai/gpt-5.6": 1800,
		"openai/gpt-5.4": 300, "": 300,
	} {
		if got := int(CacheTTL(model).Seconds()); got != want {
			t.Errorf("CacheTTL(%q) = %ds, want %ds", model, got, want)
		}
	}
}
