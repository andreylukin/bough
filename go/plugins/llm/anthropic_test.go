package llm

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
)

func TestAnthropicParamsCacheControl(t *testing.T) {
	a := &anthropicLLM{model: "claude-sonnet-4-5"}
	params := a.params("be brief", []Message{
		{Role: "user", Content: "hi"},
		{Role: "assistant", Content: "hello"},
		{Role: "user", Content: "bye"},
	})
	if len(params.System) != 1 {
		t.Fatalf("system blocks = %d, want 1", len(params.System))
	}
	if params.System[0].Text != "be brief" {
		t.Errorf("system text = %q, want %q", params.System[0].Text, "be brief")
	}
	if params.System[0].CacheControl.Type != "ephemeral" {
		t.Errorf("system cache_control type = %q, want %q", params.System[0].CacheControl.Type, "ephemeral")
	}
	if len(params.Messages) != 3 {
		t.Fatalf("messages = %d, want 3", len(params.Messages))
	}
}

// The SDK client pointed at a fake SSE server: usage from
// message_start must fold cache tokens into InputTokens and the
// new Cache* fields.
func TestAnthropicStreamUsageCache(t *testing.T) {
	sse := strings.Join([]string{
		"event: message_start",
		"data: " + `{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}`,
		"",
		"event: content_block_delta",
		"data: " + `{"type":"content_block_delta","delta":{"type":"text_delta","text":"ok"}}`,
		"",
		"event: message_delta",
		"data: " + `{"type":"message_delta","usage":{"output_tokens":5}}`,
		"",
		"event: message_stop",
		"data: " + `{"type":"message_stop"}`,
		"",
	}, "\n")
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.Write([]byte(sse))
	}))
	defer srv.Close()

	a := &anthropicLLM{model: "claude-sonnet-4-5"}
	a.client = anthropic.NewClient(
		option.WithAPIKey("test"),
		option.WithBaseURL(srv.URL),
	)
	out, err := a.stream(context.Background(), "be brief", []Message{{Role: "user", Content: "hi"}}, func(string) {})
	if err != nil {
		t.Fatalf("stream: %v", err)
	}
	if out != "ok" {
		t.Errorf("stream out = %q, want %q", out, "ok")
	}
	u := a.usage
	if u.InputTokens != 130 {
		t.Errorf("InputTokens = %d, want 130 (10 + 100 cache-read + 20 cache-creation)", u.InputTokens)
	}
	if u.OutputTokens != 5 {
		t.Errorf("OutputTokens = %d, want 5", u.OutputTokens)
	}
	if u.LastInputTokens != 130 {
		t.Errorf("LastInputTokens = %d, want 130", u.LastInputTokens)
	}
	if u.CacheReadTokens != 100 {
		t.Errorf("CacheReadTokens = %d, want 100", u.CacheReadTokens)
	}
	if u.CacheCreationTokens != 20 {
		t.Errorf("CacheCreationTokens = %d, want 20", u.CacheCreationTokens)
	}
}
