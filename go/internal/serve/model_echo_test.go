package serve

import (
	"testing"

	"github.com/andreylukin/bough/plugins/history"
)

// R3-D: a /model typed in the composer is recorded as its echo; after a
// reload the picker named "Default model" until a reply came back.
func TestLastModelReadsSwitchEcho(t *testing.T) {
	es := []history.Entry{
		{Seq: 1, Kind: "assistant", Data: map[string]any{"model": "old"}},
		{Seq: 2, Kind: "command", Data: map[string]any{"text": "/model openai/gpt-5"}},
		{Seq: 3, Kind: "system", Data: map[string]any{"text": "model: llm-openrouter · openai/gpt-5"}},
	}
	if got := lastModel(es); got != "openai/gpt-5" {
		t.Fatalf("lastModel = %q, want openai/gpt-5", got)
	}
	if got := lastModel(es[:1]); got != "old" {
		t.Fatalf("lastModel = %q, want old", got)
	}
	picker := []history.Entry{{Seq: 1, Kind: "system", Data: map[string]any{"text": "model: llm-a · m\nchoices: x"}}}
	if got := lastModel(picker); got != "" {
		t.Fatalf("picker listing is not a switch: %q", got)
	}
}
