package llm

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"

	"github.com/andreylukin/bough/kernel"
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
	out, err := a.stream(context.Background(), "be brief", []Message{{Role: "user", Content: "hi"}}, func(string) {}, nil)
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

// max_tokens is configurable, and 4096 is no longer the silent cap: an
// agent that writes files hits it and comes back cut in half.
func TestMaxTokensConfig(t *testing.T) {
	ctx := kernel.NewContext()
	if err := (&anthropicPlugin{}).Apply(ctx, map[string]any{"model": "claude-sonnet-5"}); err != nil {
		t.Fatal(err)
	}
	got, err := kernel.Get[llmAny](ctx, "llm")
	if err != nil {
		t.Fatal(err)
	}
	if a, ok := got.(*anthropicLLM); !ok || a.maxTokens != defaultMaxTokens {
		t.Errorf("default max_tokens should be %d, got %v", defaultMaxTokens, got)
	}

	ctx2 := kernel.NewContext()
	if err := (&anthropicPlugin{}).Apply(ctx2, map[string]any{"model": "m", "max_tokens": 32000}); err != nil {
		t.Fatal(err)
	}
	a2, _ := kernel.Get[llmAny](ctx2, "llm")
	if a, ok := a2.(*anthropicLLM); !ok || a.maxTokens != 32000 {
		t.Errorf("max_tokens should be honoured, got %v", a2)
	}

	for _, bad := range []any{0, -5, "lots", 1.5} {
		if err := (&anthropicPlugin{}).Apply(kernel.NewContext(), map[string]any{"model": "m", "max_tokens": bad}); err == nil {
			t.Errorf("max_tokens %v (%T) should be rejected", bad, bad)
		}
	}
}

type llmAny = any

// Under /think the model thinks by default and thinking counts against
// max_tokens: the thinking streams out as it happens, a cap spent
// entirely on thinking is an error naming the cap, and an unset cap
// grows with the effort.
func TestAnthropicThinkingStreamAndCap(t *testing.T) {
	sse := strings.Join([]string{
		"event: message_start",
		"data: " + `{"type":"message_start","message":{"usage":{"input_tokens":10}}}`,
		"",
		"event: content_block_delta",
		"data: " + `{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm "}}`,
		"",
		"event: content_block_delta",
		"data: " + `{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"more"}}`,
		"",
		"event: message_delta",
		"data: " + `{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":16384}}`,
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
	a := &anthropicLLM{model: "claude-opus-5-5", maxTokens: defaultMaxTokens}
	a.client = anthropic.NewClient(option.WithAPIKey("test"), option.WithBaseURL(srv.URL))
	var thought strings.Builder
	_, err := a.stream(context.Background(), "", []Message{{Role: "user", Content: "hi"}}, func(string) {}, func(s string) { thought.WriteString(s) })
	if err == nil || !strings.Contains(err.Error(), "max_tokens (16384) while thinking") {
		t.Fatalf("thinking-only max_tokens: %v", err)
	}
	if thought.String() != "hmm more" {
		t.Fatalf("thinking not streamed: %q", thought.String())
	}
	if _, err := a.stream(context.Background(), "", nil, func(string) {}, nil); err == nil {
		t.Fatal("a nil onThink must still report the cut")
	}
	// Text that was cut is kept, marked.
	if out, err := a.cut("partial", "max_tokens", 5); err != nil || out != MarkTruncated("partial") {
		t.Fatalf("cut text: %q %v", out, err)
	}
	if out, err := a.cut("", "end_turn", 5); err != nil || out != "" {
		t.Fatalf("a real empty reply stays empty: %q %v", out, err)
	}
	for effort, want := range map[string]int64{"": defaultMaxTokens, "low": defaultMaxTokens, "high": 32768, "xhigh": 65536, "max": 65536} {
		if got := capFor(effort, false, defaultMaxTokens); got != want {
			t.Errorf("capFor(%q) = %d, want %d", effort, got, want)
		}
	}
	if got := capFor("max", true, 4096); got != 4096 {
		t.Errorf("a set max_tokens wins: %d", got)
	}
}
