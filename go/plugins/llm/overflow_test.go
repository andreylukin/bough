package llm

// Outgrowing the context window is a dead end, not a hiccup: it is a
// 400 so it is never retried, and every later turn sends more history
// and fails identically. Recognising it is what stops a session being
// silently bricked.

import (
	"errors"
	"strings"
	"testing"
)

func TestIsOverflowAcrossProviders(t *testing.T) {
	// The wording each provider actually uses.
	for _, msg := range []string{
		`llm-openrouter: HTTP 400: This endpoint's maximum context length is 200000 tokens. However, you requested 210000 tokens.`,
		`llm-openai: 400 {"error":{"message":"...","code":"context_length_exceeded"}}`,
		`llm-anthropic: prompt is too long: 205000 tokens > 200000 maximum`,
		`input length and ` + "`max_tokens`" + ` exceed context limit`,
		`Please reduce the length of the messages.`,
	} {
		if !IsOverflow(errors.New(msg)) {
			t.Errorf("should be recognised as an overflow: %s", msg)
		}
	}
}

func TestIsOverflowIgnoresOtherFailures(t *testing.T) {
	for _, msg := range []string{
		"llm-openrouter: HTTP 401: Missing Authentication header",
		"llm-anthropic: ANTHROPIC_API_KEY is not set.",
		"llm-openrouter: stream: connection reset by peer",
		"loop: gave up after 10 steps",
		"",
	} {
		if IsOverflow(errors.New(msg)) {
			t.Errorf("not an overflow: %q", msg)
		}
	}
	if IsOverflow(nil) {
		t.Error("nil is not an overflow")
	}
}

// The help names the two ways forward and does not offer a third that
// bough deliberately does not do.
func TestOverflowHelpNamesTheWayOut(t *testing.T) {
	for _, want := range []string{"/model", "/new", "memory graph"} {
		if !strings.Contains(OverflowHelp, want) {
			t.Errorf("the help should mention %q", want)
		}
	}
	if !strings.Contains(OverflowHelp, "never compacts") {
		t.Error("it should say nothing was dropped: bough does not compact behind your back")
	}
}
