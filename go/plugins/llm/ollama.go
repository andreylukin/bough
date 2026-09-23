//go:build !windows

package llm

// llm-ollama: a local model behind Ollama's Responses endpoint. Config:
// model (required), base_url (default http://localhost:11434/v1),
// effort, max_tokens, max_attempts. No key, no price. Complete is one
// request with no tools, so the row also serves llm-small; the engine
// gets the full adapter.

import (
	"context"
	"fmt"
	"strings"
	"sync"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/responses"
	"github.com/andreylukin/bough/internal/unreal/wrap"
	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-ollama", func() kernel.Plugin { return &ollamaPlugin{} })
}

type ollamaPlugin struct{}

func (p *ollamaPlugin) Name() string     { return "llm-ollama" }
func (p *ollamaPlugin) Inject() []string { return nil }

func (p *ollamaPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	model, ok := cfg["model"].(string)
	if !ok || model == "" {
		return fmt.Errorf("llm-ollama: config needs model (string), an installed model like qwen3:8b")
	}
	o := &ollamaLLM{model: model, base: responses.OllamaBase}
	if b, ok := cfg["base_url"].(string); ok && b != "" {
		o.base = strings.TrimRight(b, "/")
	}
	if e, ok := cfg["effort"]; ok {
		level, ok := e.(string)
		if !ok || !ValidEffort(level) {
			return fmt.Errorf("llm-ollama: effort must be one of off, low, medium, high, xhigh or max, got %v", e)
		}
		o.effort = level
	}
	if v, ok := cfg["max_tokens"]; ok {
		n, ok := v.(int)
		if !ok || n < 1 {
			return fmt.Errorf("llm-ollama: max_tokens must be a positive integer, got %v", v)
		}
		o.maxTokens = int64(n)
	}
	n, err := attempts(cfg, "llm-ollama")
	if err != nil {
		return err
	}
	o.maxAttempts = n
	ctx.Provide(serviceKey(cfg), o)
	return nil
}

type ollamaLLM struct {
	model       string
	base        string
	maxTokens   int64
	maxAttempts int

	mu     sync.Mutex
	effort string
	usage  Usage

	once     sync.Once
	complete agentllm.Adapter
	err      error
}

var _ agentllm.Source = (*ollamaLLM)(nil)

func (o *ollamaLLM) Model() string { return o.model }

// Ready implements Ready: there is no key to miss. Whether the server
// is up is a health check, which Ready is not.
func (o *ollamaLLM) Ready() error { return nil }

func (o *ollamaLLM) Usage() Usage {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.usage
}

func (o *ollamaLLM) Effort() string {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.effort
}

func (o *ollamaLLM) SetEffort(level string) error {
	if !ValidEffort(level) {
		return fmt.Errorf("llm-ollama: unknown thinking level %q", level)
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	o.effort = level
	return nil
}

func (o *ollamaLLM) adapter(opts agentllm.Options) (agentllm.Adapter, error) {
	inner, err := responses.New(responses.Config{
		Kind:        "ollama",
		BaseURL:     o.base,
		Model:       o.Model,
		Effort:      o.Effort,
		MaxTokens:   o.maxTokens,
		MaxAttempts: o.maxAttempts,
		HTTP:        httpClient,
		Options:     opts,
	})
	if err != nil {
		return nil, err
	}
	return wrap.Observe(wrap.Envelope(inner), func(r ullm.Response) {
		o.mu.Lock()
		addAgentUsage(&o.usage, r.Usage)
		o.mu.Unlock()
	}), nil
}

// AgentAdapter implements agentllm.Source.
func (o *ollamaLLM) AgentAdapter(opts agentllm.Options) (agentllm.Adapter, error) {
	return o.adapter(opts)
}

// Complete is one request with no tools.
func (o *ollamaLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	o.once.Do(func() { o.complete, o.err = o.adapter(agentllm.Options{}) })
	if o.err != nil {
		return "", o.err
	}
	in := []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: system}}}
	for _, m := range messages {
		role := ullm.RoleUser
		if m.Role == "assistant" {
			role = ullm.RoleAssistant
		}
		in = append(in, ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: role, Text: m.Content}})
	}
	resp, err := o.complete.Respond(ctx, ullm.Request{Input: in}, ullm.RequestOptions{})
	if err != nil {
		return "", err
	}
	if resp.Failure != nil {
		return "", fmt.Errorf("llm-ollama: %s: %s", resp.Failure.Code, resp.Failure.Message)
	}
	var out strings.Builder
	for _, it := range resp.Output {
		if m, ok := it.Data.(ullm.Message); ok && m.Role == ullm.RoleAssistant {
			out.WriteString(m.Text)
		}
	}
	if resp.Stop == ullm.StopMaxOutputTokens {
		return MarkTruncated(out.String()), nil
	}
	return out.String(), nil
}
