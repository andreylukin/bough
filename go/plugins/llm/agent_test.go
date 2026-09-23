//go:build !windows

package llm

import (
	"context"
	"encoding/json"
	"encoding/json/jsontext"
	"errors"
	"io"
	"math"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/models"
	"github.com/andreylukin/bough/kernel"
)

// seen is a local provider that answers every request with one body
// and keeps what it was sent.
type seen struct {
	mu      sync.Mutex
	bodies  []string
	headers []http.Header
	paths   []string
	ctype   string
	body    string
}

func (s *seen) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	b, _ := io.ReadAll(r.Body)
	s.mu.Lock()
	s.bodies = append(s.bodies, string(b))
	s.headers = append(s.headers, r.Header.Clone())
	s.paths = append(s.paths, r.URL.Path)
	s.mu.Unlock()
	w.Header().Set("Content-Type", s.ctype)
	io.WriteString(w, s.body)
}

func serve(t *testing.T, ctype, body string) (*seen, *httptest.Server) {
	t.Helper()
	s := &seen{ctype: ctype, body: body}
	srv := httptest.NewServer(s)
	t.Cleanup(srv.Close)
	return s, srv
}

const anthropicSSE = "event: message_start\n" +
	`data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-opus-5","content":[],"stop_reason":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":20},"output_tokens":1}}}` + "\n\n" +
	"event: content_block_start\n" +
	`data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}` + "\n\n" +
	"event: content_block_delta\n" +
	`data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}` + "\n\n" +
	"event: content_block_stop\n" +
	`data: {"type":"content_block_stop","index":0}` + "\n\n" +
	"event: message_delta\n" +
	`data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}` + "\n\n" +
	"event: message_stop\n" +
	`data: {"type":"message_stop"}` + "\n\n"

func agentRequest() ullm.Request {
	return ullm.Request{Input: []ullm.Item{
		{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: "sys"}},
		{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "hi"}},
	}}
}

func mountAnthropic(t *testing.T, cfg map[string]any) *anthropicLLM {
	t.Helper()
	ctx := kernel.NewContext()
	if err := (&anthropicPlugin{}).Apply(ctx, cfg); err != nil {
		t.Fatal(err)
	}
	v, err := kernel.Get[llmAny](ctx, "llm")
	if err != nil {
		t.Fatal(err)
	}
	a := v.(*anthropicLLM)
	a.once.Do(func() { a.key = "test-key" }) // no env: tests run in parallel
	return a
}

// llm-anthropic drives the engine over the Messages API with the row's
// model, effort and cache_ttl, and folds the usage into the same tally
// the loop fills: inclusive input, cache counts, the 1h write split.
func TestAnthropicAgentAdapter(t *testing.T) {
	t.Parallel()
	s, srv := serve(t, "text/event-stream", anthropicSSE)
	a := mountAnthropic(t, map[string]any{"model": "claude-opus-5", "effort": "xhigh", "cache_ttl": "5m"})
	a.agent.base = srv.URL
	var src agentllm.Source = a
	ad, err := src.AgentAdapter(agentllm.Options{Session: "s"})
	if err != nil {
		t.Fatal(err)
	}
	if ad.Provider() != "anthropic" || ad.Model() != "claude-opus-5" {
		t.Errorf("identity %s/%s", ad.Provider(), ad.Model())
	}
	resp, err := ad.Respond(t.Context(), agentRequest(), ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if len(resp.Output) != 1 {
		t.Fatalf("output %+v", resp.Output)
	}
	body := s.bodies[0]
	for _, want := range []string{`"model":"claude-opus-5"`, `"effort":"xhigh"`, `"ttl":"5m"`, `"fallbacks":"default"`, `"max_tokens":64000`} {
		if !strings.Contains(body, want) {
			t.Errorf("request lacks %s:\n%s", want, body)
		}
	}
	if !strings.Contains(strings.Join(s.headers[0].Values("anthropic-beta"), ","), "server-side-fallback-2026-07-01") {
		t.Errorf("fallbacks need their beta: %v", s.headers[0].Values("anthropic-beta"))
	}
	u := a.Usage()
	if u.InputTokens != 130 || u.LastInputTokens != 130 || u.CacheReadTokens != 100 || u.CacheCreationTokens != 20 || u.CacheWrite1hTokens != 20 || u.OutputTokens != 5 {
		t.Errorf("usage = %+v", u)
	}
	if AgentCacheTTL(a) != 5*time.Minute {
		t.Errorf("AgentCacheTTL = %v", AgentCacheTTL(a))
	}

	// /think applies to the next request without a new adapter.
	if err := a.SetEffort("low"); err != nil {
		t.Fatal(err)
	}
	if _, err := ad.Respond(t.Context(), agentRequest(), ullm.RequestOptions{}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(s.bodies[1], `"effort":"low"`) {
		t.Errorf("the second request should carry the new effort:\n%s", s.bodies[1])
	}
}

func TestAnthropicAgentConfigErrorsNameTheRow(t *testing.T) {
	t.Parallel()
	for _, cfg := range []map[string]any{
		{"model": "m", "cache_ttl": "2h"},
		{"model": "m", "thinking_display": "loud"},
		{"model": "m", "fallbacks": "sometimes"},
		{"model": "m", "block_binding": "keep"},
		{"model": "m", "max_attempts": -1},
		{"model": "m", "idle_timeout": "soon"},
		{"model": "m", "effort": "extreme"},
	} {
		err := (&anthropicPlugin{}).Apply(kernel.NewContext(), cfg)
		if err == nil || !strings.HasPrefix(err.Error(), "llm-anthropic: ") {
			t.Errorf("%v: err = %v", cfg, err)
		}
	}
	a := mountAnthropic(t, map[string]any{"model": "m", "idle_timeout": "90s", "max_attempts": 3, "thinking_display": "summarized", "block_binding": "drop_block"})
	if a.agent.idle != 90*time.Second || a.agent.maxAttempts != 3 || a.agent.display != "summarized" || a.agent.binding != "drop_block" || a.agent.cacheTTL != "1h" {
		t.Errorf("parsed %+v", a.agent)
	}
}

// The loop's Anthropic path sends an effort only when one was set, so a
// row that never touched /think sends exactly what it always sent.
func TestLoopEffortOnlyWhenSet(t *testing.T) {
	t.Parallel()
	a := &anthropicLLM{model: "claude-sonnet-5", maxTokens: 100}
	if p := a.params("s", []Message{{Role: "user", Content: "x"}}); p.OutputConfig.Effort != "" {
		t.Errorf("no level set, but effort %q was sent", p.OutputConfig.Effort)
	}
	a.SetEffort("xhigh")
	if p := a.params("s", []Message{{Role: "user", Content: "x"}}); p.OutputConfig.Effort != "xhigh" {
		t.Errorf("effort = %q, want xhigh", p.OutputConfig.Effort)
	}
	h := &anthropicLLM{model: "claude-haiku-4-5", maxTokens: 100}
	h.SetEffort("high")
	if p := h.params("s", []Message{{Role: "user", Content: "x"}}); p.OutputConfig.Effort != "" {
		t.Errorf("Haiku takes no effort; sent %q", p.OutputConfig.Effort)
	}
}

const responsesSSE = `data: {"type":"response.output_text.delta","delta":"ok"}` + "\n\n" +
	`data: {"type":"response.completed","response":{"id":"r1","status":"completed","output":[{"type":"message","id":"m1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":50,"input_tokens_details":{"cached_tokens":40},"output_tokens":7,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":57,"cost":0.0025}}}` + "\n\n"

func TestOpenAIAgentAdapter(t *testing.T) {
	t.Parallel()
	s, srv := serve(t, "text/event-stream", responsesSSE)
	o := &openaiLLM{model: "gpt-5.6-sol", effort: "high", base: srv.URL}
	o.once.Do(func() { o.key = "test-key" })
	ad, err := o.AgentAdapter(agentllm.Options{})
	if err != nil {
		t.Fatal(err)
	}
	if ad.Provider() != "openai" {
		t.Errorf("provider %q", ad.Provider())
	}
	if _, err := ad.Respond(t.Context(), agentRequest(), ullm.RequestOptions{CacheKey: "s"}); err != nil {
		t.Fatal(err)
	}
	if s.paths[0] != "/v1/responses" || !strings.Contains(s.bodies[0], `"effort":"high"`) || s.headers[0].Get("Authorization") != "Bearer test-key" {
		t.Errorf("request %s %s", s.paths[0], s.bodies[0])
	}
	u := o.Usage()
	if u.InputTokens != 50 || u.CacheReadTokens != 40 || u.OutputTokens != 7 || u.LastInputTokens != 50 {
		t.Errorf("usage = %+v", u)
	}
}

// OpenRouter gets its late results as text for anthropic/* (auto), its
// own price from the response, and an envelope family per vendor.
func TestOpenRouterAgentAdapter(t *testing.T) {
	t.Parallel()
	lateInput := func() ullm.Request {
		r := agentRequest()
		r.Input = append(r.Input,
			ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "c1", Name: "bash", Arguments: "{}"}},
			ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "c1", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "running"}}}},
			ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleAssistant, Text: "waiting"}},
			ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "c1", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "LATE-DONE"}}}},
		)
		return r
	}
	for _, tc := range []struct {
		model, late string
		text        bool
		provider    string
	}{
		{"anthropic/claude-sonnet-5", "auto", true, "openrouter:anthropic"},
		{"openai/gpt-5.6-sol", "auto", false, "openrouter:openai"},
		{"openai/gpt-5.6-sol", "text", true, "openrouter:openai"},
		{"anthropic/claude-sonnet-5", "native", false, "openrouter:anthropic"},
	} {
		s, srv := serve(t, "text/event-stream", responsesSSE)
		ctx := kernel.NewContext()
		if err := (&openrouterPlugin{}).Apply(ctx, map[string]any{"model": tc.model, "late_results": tc.late, "cache_ttl": "5m"}); err != nil {
			t.Fatal(err)
		}
		v, _ := kernel.Get[llmAny](ctx, "llm")
		o := v.(*openrouterLLM)
		o.agentBase = srv.URL
		o.once.Do(func() { o.key = "test-key" })
		ad, err := o.AgentAdapter(agentllm.Options{})
		if err != nil {
			t.Fatal(err)
		}
		if ad.Provider() != tc.provider {
			t.Errorf("%s: provider %q", tc.model, ad.Provider())
		}
		if _, err := ad.Respond(t.Context(), lateInput(), ullm.RequestOptions{CacheKey: "s"}); err != nil {
			t.Fatal(err)
		}
		asText := strings.Contains(s.bodies[0], `\u003ctool_result call_id=\"c1\"`) || strings.Contains(s.bodies[0], `<tool_result call_id=\"c1\"`)
		if asText != tc.text {
			t.Errorf("%s late_results %s: late result as text = %v, want %v\n%s", tc.model, tc.late, asText, tc.text, s.bodies[0])
		}
		if !strings.Contains(s.bodies[0], `"cache_control":{"type":"ephemeral","ttl":"5m"}`) || s.headers[0].Get("X-Session-Id") == "" {
			t.Errorf("%s: cache routing missing: %s", tc.model, s.bodies[0])
		}
		if u := o.Usage(); !u.Priced || u.Cost != 0.0025 {
			t.Errorf("%s: OpenRouter's own price should be kept: %+v", tc.model, u)
		}
	}
	if err := (&openrouterPlugin{}).Apply(kernel.NewContext(), map[string]any{"model": "m", "late_results": "sometimes"}); err == nil || !strings.HasPrefix(err.Error(), "llm-openrouter: late_results") {
		t.Errorf("bad late_results: %v", err)
	}
}

func TestEchoIsASource(t *testing.T) {
	t.Parallel()
	ad, err := echoLLM{}.AgentAdapter(agentllm.Options{})
	if err != nil {
		t.Fatal(err)
	}
	r := agentRequest()
	r.Input[1] = ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "say CODE! please"}}
	resp, err := ad.Respond(t.Context(), r, ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if c, ok := resp.Output[0].Data.(ullm.ToolCall); !ok || c.Name != "bash" {
		t.Errorf("echo CODE! should call bash: %+v", resp.Output)
	}
}

func writeScript(t *testing.T, body string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "tape.json")
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	return p
}

// llm-script plays one tape across every adapter the row builds, and
// the title/status jobs (Complete) never take a step from it.
func TestScriptRow(t *testing.T) {
	t.Parallel()
	path := writeScript(t, `{"steps":[{"text":"first","usage":{"input":10,"output":2}},{"calls":[{"name":"bash","args":{"command":"ls"}}]}]}`)
	ctx := kernel.NewContext()
	if err := (&scriptPlugin{}).Apply(ctx, map[string]any{"script": path}); err != nil {
		t.Fatal(err)
	}
	v, _ := kernel.Get[llmAny](ctx, "llm")
	s := v.(*scriptLLM)
	if out, _ := s.Complete(t.Context(), "", []Message{{Role: "user", Content: "name this session"}}); out != "script: name this session" {
		t.Errorf("Complete = %q", out)
	}
	var deltas []agentllm.Delta
	a1, _ := s.AgentAdapter(agentllm.Options{Sink: func(d agentllm.Delta) { deltas = append(deltas, d) }})
	a2, _ := s.AgentAdapter(agentllm.Options{})
	r1, err := a1.Respond(agentllm.WithSeq(t.Context(), 1), agentRequest(), ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	r2, err := a2.Respond(t.Context(), agentRequest(), ullm.RequestOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if m, ok := r1.Output[0].Data.(ullm.Message); !ok || m.Text != "first" {
		t.Errorf("first step: %+v", r1.Output)
	}
	if c, ok := r2.Output[0].Data.(ullm.ToolCall); !ok || c.CallID != "call_2_1" {
		t.Errorf("second step through the other adapter: %+v", r2.Output)
	}
	if len(deltas) == 0 || deltas[0].Kind != agentllm.DeltaText || deltas[0].Seq != 1 {
		t.Errorf("script text streams: %+v", deltas)
	}
	if u := s.Usage(); u.InputTokens != 10+200 || u.OutputTokens != 12 {
		t.Errorf("usage = %+v (10+2 recorded, then the 100-per-item default)", u)
	}
	for _, cfg := range []map[string]any{{}, {"script": filepath.Join(t.TempDir(), "missing.json")}, {"script": writeScript(t, `{"steps":[{"bogus":1}]}`)}} {
		if err := (&scriptPlugin{}).Apply(kernel.NewContext(), cfg); err == nil || !strings.HasPrefix(err.Error(), "llm-script: ") {
			t.Errorf("%v: err = %v", cfg, err)
		}
	}
}

func TestOllamaRow(t *testing.T) {
	t.Parallel()
	s, srv := serve(t, "text/event-stream", responsesSSE)
	ctx := kernel.NewContext()
	if err := (&ollamaPlugin{}).Apply(ctx, map[string]any{"model": "qwen3:8b", "base_url": srv.URL + "/v1", "effort": "low"}); err != nil {
		t.Fatal(err)
	}
	v, _ := kernel.Get[llmAny](ctx, "llm")
	o := v.(*ollamaLLM)
	if o.Ready() != nil {
		t.Error("ollama has no key to miss")
	}
	out, err := o.Complete(t.Context(), "be brief", []Message{{Role: "user", Content: "hi"}})
	if err != nil || out != "ok" {
		t.Fatalf("Complete = %q, %v", out, err)
	}
	if s.paths[0] != "/v1/responses" || s.headers[0].Get("Authorization") != "" || !strings.Contains(s.bodies[0], `"model":"qwen3:8b"`) || strings.Contains(s.bodies[0], `"tools"`) {
		t.Errorf("request %s %v %s", s.paths[0], s.headers[0], s.bodies[0])
	}
	ad, err := o.AgentAdapter(agentllm.Options{})
	if err != nil || ad.Provider() != "ollama" {
		t.Fatalf("adapter %v %v", ad, err)
	}
	if err := (&ollamaPlugin{}).Apply(kernel.NewContext(), map[string]any{}); err == nil || !strings.HasPrefix(err.Error(), "llm-ollama: ") {
		t.Errorf("a missing model: %v", err)
	}
}

// The catalogue bridge: a level the model's catalogue entry does not
// list comes down to the highest one it does; unknown models pass.
func TestClampEffortByTheCatalogue(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ plugin, model, level, want string }{
		{"llm-anthropic", "claude-opus-5-5", "max", "max"}, // the override lists low..max
		{"llm-anthropic", "claude-opus-5-5", "off", "off"}, // off and "" are the adapter's to map
		{"llm-anthropic", "claude-opus-5-5", "", ""},
		{"llm-openai", "a-model-nobody-has", "max", "max"},
		// The Responses API has no "none": off asks for low, so a model
		// without low gets the least level it lists instead of a 400.
		{"llm-openai", "gpt-5-pro", "off", "high"},
		{"llm-openai", "gpt-5.2-chat-latest", "off", "medium"},
		{"llm-openai", "gpt-5.5", "off", "off"},
		{"llm-openai", "a-model-nobody-has", "off", "off"},
	} {
		if got := clampEffort(tc.plugin, tc.model, tc.level); got != tc.want {
			t.Errorf("clampEffort(%s, %s, %q) = %q, want %q", tc.plugin, tc.model, tc.level, got, tc.want)
		}
	}
	if AgentCacheTTL(echoLLM{}) != CacheTTL("") {
		t.Error("a row without its own TTL answers the provider default")
	}
}

// The loop's own paths send every level main sent unchanged; max, newer
// than they are, is fitted to the model or falls back to xhigh.
func TestLoopLevelFitsMax(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ plugin, model, level, want string }{
		{"llm-cerebras", "gpt-oss-120b", "max", "high"},
		{"llm-openai", "gpt-5.5", "max", "xhigh"},
		{"llm-openai", "gpt-5.6-sol", "max", "max"},
		{"llm-openrouter", "someone/unknown-model", "max", "xhigh"},
		{"llm-cerebras", "gpt-oss-120b", "xhigh", "xhigh"},
		{"llm-openai", "gpt-5-pro", "off", "off"},
		{"llm-openai", "gpt-5.5", "", ""},
	} {
		if got := loopLevel(tc.plugin, tc.model, tc.level); got != tc.want {
			t.Errorf("loopLevel(%s, %s, %q) = %q, want %q", tc.plugin, tc.model, tc.level, got, tc.want)
		}
	}
}

// /think max on a loop session reaches the wire as a level the model
// accepts.
func TestLoopOpenAISendsFittedMax(t *testing.T) {
	t.Parallel()
	o := &openaiLLM{model: "gpt-5.5", effort: EffortMax}
	b := o.body("", nil, false, false)
	if r, _ := b["reasoning"].(map[string]any); r["effort"] != "xhigh" {
		t.Errorf("reasoning = %v, want effort xhigh", b["reasoning"])
	}
}

func TestMaxIsAValidEffort(t *testing.T) {
	t.Parallel()
	if !ValidEffort(EffortMax) || ValidEffort("maximum") {
		t.Error("max is a level, maximum is not")
	}
	for _, e := range Efforts {
		if e == EffortMax {
			t.Error("max must stay out of the shift+tab cycle")
		}
	}
}

func anthropicClient(url string) anthropic.Client {
	return anthropic.NewClient(option.WithAPIKey("test-key"), option.WithBaseURL(url), option.WithMaxRetries(0))
}

// An overloaded_error inside a stream arrives on a 200; it is still
// worth another try.
func TestRetryableReadsTheErrorType(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n")
	}))
	defer srv.Close()
	a := &anthropicLLM{model: "claude-sonnet-5", maxTokens: 10}
	a.client = anthropicClient(srv.URL)
	_, err := a.stream(context.Background(), "s", []Message{{Role: "user", Content: "x"}}, func(string) {}, nil)
	var apiErr *anthropic.Error
	if !errors.As(err, &apiErr) || apiErr.StatusCode != 200 {
		t.Fatalf("want an in-stream API error on a 200, got %v", err)
	}
	if !retryable(err) {
		t.Error("an in-stream overloaded_error must be retryable")
	}
	bad := &anthropic.Error{StatusCode: 400}
	_ = json.Unmarshal([]byte(`{"type":"error","error":{"type":"invalid_request_error","message":"no"}}`), bad)
	if retryable(bad) {
		t.Error("a 400 is not retryable")
	}
}

// A Fable row that a server-side fallback served from Opus is priced at
// Opus's rates for that attempt, and the declined attempt's billed
// partial output, which only usage.iterations carries, is counted.
func TestFallbackUsageIsPricedAtTheServingModel(t *testing.T) {
	t.Parallel()
	raw := `{"input_tokens":1000,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,
		"iterations":[
			{"type":"message","model":"claude-fable-5-1","input_tokens":1000,"output_tokens":0},
			{"type":"message","model":"claude-fable-5-1","input_tokens":1000,"output_tokens":50},
			{"type":"fallback_message","model":"claude-opus-5","input_tokens":1000,"output_tokens":200}]}`
	r := ullm.Usage{InputTokens: 1000, OutputTokens: 200, Raw: jsontext.Value(raw)}
	var u Usage
	addAgentUsage(&u, r)
	addFallbackUsage(&u, r, "claude-fable-5-1")
	if u.InputTokens != 2000 || u.OutputTokens != 250 {
		t.Errorf("tally in %d out %d, want 2000 and 250 (the unbilled decline left out)", u.InputTokens, u.OutputTokens)
	}
	fable, _ := models.Lookup("llm-anthropic", "claude-fable-5-1")
	opus, _ := models.Lookup("llm-anthropic", "claude-opus-5")
	want := opus.Cost(1000, 200) - fable.Cost(1000, 200)
	if want >= 0 || math.Abs(u.FallbackCost-want) > 1e-12 {
		t.Errorf("FallbackCost = %v, want %v", u.FallbackCost, want)
	}
	if got := agentllm.ServedModel(r); got != "claude-opus-5" {
		t.Errorf("ServedModel = %q", got)
	}

	var plain Usage
	one := ullm.Usage{InputTokens: 10, OutputTokens: 2, Raw: jsontext.Value(`{"input_tokens":10,"output_tokens":2,"iterations":[{"type":"message","model":"claude-fable-5-1","input_tokens":10,"output_tokens":2}]}`)}
	addAgentUsage(&plain, one)
	addFallbackUsage(&plain, one, "claude-fable-5-1")
	if plain.InputTokens != 10 || plain.OutputTokens != 2 || plain.FallbackCost != 0 || agentllm.ServedModel(one) != "" {
		t.Errorf("a response the row's model served changes nothing: %+v", plain)
	}
}
