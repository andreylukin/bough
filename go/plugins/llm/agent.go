//go:build !windows

package llm

// The llm rows' side of the engine seam (internal/agentllm): each row
// that can drive engine-unreal builds a harness adapter for a session
// from the model, effort and key it already owns, so /model, /think and
// the cost row keep working unchanged on the engine. The stacks are in
// the order go/docs/unreal-engine.md §6.3 gives.

import (
	"strings"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/messagesapi"
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
		addFallbackUsage(&a.usage, r.Usage, a.Model())
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
