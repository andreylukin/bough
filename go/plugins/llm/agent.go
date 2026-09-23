package llm

// The llm rows' side of the engine seam (internal/agentllm): each row
// that can drive engine-unreal builds a harness adapter for a session
// from the model, effort and key it already owns, so /model, /think and
// the cost row keep working unchanged on the engine. The stacks are in
// the order go/docs/unreal-engine.md §6.3 gives.

import (
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/messagesapi"
	"github.com/andreylukin/bough/internal/models"
	"github.com/andreylukin/bough/internal/unreal/echo"
	"github.com/andreylukin/bough/internal/unreal/responses"
	"github.com/andreylukin/bough/internal/unreal/wrap"
)

var (
	_ agentllm.Source = (*anthropicLLM)(nil)
	_ agentllm.Source = (*openaiLLM)(nil)
	_ agentllm.Source = (*openrouterLLM)(nil)
	_ agentllm.Source = echoLLM{}
)

// agentAnthropic is llm-anthropic's engine-only config. The loop never
// reads it.
type agentAnthropic struct {
	cacheTTL    string // "1h" | "5m"
	display     string // "auto" | "summarized" | "omitted" | "updates"
	fallbacks   string // "auto" | "off"
	binding     string // "" | "drop_block" | "error"
	maxAttempts int
	idle        time.Duration
	base        string // tests point the client at a local server
}

func parseAgentAnthropic(cfg map[string]any) (agentAnthropic, error) {
	out := agentAnthropic{cacheTTL: "1h", display: "auto", fallbacks: "auto"}
	var err error
	if out.cacheTTL, err = oneOf(cfg, "llm-anthropic", "cache_ttl", out.cacheTTL, "1h", "5m"); err != nil {
		return out, err
	}
	if out.display, err = oneOf(cfg, "llm-anthropic", "thinking_display", out.display, "auto", "summarized", "omitted", "updates"); err != nil {
		return out, err
	}
	if out.fallbacks, err = oneOf(cfg, "llm-anthropic", "fallbacks", out.fallbacks, "auto", "off"); err != nil {
		return out, err
	}
	if out.binding, err = oneOf(cfg, "llm-anthropic", "block_binding", "", "", "drop_block", "error"); err != nil {
		return out, err
	}
	if out.maxAttempts, err = attempts(cfg, "llm-anthropic"); err != nil {
		return out, err
	}
	if v, ok := cfg["idle_timeout"]; ok {
		d, err := duration(v)
		if err != nil || d < 0 {
			return out, fmt.Errorf("llm-anthropic: idle_timeout must be a duration like 5m, got %v", v)
		}
		out.idle = d
	}
	return out, nil
}

func oneOf(cfg map[string]any, row, key, def string, allowed ...string) (string, error) {
	v, ok := cfg[key]
	if !ok {
		return def, nil
	}
	s, ok := v.(string)
	for _, a := range allowed {
		if ok && s == a {
			return s, nil
		}
	}
	var names []string
	for _, a := range allowed {
		if a != "" {
			names = append(names, a)
		}
	}
	return "", fmt.Errorf("%s: %s must be %s, got %v", row, key, strings.Join(names, " or "), v)
}

func attempts(cfg map[string]any, row string) (int, error) {
	v, ok := cfg["max_attempts"]
	if !ok {
		return 0, nil
	}
	n, ok := v.(int)
	if !ok || n < 0 {
		return 0, fmt.Errorf("%s: max_attempts must be a non-negative integer, got %v", row, v)
	}
	return n, nil
}

func duration(v any) (time.Duration, error) {
	switch d := v.(type) {
	case string:
		return time.ParseDuration(d)
	case int:
		return time.Duration(d) * time.Second, nil
	}
	return 0, fmt.Errorf("not a duration: %v", v)
}

// clampEffort fits a bough level to what the model's catalogue entry
// accepts, when it lists any: max on a model that stops at xhigh asks
// for xhigh rather than a 400. Unknown models pass through.
//
// off is the Messages adapter's to map (it can disable thinking). The
// Responses API has no "none", so there off asks for low; on a model
// whose catalogue has no low (gpt-5-pro takes only high) that is a 400
// on every request, so off becomes the least level the model lists.
func clampEffort(plugin, model, level string) string {
	if level == "" || (level == "off" && plugin == "llm-anthropic") {
		return level
	}
	m, ok := models.Lookup(plugin, model)
	if !ok || len(m.Efforts) == 0 {
		return level
	}
	if level == "off" {
		if slices.Contains(m.Efforts, "low") {
			return level
		}
		return messagesapi.Clamp("low", m.Efforts)
	}
	if c := messagesapi.Clamp(level, m.Efforts); c != "" {
		return c
	}
	return level
}

// loopLevel is the level the loop's OpenAI, OpenRouter and Cerebras
// paths send. They send every other level as they always have; max is
// newer than they are, so it is fitted to the model's catalogue entry,
// and on a model the catalogue does not know it asks for xhigh, the
// most any of those paths sent before max existed.
func loopLevel(plugin, model, level string) string {
	if level != EffortMax {
		return level
	}
	if m, ok := models.Lookup(plugin, model); ok && len(m.Efforts) > 0 {
		if c := messagesapi.Clamp(level, m.Efforts); c != "" {
			return c
		}
	}
	return "xhigh"
}

// loopEffort is the output_config.effort the loop's Anthropic path
// sends: nothing unless a level was set, and nothing on a model that
// takes no effort.
func loopEffort(model, level string) string {
	if level == "" {
		return ""
	}
	spec := messagesapi.Spec(model)
	e := messagesapi.EffortFor(level, spec)
	if e == "" || spec.Efforts == nil {
		return ""
	}
	return messagesapi.Clamp(string(e), spec.Efforts)
}

// addAgentUsage folds one engine response into a row's tally, so the
// status bar, /cost, the cost row and done.usage read the same numbers
// they read on the loop. InputTokens stays inclusive of the cache.
func addAgentUsage(u *Usage, r ullm.Usage) {
	u.InputTokens += int(r.InputTokens)
	u.OutputTokens += int(r.OutputTokens)
	if r.InputTokens > 0 {
		u.LastInputTokens = int(r.InputTokens)
	}
	u.CacheReadTokens += int(r.CachedInputTokens)
	u.CacheCreationTokens += int(r.CacheWriteInputTokens)
	if len(r.Raw) == 0 {
		return
	}
	var raw struct {
		CacheCreation struct {
			OneHour int `json:"ephemeral_1h_input_tokens"`
		} `json:"cache_creation"`
		Cost *float64 `json:"cost"`
	}
	if json.Unmarshal(r.Raw, &raw) != nil {
		return
	}
	u.CacheWrite1hTokens += raw.CacheCreation.OneHour
	if raw.Cost != nil {
		// OpenRouter prices every response itself; the Anthropic and
		// OpenAI tallies stay unpriced and the cost row prices them.
		u.Cost += *raw.Cost
		u.Priced = true
	}
}

// AgentAdapter implements agentllm.Source over the Messages API.
func (a *anthropicLLM) AgentAdapter(o agentllm.Options) (agentllm.Adapter, error) {
	if err := a.init(); err != nil {
		return nil, err
	}
	opts := []option.RequestOption{option.WithAPIKey(a.key), option.WithHTTPClient(httpClient), option.WithMaxRetries(0)}
	if a.agent.base != "" {
		opts = append(opts, option.WithBaseURL(a.agent.base))
	}
	var maxTokens int64
	if a.maxTokensSet {
		maxTokens = a.maxTokens
	}
	inner, err := messagesapi.New(messagesapi.Config{
		Client:      anthropic.NewClient(opts...),
		Model:       a.Model,
		Effort:      func() string { return clampEffort("llm-anthropic", a.model, a.Effort()) },
		MaxTokens:   maxTokens,
		CacheTTL:    a.agent.cacheTTL,
		Display:     a.agent.display,
		Fallbacks:   a.agent.fallbacks,
		Binding:     a.agent.binding,
		MaxAttempts: a.agent.maxAttempts,
		IdleTimeout: a.agent.idle,
		Options:     o,
	})
	if err != nil {
		return nil, err
	}
	return wrap.Observe(wrap.Envelope(inner), func(r ullm.Response) {
		a.mu.Lock()
		addAgentUsage(&a.usage, r.Usage)
		a.mu.Unlock()
	}), nil
}

// AgentCacheTTL is how long the prompt cache the engine's adapter writes
// lives: the row's cache_ttl, since every marker carries it.
func (a *anthropicLLM) AgentCacheTTL() time.Duration { return ttlDuration(a.agent.cacheTTL) }

func ttlDuration(ttl string) time.Duration {
	if ttl == "5m" {
		return 5 * time.Minute
	}
	return time.Hour
}

// AgentAdapter implements agentllm.Source over the Responses API.
func (o *openaiLLM) AgentAdapter(opts agentllm.Options) (agentllm.Adapter, error) {
	if err := o.init(); err != nil {
		return nil, err
	}
	inner, err := responses.New(responses.Config{
		Kind:        "openai",
		APIKey:      func() (string, error) { return o.key, nil },
		BaseURL:     o.base + "/v1",
		Model:       o.Model,
		Effort:      func() string { return clampEffort("llm-openai", o.model, o.Effort()) },
		MaxAttempts: o.maxAttempts,
		HTTP:        httpClient,
		Options:     opts,
	})
	if err != nil {
		return nil, err
	}
	return wrap.Observe(wrap.Envelope(wrap.StripReasoningOn400(inner)), func(r ullm.Response) {
		o.mu.Lock()
		addAgentUsage(&o.usage, r.Usage)
		o.mu.Unlock()
	}), nil
}

// AgentAdapter implements agentllm.Source over OpenRouter's Responses
// endpoint. An anthropic/* model gets its late results as text under
// late_results: auto, because OpenRouter translating a second
// function_call_output for one call id into Messages is unverified
// (probe P5) and a 400 there fails every slow call's turn.
func (o *openrouterLLM) AgentAdapter(opts agentllm.Options) (agentllm.Adapter, error) {
	if err := o.init(); err != nil {
		return nil, err
	}
	inner, err := responses.New(responses.Config{
		Kind:        "openrouter",
		APIKey:      func() (string, error) { return o.key, nil },
		BaseURL:     o.agentBase,
		Model:       o.Model,
		Effort:      func() string { return clampEffort("llm-openrouter", o.model, o.Effort()) },
		CacheTTL:    o.cacheTTL,
		MaxAttempts: o.maxAttempts,
		HTTP:        httpClient,
		Options:     opts,
	})
	if err != nil {
		return nil, err
	}
	late := o.lateResults == "text" || (o.lateResults != "native" && strings.HasPrefix(o.model, "anthropic/"))
	if late {
		inner = wrap.LateResultsAsText(inner)
	}
	return wrap.Observe(wrap.Envelope(wrap.StripReasoningOn400(inner)), func(r ullm.Response) {
		o.mu.Lock()
		addAgentUsage(&o.usage, r.Usage)
		o.mu.Unlock()
	}), nil
}

// AgentCacheTTL is the row's cache_ttl for the Anthropic upstreams the
// cache_control extension reaches; the others cache on their own terms.
func (o *openrouterLLM) AgentCacheTTL() time.Duration {
	if strings.HasPrefix(o.model, "anthropic/") {
		return ttlDuration(o.cacheTTL)
	}
	return CacheTTL(o.model)
}

// AgentAdapter implements agentllm.Source with the deterministic echo.
func (echoLLM) AgentAdapter(o agentllm.Options) (agentllm.Adapter, error) {
	return echo.New(o), nil
}
