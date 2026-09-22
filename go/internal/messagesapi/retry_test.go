package messagesapi

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

func apiErr(typ, msg string) string {
	return `{"type":"error","error":{"type":"` + typ + `","message":"` + strings.ReplaceAll(msg, `"`, `\"`) + `"}}`
}

func retries(ta *testAdapter) []agentllm.Delta {
	ta.mu.Lock()
	defer ta.mu.Unlock()
	var out []agentllm.Delta
	for _, d := range ta.deltas {
		if d.Kind == agentllm.DeltaRetry {
			out = append(out, d)
		}
	}
	return out
}

func mustRespond(t *testing.T, ta *testAdapter, req ullm.Request) ullm.Response {
	t.Helper()
	resp, err := ta.Respond(agentllm.WithSeq(t.Context(), 1), req, ullm.RequestOptions{})
	if err != nil {
		t.Fatalf("Respond: %v", err)
	}
	return resp
}

func TestRetryRateLimitHonoursRetryAfter(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{status: 429, body: apiErr("rate_limit_error", "slow down"), headers: map[string]string{"Retry-After": "7"}},
		reply{sse: "thinking_text.sse"})
	mustRespond(t, ta, simpleRequest())
	if len(ta.waits) != 1 || ta.waits[0] != 7*time.Second {
		t.Errorf("waits = %v, want [7s]", ta.waits)
	}
	r := retries(ta)
	if len(r) != 1 || r[0].Attempt != 2 || r[0].Wait != 7*time.Second || r[0].Seq != 1 || !strings.Contains(r[0].Err, "slow down") {
		t.Errorf("retry deltas = %+v", r)
	}
}

func TestRetryRateLimitReadsResetHeaders(t *testing.T) {
	t.Parallel()
	reset := time.Date(2026, 9, 22, 12, 0, 30, 0, time.UTC).Format(time.RFC3339)
	ta := newTestAdapter(t, Config{},
		reply{status: 429, body: apiErr("rate_limit_error", "tokens"), headers: map[string]string{"anthropic-ratelimit-input-tokens-reset": reset}},
		reply{status: 429, body: apiErr("rate_limit_error", "tokens")},
		reply{sse: "thinking_text.sse"})
	mustRespond(t, ta, simpleRequest())
	if len(ta.waits) != 2 || ta.waits[0] != 30*time.Second || ta.waits[1] != 10*time.Second {
		t.Errorf("waits = %v, want [30s (the reset header) 10s (the schedule's second step)]", ta.waits)
	}
}

func TestRetryOverloadedBacksOffLonger(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{status: 529, body: apiErr("overloaded_error", "Overloaded")},
		reply{sse: "thinking_text.sse"})
	mustRespond(t, ta, simpleRequest())
	if len(ta.waits) != 1 || ta.waits[0] != 10*time.Second {
		t.Errorf("waits = %v, want [10s]", ta.waits)
	}
}

// An overload that arrives inside a 200 stream is still an overload.
func TestRetryInStreamOverloaded(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{sse: "overloaded_instream.sse"},
		reply{sse: "thinking_text.sse"})
	resp := mustRespond(t, ta, simpleRequest())
	if resp.ID != "msg_01Thinking" || len(ta.waits) != 1 || ta.waits[0] != 10*time.Second {
		t.Errorf("resp %q waits %v: want the second reply after a 10s overload backoff", resp.ID, ta.waits)
	}
}

// A stream that ends before message_stop is retried from the start, and
// the new attempt's deltas carry the new attempt number so the UI drops
// the half reply it showed.
func TestRetryCutStream(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{sse: "cut.sse"},
		reply{sse: "thinking_text.sse"})
	mustRespond(t, ta, simpleRequest())
	if len(ta.waits) != 1 || ta.waits[0] != time.Second {
		t.Errorf("waits = %v, want [1s]", ta.waits)
	}
	var first, second bool
	for _, d := range ta.deltas {
		if d.Kind == agentllm.DeltaText && d.Attempt == 1 && d.Text == "Half a rep" {
			first = true
		}
		if d.Kind == agentllm.DeltaText && d.Attempt == 2 {
			second = true
		}
	}
	if !first || !second {
		t.Errorf("want text deltas from attempt 1 and attempt 2: %+v", ta.deltas)
	}
}

func TestRetryGivesUpAfterTheBudget(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{MaxAttempts: 3},
		reply{status: 500, body: apiErr("api_error", "boom")},
		reply{status: 503, body: apiErr("api_error", "boom")},
		reply{status: 502, body: apiErr("api_error", "boom")})
	_, err := ta.Respond(t.Context(), simpleRequest(), ullm.RequestOptions{})
	if err == nil || !strings.Contains(err.Error(), "llm-anthropic") || !strings.Contains(err.Error(), "502") {
		t.Errorf("err = %v, want the last status named by the row", err)
	}
	if ta.s.requests() != 3 || len(ta.waits) != 2 {
		t.Errorf("requests %d waits %v, want 3 and 2", ta.s.requests(), ta.waits)
	}
}

func TestA400IsNotRetried(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{status: 400, body: apiErr("invalid_request_error", "messages: roles must alternate")})
	_, err := ta.Respond(t.Context(), simpleRequest(), ullm.RequestOptions{})
	if err == nil || !strings.Contains(err.Error(), "roles must alternate") || !strings.Contains(err.Error(), "llm-anthropic") {
		t.Errorf("err = %v", err)
	}
	if ta.s.requests() != 1 {
		t.Errorf("requests = %d, want 1", ta.s.requests())
	}
}

func TestA401NamesTheKey(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{status: 401, body: apiErr("authentication_error", "invalid x-api-key")})
	_, err := ta.Respond(t.Context(), simpleRequest(), ullm.RequestOptions{})
	if err == nil || !strings.Contains(err.Error(), "ANTHROPIC_API_KEY was rejected") || !strings.Contains(err.Error(), "/connect anthropic") {
		t.Errorf("err = %v", err)
	}
}

func TestOverflowIsTyped(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{status: 400, body: apiErr("invalid_request_error", "prompt is too long: 1100000 tokens > 1000000 maximum")})
	_, err := ta.Respond(t.Context(), simpleRequest(), ullm.RequestOptions{})
	if !errors.Is(err, agentllm.ErrContextOverflow) {
		t.Errorf("err = %v, want ErrContextOverflow", err)
	}
	if ta.s.requests() != 1 {
		t.Errorf("an overflow is not retried; requests = %d", ta.s.requests())
	}
}

// A beta the account does not have is dropped, the request retried
// once without it, and every later request leaves it out up front.
func TestUnknownBetaIsDroppedForTheProcess(t *testing.T) {
	t.Parallel()
	fb := "server-side-fallback-2026-07-01"
	ta := newTestAdapter(t, Config{Model: func() string { return "claude-opus-5" }},
		reply{status: 400, body: apiErr("invalid_request_error", "Unexpected value(s) `"+fb+"` for the `anthropic-beta` header.")},
		reply{sse: "thinking_text.sse"},
		reply{sse: "thinking_text.sse"})
	mustRespond(t, ta, simpleRequest())
	mustRespond(t, ta, simpleRequest())
	if !strings.Contains(ta.s.betas[0], fb) || !strings.Contains(ta.s.bodies[0], `"fallbacks":"default"`) {
		t.Fatalf("the first request should carry fallbacks: %s %s", ta.s.betas[0], ta.s.bodies[0])
	}
	for i := 1; i < 3; i++ {
		if strings.Contains(ta.s.betas[i], fb) || strings.Contains(ta.s.bodies[i], `"fallbacks"`) {
			t.Errorf("request %d still sends the refused beta or its field: %s", i+1, ta.s.betas[i])
		}
	}
}

func thinkingRequest() ullm.Request {
	raw := `{"signature":"sigOld","thinking":"earlier","type":"thinking"}`
	return ullm.Request{Input: []ullm.Item{
		system("s"), userText("one"),
		{Type: ullm.ItemReasoning, Data: ullm.Reasoning{Raw: []byte(raw)}},
		{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: "ok"}},
		userText("two"),
	}}
}

// A thinking block the API says belongs to another conversation is
// stripped, the request retried once, and the strip sticks for the
// adapter: the rest of the session's blocks would fail the same way.
func TestBindingErrorStripsThinking(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{},
		reply{status: 400, body: apiErr("invalid_request_error", "messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation.")},
		reply{sse: "thinking_text.sse"},
		reply{sse: "thinking_text.sse"})
	mustRespond(t, ta, thinkingRequest())
	mustRespond(t, ta, thinkingRequest())
	if !strings.Contains(ta.s.bodies[0], "sigOld") {
		t.Fatal("the first request should carry the old thinking block")
	}
	for i := 1; i < 3; i++ {
		if strings.Contains(ta.s.bodies[i], "sigOld") {
			t.Errorf("request %d still carries the stripped block", i+1)
		}
	}
}

// A stream that goes silent is cut by the watchdog and retried.
func TestIdleWatchdog(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{IdleTimeout: 150 * time.Millisecond},
		reply{sse: "thinking_text.sse", hang: 10 * time.Second},
		reply{sse: "thinking_text.sse"})
	start := time.Now()
	mustRespond(t, ta, simpleRequest())
	if el := time.Since(start); el > 5*time.Second {
		t.Errorf("the watchdog took %v", el)
	}
	r := retries(ta)
	if len(r) != 1 || !strings.Contains(r[0].Err, "silent") {
		t.Errorf("retry deltas = %+v, want one naming the silent stream", r)
	}
}

func TestCancelReturnsTheContextError(t *testing.T) {
	t.Parallel()
	ta := newTestAdapter(t, Config{}, reply{sse: "thinking_text.sse", hang: 10 * time.Second})
	ctx, cancel := context.WithTimeout(t.Context(), 200*time.Millisecond)
	defer cancel()
	_, err := ta.Respond(ctx, simpleRequest(), ullm.RequestOptions{})
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Errorf("err = %v, want the context's", err)
	}
	if len(ta.waits) != 0 {
		t.Errorf("a cancelled request must not be retried: waits %v", ta.waits)
	}
}

func TestClassifyTransport(t *testing.T) {
	t.Parallel()
	now := time.Now()
	for _, tc := range []struct {
		err  error
		want class
	}{
		{errors.New("read tcp: connection reset by peer"), transient},
		{errors.New("unexpected EOF"), transient},
		{errCut, transient},
		{context.Canceled, fatal},
		{errors.New("something else"), fatal},
	} {
		if got := classify(tc.err, now).class; got != tc.want {
			t.Errorf("classify(%v) = %v, want %v", tc.err, got, tc.want)
		}
	}
}

func TestNamedBetas(t *testing.T) {
	t.Parallel()
	got := namedBetas("Unexpected value(s) `a-1`, `b-2` for the `anthropic-beta` header.")
	if strings.Join(got, ",") != "a-1,b-2" {
		t.Errorf("namedBetas = %q", got)
	}
}
