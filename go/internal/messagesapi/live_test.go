package messagesapi

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

// liveKey guards every test that reaches the real API: BOUGH_LIVE=1 and
// a key, or the test is skipped. go test ./... never spends money.
func liveKey(t *testing.T) string {
	t.Helper()
	if os.Getenv("BOUGH_LIVE") != "1" {
		t.Skip("set BOUGH_LIVE=1 to call the real Anthropic API")
	}
	key := os.Getenv("ANTHROPIC_API_KEY")
	if key == "" {
		t.Skip("skipped: no ANTHROPIC_API_KEY")
	}
	return key
}

func liveModel() string {
	if m := os.Getenv("BOUGH_LIVE_ANTHROPIC_MODEL"); m != "" {
		return m
	}
	return "claude-sonnet-5"
}

// One real round trip: a tool call, its result, and a reply that uses
// it, with streaming deltas and a cache read on the second request.
func TestLiveAnthropicToolRoundTrip(t *testing.T) {
	t.Parallel()
	key := liveKey(t)
	var deltas []agentllm.Delta
	a, err := New(Config{
		Client:  anthropic.NewClient(option.WithAPIKey(key), option.WithMaxRetries(0)),
		Model:   liveModel,
		Effort:  func() string { return "low" },
		Options: agentllm.Options{Sink: func(d agentllm.Delta) { deltas = append(deltas, d) }},
	})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 3*time.Minute)
	defer cancel()
	system := "You are a test harness. Call read_secret exactly once, then reply with the secret and nothing else. " + strings.Repeat("Padding so the prefix is long enough to cache. ", 200)
	req := ullm.Request{
		Input: []ullm.Item{
			{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: system}},
			{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "What is the secret?"}},
		},
		Tools: []ullm.Tool{{Type: ullm.ToolFunction, Name: "read_secret", Description: "Read the secret.", Parameters: map[string]any{"type": "object", "properties": map[string]any{}}}},
	}
	resp, err := a.Respond(agentllm.WithSeq(ctx, 1), req, ullm.RequestOptions{})
	if err != nil {
		t.Fatalf("first request: %v", err)
	}
	req.Input = append(req.Input, resp.Output...)
	calls := 0
	for _, it := range resp.Output {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			calls++
			req.Input = append(req.Input, ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: c.CallID, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "orchid-7319"}}}})
		}
	}
	if calls != 1 {
		t.Fatalf("got %d calls, want 1: %+v", calls, resp.Output)
	}
	resp2, err := a.Respond(agentllm.WithSeq(ctx, 2), req, ullm.RequestOptions{})
	if err != nil {
		t.Fatalf("second request: %v", err)
	}
	found := false
	for _, it := range resp2.Output {
		if m, ok := it.Data.(ullm.Message); ok && strings.Contains(m.Text, "orchid-7319") {
			found = true
		}
	}
	if !found {
		t.Fatalf("the reply does not carry the result: %+v", resp2.Output)
	}
	if resp2.Usage.CachedInputTokens == 0 {
		t.Errorf("no cache read on the second request: %+v", resp2.Usage)
	}
	sawText := false
	for _, d := range deltas {
		if d.Kind == agentllm.DeltaText && d.Seq == 2 {
			sawText = true
		}
	}
	if !sawText {
		t.Errorf("no text deltas streamed for the second request")
	}
}
