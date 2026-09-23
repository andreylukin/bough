package llm

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"

	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-anthropic", func() kernel.Plugin { return &anthropicPlugin{} })
}

type anthropicPlugin struct{}

func (p *anthropicPlugin) Name() string     { return "llm-anthropic" }
func (p *anthropicPlugin) Inject() []string { return nil }

func (p *anthropicPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	model, ok := cfg["model"].(string)
	if !ok || model == "" {
		return fmt.Errorf("llm-anthropic: config needs model (string)")
	}
	// 4096 was hardcoded, which is not much for an agent that writes
	// files: a long patch hit the cap and came back cut in half.
	var maxTokens int64 = defaultMaxTokens
	if v, ok := cfg["max_tokens"]; ok {
		n, ok := v.(int)
		if !ok || n < 1 {
			return fmt.Errorf("llm-anthropic: max_tokens must be a positive integer, got %v", v)
		}
		maxTokens = int64(n)
	}
	a := &anthropicLLM{model: model, maxTokens: maxTokens}
	_, a.maxTokensSet = cfg["max_tokens"]
	if e, ok := cfg["effort"]; ok {
		level, ok := e.(string)
		if !ok || !ValidEffort(level) {
			return fmt.Errorf("llm-anthropic: effort must be one of off, low, medium, high, xhigh or max, got %v", e)
		}
		a.effort = level
	}
	agent, err := parseAgentAnthropic(cfg)
	if err != nil {
		return err
	}
	a.agent = agent
	ctx.Provide(serviceKey(cfg), a)
	return nil
}

// defaultMaxTokens is the reply cap when the row does not set one.
const defaultMaxTokens = 16384

type anthropicLLM struct {
	model        string
	maxTokens    int64
	maxTokensSet bool
	agent        agentAnthropic // the engine's keys; see agent.go

	once   sync.Once
	key    string
	client anthropic.Client
	err    error

	mu     sync.Mutex
	usage  Usage
	effort string
}

// Usage implements UsageReporter: token counts only (no price table
// here, so Priced stays false).
// Model implements Modeler.
func (a *anthropicLLM) Model() string { return a.model }

func (a *anthropicLLM) Usage() Usage {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.usage
}

// Effort implements Efforter: the level /think set, "" for the model's
// own default.
func (a *anthropicLLM) Effort() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.effort
}

// SetEffort changes it for the next request.
func (a *anthropicLLM) SetEffort(level string) error {
	if !ValidEffort(level) {
		return fmt.Errorf("llm-anthropic: unknown thinking level %q", level)
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	a.effort = level
	return nil
}

// Ready reports whether this provider is configured (see llm.Ready):
// init() is idempotent, so asking early costs nothing.
func (a *anthropicLLM) Ready() error { return a.init() }

func (a *anthropicLLM) init() error {
	a.once.Do(func() {
		key := os.Getenv("ANTHROPIC_API_KEY")
		if key == "" {
			a.err = MissingKey("llm-anthropic", "ANTHROPIC_API_KEY")
			return
		}
		a.key = key
		// The shared client: bounded dial, TLS and response-header
		// waits, so a connection that opens and goes quiet cannot hang
		// the turn (see httpclient.go).
		a.client = anthropic.NewClient(option.WithAPIKey(key), option.WithHTTPClient(httpClient))
	})
	return a.err
}

// capFor is the reply cap for an effort level when the row set none.
// Opus 5.5 and its peers think by default under output_config.effort,
// and thinking counts against max_tokens: at /think max a 16k cap was
// spent entirely on thinking, twice, and the loop read two empty
// replies as a provider hiccup after five silent minutes.
func capFor(effort string, set bool, base int64) int64 {
	if set {
		return base
	}
	switch effort {
	case "high":
		return 32768
	case "xhigh", "max":
		return 65536
	}
	return base
}

func (a *anthropicLLM) params(system string, messages []Message) anthropic.MessageNewParams {
	params := anthropic.MessageNewParams{
		Model:     anthropic.Model(a.model),
		MaxTokens: capFor(a.Effort(), a.maxTokensSet, a.maxTokens),
	}
	// Only a level someone set is sent, so a row that never touched
	// /think sends exactly what it always sent.
	if e := loopEffort(a.model, a.Effort()); e != "" {
		params.OutputConfig.Effort = anthropic.OutputConfigEffort(e)
	}
	if system != "" {
		// cache_control on the system prompt lets Anthropic reuse it
		// across turns instead of re-reading it every request.
		params.System = []anthropic.TextBlockParam{{
			Text:         system,
			CacheControl: anthropic.NewCacheControlEphemeralParam(),
		}}
	}
	for _, m := range messages {
		block := anthropic.NewTextBlock(m.Content)
		switch m.Role {
		case "assistant":
			params.Messages = append(params.Messages, anthropic.NewAssistantMessage(block))
		default:
			blocks := []anthropic.ContentBlockParamUnion{block}
			for _, img := range loadImages(m.Images) {
				blocks = append(blocks, anthropic.NewImageBlockBase64(img.mime, img.data))
			}
			params.Messages = append(params.Messages, anthropic.NewUserMessage(blocks...))
		}
	}
	return params
}

func (a *anthropicLLM) wrapErr(err error) error {
	if apiErr, ok := errors.AsType[*anthropic.Error](err); ok && apiErr.StatusCode == 401 {
		return keyRejected("llm-anthropic", "ANTHROPIC_API_KEY", "anthropic", apiErr.StatusCode, "API key is invalid")
	}
	if apiErr, ok := errors.AsType[*anthropic.Error](err); ok && apiErr.StatusCode == 404 {
		return fmt.Errorf("llm-anthropic: model %q not found on anthropic — switch with /model", a.model)
	}
	return fmt.Errorf("llm-anthropic: %w", err)
}

// Stream implements Streamer over the SDK's SSE stream: text_delta
// events feed onDelta; message_start/message_delta carry the usage.
func (a *anthropicLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	return a.StreamThinking(ctx, system, messages, onDelta, nil)
}

// StreamThinking is Stream with the model's thinking shown as it
// streams (onThink may be nil): under /think the model can think for
// minutes before its first word, and a silent turn read as a hang.
func (a *anthropicLLM) StreamThinking(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error) {
	if err := a.init(); err != nil {
		return "", err
	}
	// A stream that already delivered text is never retried: the user
	// would see the reply twice.
	delivered := false
	return withRetries(ctx, func() (string, bool, error) {
		out, err := a.stream(ctx, system, messages, func(d string) { delivered = true; onDelta(d) }, onThink)
		return out, err != nil && !delivered && retryable(err), err
	})
}

func (a *anthropicLLM) stream(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error) {
	params := a.params(system, messages)
	stream := a.client.Messages.NewStreaming(ctx, params)
	defer stream.Close()
	var out strings.Builder
	var in, outTok, cacheRead, cacheCreate int
	var stop string
	for stream.Next() {
		switch ev := stream.Current().AsAny().(type) {
		case anthropic.MessageStartEvent:
			// InputTokens excludes cache_read/cache_creation tokens;
			// sum the three so the context % reads the whole prompt.
			in += int(ev.Message.Usage.InputTokens)
			cacheRead += int(ev.Message.Usage.CacheReadInputTokens)
			cacheCreate += int(ev.Message.Usage.CacheCreationInputTokens)
		case anthropic.MessageDeltaEvent:
			outTok += int(ev.Usage.OutputTokens)
			if ev.Delta.StopReason != "" {
				stop = string(ev.Delta.StopReason)
			}
		case anthropic.ContentBlockDeltaEvent:
			switch d := ev.Delta.AsAny().(type) {
			case anthropic.TextDelta:
				if d.Text != "" {
					out.WriteString(d.Text)
					onDelta(d.Text)
				}
			case anthropic.ThinkingDelta:
				if onThink != nil && d.Thinking != "" {
					onThink(d.Thinking)
				}
			}
		}
	}
	if err := stream.Err(); err != nil {
		return "", a.wrapErr(err)
	}
	a.mu.Lock()
	a.usage.InputTokens += in + cacheRead + cacheCreate
	a.usage.OutputTokens += outTok
	a.usage.LastInputTokens = in + cacheRead + cacheCreate
	a.usage.CacheReadTokens += cacheRead
	a.usage.CacheCreationTokens += cacheCreate
	a.mu.Unlock()
	return a.cut(out.String(), stop, params.MaxTokens)
}

// cut applies the stop reason: a reply cut at max_tokens is marked
// truncated, and one cut before any text (the cap went to thinking) is
// an error that says what to change, never an empty reply the loop
// retries in silence.
func (a *anthropicLLM) cut(text, stop string, cap int64) (string, error) {
	if stop != "max_tokens" {
		return text, nil
	}
	if strings.TrimSpace(text) == "" {
		return "", fmt.Errorf("llm-anthropic: the reply hit max_tokens (%d) while thinking, before any text — raise max_tokens on the llm row, or lower /think", cap)
	}
	return MarkTruncated(text), nil
}

func (a *anthropicLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	if err := a.init(); err != nil {
		return "", err
	}
	return withRetries(ctx, func() (string, bool, error) {
		out, err := a.complete(ctx, system, messages)
		return out, retryable(err), err
	})
}

func (a *anthropicLLM) complete(ctx context.Context, system string, messages []Message) (string, error) {
	params := a.params(system, messages)
	resp, err := a.client.Messages.New(ctx, params)
	if err != nil {
		return "", a.wrapErr(err)
	}
	in := int(resp.Usage.InputTokens) + int(resp.Usage.CacheReadInputTokens) + int(resp.Usage.CacheCreationInputTokens)
	a.mu.Lock()
	a.usage.InputTokens += in
	a.usage.OutputTokens += int(resp.Usage.OutputTokens)
	a.usage.LastInputTokens = in
	a.usage.CacheReadTokens += int(resp.Usage.CacheReadInputTokens)
	a.usage.CacheCreationTokens += int(resp.Usage.CacheCreationInputTokens)
	a.mu.Unlock()
	var out strings.Builder
	for _, b := range resp.Content {
		if b.Type == "text" {
			out.WriteString(b.Text)
		}
	}
	return a.cut(out.String(), string(resp.StopReason), params.MaxTokens)
}
