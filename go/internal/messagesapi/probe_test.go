package messagesapi

// Probes P1-P4 and P7 of go/docs/unreal-engine.md §15.3 on the Messages
// side. They log what the API did; the results go in doc.go.
//
//	BOUGH_LIVE=1 BOUGH_PROBES=1 go test -run TestProbe -v ./internal/messagesapi/

import (
	"context"
	"io"
	"net/http"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

func probeKey(t *testing.T) string {
	t.Helper()
	if os.Getenv("BOUGH_PROBES") != "1" {
		t.Skip("set BOUGH_PROBES=1 (and BOUGH_LIVE=1) to run the API probes")
	}
	return liveKey(t)
}

// gapTransport measures the longest silence between body reads.
type gapTransport struct {
	mu  sync.Mutex
	max time.Duration
}

func (g *gapTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	resp, err := http.DefaultTransport.RoundTrip(r)
	if err == nil {
		resp.Body = &gapBody{ReadCloser: resp.Body, g: g, last: time.Now()}
	}
	return resp, err
}

type gapBody struct {
	io.ReadCloser
	g    *gapTransport
	last time.Time
}

func (b *gapBody) Read(p []byte) (int, error) {
	n, err := b.ReadCloser.Read(p)
	now := time.Now()
	b.g.mu.Lock()
	if d := now.Sub(b.last); d > b.g.max {
		b.g.max = d
	}
	b.g.mu.Unlock()
	b.last = now
	return n, err
}

func probeAdapter(t *testing.T, key, model string, c Config, rt http.RoundTripper) agentllm.Adapter {
	t.Helper()
	if rt == nil {
		rt = http.DefaultTransport
	}
	c.Client = anthropic.NewClient(option.WithAPIKey(key), option.WithMaxRetries(0), option.WithHTTPClient(&http.Client{Transport: rt}))
	c.Model = func() string { return model }
	c.MaxAttempts = 1
	a, err := New(c)
	if err != nil {
		t.Fatal(err)
	}
	return a
}

var probeTool = ullm.Tool{Type: ullm.ToolFunction, Name: "get_value", Description: "Get the value.", Parameters: map[string]any{"type": "object", "properties": map[string]any{}}}

// P1 (and P2's precondition): Opus 5.5 with a committed placeholder and
// a late text result, under block_binding "error". No 400 and a growing
// cache read mean the late-result render keeps the prefix intact.
func TestProbeP1LateResultOpus55(t *testing.T) {
	key := probeKey(t)
	var trace []agentllm.Exchange
	a := probeAdapter(t, key, "claude-opus-5-5", Config{Binding: "error", Options: agentllm.Options{Trace: func(e agentllm.Exchange) { trace = append(trace, e) }}}, nil)
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Minute)
	defer cancel()
	in := []ullm.Item{
		system("You are a test harness. Call get_value once, then wait for its result and report it. " + strings.Repeat("Cache padding. ", 400)),
		userText("What is the value?"),
	}
	req := ullm.Request{Input: in, Tools: []ullm.Tool{probeTool}}
	r1, err := a.Respond(ctx, req, ullm.RequestOptions{})
	if err != nil {
		t.Fatalf("P1 first: %v", err)
	}
	req.Input = append(req.Input, r1.Output...)
	for _, it := range r1.Output {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			req.Input = append(req.Input, ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: c.CallID, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "This call is still running. Its result arrives later."}}}})
		}
	}
	r2, err := a.Respond(ctx, req, ullm.RequestOptions{})
	if err != nil {
		t.Fatalf("P1 second: %v", err)
	}
	req.Input = append(req.Input, r2.Output...)
	for _, it := range r1.Output {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			req.Input = append(req.Input, ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: c.CallID, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "the value is tangerine-42"}}}})
		}
	}
	r3, err := a.Respond(ctx, req, ullm.RequestOptions{})
	t.Logf("P1 third request (late result as text): err=%v cache_read=%d/%d/%d", err, r1.Usage.CachedInputTokens, r2.Usage.CachedInputTokens, r3.Usage.CachedInputTokens)
	for _, ex := range trace {
		if strings.Contains(string(ex.Response), "input_transformations") {
			t.Logf("P1 input_transformations seen: %.300s", ex.Response)
		}
	}
}

// P3: the longest byte gap in a max-effort thinking stream sizes the
// idle watchdog.
func TestProbeP3LongestGap(t *testing.T) {
	key := probeKey(t)
	g := &gapTransport{}
	a := probeAdapter(t, key, liveModel(), Config{Effort: func() string { return "max" }}, g)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Minute)
	defer cancel()
	_, err := a.Respond(ctx, ullm.Request{Input: []ullm.Item{userText("Prove that there are infinitely many primes congruent to 3 mod 4, carefully.")}}, ullm.RequestOptions{})
	t.Logf("P3 longest gap %v (err=%v)", g.max, err)
}

// P4: what display "updates" returns on the models that take it.
func TestProbeP4DisplayUpdates(t *testing.T) {
	key := probeKey(t)
	for _, model := range []string{"claude-opus-5-5", "claude-fable-5-1"} {
		a := probeAdapter(t, key, model, Config{}, nil)
		ctx, cancel := context.WithTimeout(t.Context(), 5*time.Minute)
		resp, err := a.Respond(ctx, ullm.Request{Input: []ullm.Item{
			system("Call get_value, then answer."), userText("What is the value? Say what you are about to do first."),
		}, Tools: []ullm.Tool{probeTool}}, ullm.RequestOptions{})
		cancel()
		var kinds []string
		for _, it := range resp.Output {
			if r, ok := it.Data.(ullm.Reasoning); ok {
				kinds = append(kinds, "reasoning:"+strings.Join(r.Summary, "|"))
			} else {
				kinds = append(kinds, string(it.Type))
			}
		}
		t.Logf("P4 %s: err=%v output=%q", model, err, kinds)
	}
}

// P7: a historical tool_use for a tool no longer in tools.
func TestProbeP7RetiredTool(t *testing.T) {
	key := probeKey(t)
	a := probeAdapter(t, key, liveModel(), Config{}, nil)
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Minute)
	defer cancel()
	_, err := a.Respond(ctx, ullm.Request{Input: []ullm.Item{
		system("Answer in one sentence."), userText("Use old_tool."),
		{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "toolu_p7", Name: "old_tool", Arguments: "{}"}},
		{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "toolu_p7", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "hello"}}}},
		userText("What did it say?"),
	}, Tools: []ullm.Tool{probeTool}}, ullm.RequestOptions{})
	t.Logf("P7 retired tool on the Messages API: err=%v", err)
}
