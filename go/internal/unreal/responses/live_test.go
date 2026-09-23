//go:build !windows

package responses

import (
	"context"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

// live guards every test that reaches a real provider: BOUGH_LIVE=1 and
// the key, or the test is skipped. go test ./... never spends money.
func live(t *testing.T, env string) string {
	t.Helper()
	if os.Getenv("BOUGH_LIVE") != "1" {
		t.Skip("set BOUGH_LIVE=1 to call the real API")
	}
	key := os.Getenv(env)
	if key == "" {
		t.Skip("skipped: no " + env)
	}
	return key
}

func liveRoundTrip(t *testing.T, kind, env, model string) {
	key := live(t, env)
	var deltas []agentllm.Delta
	a, err := New(Config{
		Kind:    kind,
		APIKey:  func() (string, error) { return key, nil },
		Model:   func() string { return model },
		Effort:  func() string { return "low" },
		HTTP:    &http.Client{},
		Options: agentllm.Options{Sink: func(d agentllm.Delta) { deltas = append(deltas, d) }},
	})
	if err != nil {
		t.Fatal(err)
	}
	defer a.Close()
	ctx, cancel := context.WithTimeout(t.Context(), 3*time.Minute)
	defer cancel()
	req := ullm.Request{
		Input: []ullm.Item{
			{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: "Call read_secret exactly once to answer the user. Once you receive its result, reply with the secret and no other text."}},
			{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "What is the secret?"}},
		},
		Tools: []ullm.Tool{{Type: ullm.ToolFunction, Name: "read_secret", Description: "Read the secret.", Parameters: map[string]any{"type": "object", "properties": map[string]any{}}}},
	}
	resp, err := a.Respond(agentllm.WithSeq(ctx, 1), req, ullm.RequestOptions{CacheKey: "bough-live-test"})
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
	resp2, err := a.Respond(agentllm.WithSeq(ctx, 2), req, ullm.RequestOptions{CacheKey: "bough-live-test"})
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
	sawTool := false
	for _, d := range deltas {
		if d.Kind == agentllm.DeltaToolStart && d.Seq == 1 && d.Name == "read_secret" {
			sawTool = true
		}
	}
	if !sawTool {
		t.Errorf("the tap saw no function_call start: %+v", deltas)
	}
}

func TestLiveOpenAI(t *testing.T) {
	t.Parallel()
	model := os.Getenv("BOUGH_LIVE_OPENAI_MODEL")
	if model == "" {
		model = "gpt-5.6-sol"
	}
	liveRoundTrip(t, "openai", "OPENAI_API_KEY", model)
}

func TestLiveOpenRouter(t *testing.T) {
	t.Parallel()
	model := os.Getenv("BOUGH_LIVE_OPENROUTER_MODEL")
	if model == "" {
		model = "openai/gpt-5.6-sol"
	}
	liveRoundTrip(t, "openrouter", "OPENROUTER_API_KEY", model)
}
