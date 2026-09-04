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
	ctx.Provide(serviceKey(cfg), &anthropicLLM{model: model, maxTokens: maxTokens})
	return nil
}

// defaultMaxTokens is the reply cap when the row does not set one.
const defaultMaxTokens = 16384

type anthropicLLM struct {
	model     string
	maxTokens int64

	once   sync.Once
	client anthropic.Client
	err    error

	mu    sync.Mutex
	usage Usage
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
		a.client = anthropic.NewClient(option.WithAPIKey(key))
	})
	return a.err
}

func (a *anthropicLLM) params(system string, messages []Message) anthropic.MessageNewParams {
	params := anthropic.MessageNewParams{
		Model:     anthropic.Model(a.model),
		MaxTokens: a.maxTokens,
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
			params.Messages = append(params.Messages, anthropic.NewUserMessage(block))
		}
	}
	return params
}

func (a *anthropicLLM) wrapErr(err error) error {
	if apiErr, ok := errors.AsType[*anthropic.Error](err); ok && apiErr.StatusCode == 404 {
		return fmt.Errorf("llm-anthropic: model %q not found on anthropic — switch with /model", a.model)
	}
	return fmt.Errorf("llm-anthropic: %w", err)
}

// Stream implements Streamer over the SDK's SSE stream: text_delta
// events feed onDelta; message_start/message_delta carry the usage.
func (a *anthropicLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	if err := a.init(); err != nil {
		return "", err
	}
	// A stream that already delivered text is never retried: the user
	// would see the reply twice.
	delivered := false
	return withRetries(ctx, func() (string, bool, error) {
		out, err := a.stream(ctx, system, messages, func(d string) { delivered = true; onDelta(d) })
		return out, err != nil && !delivered && retryable(err), err
	})
}

func (a *anthropicLLM) stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	stream := a.client.Messages.NewStreaming(ctx, a.params(system, messages))
	defer stream.Close()
	var out strings.Builder
	var in, outTok, cacheRead, cacheCreate int
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
		case anthropic.ContentBlockDeltaEvent:
			if d, ok := ev.Delta.AsAny().(anthropic.TextDelta); ok && d.Text != "" {
				out.WriteString(d.Text)
				onDelta(d.Text)
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
	return out.String(), nil
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
	resp, err := a.client.Messages.New(ctx, a.params(system, messages))
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
	// Cut off at max_tokens: the text is real but it is not an answer.
	if resp.StopReason == "max_tokens" {
		return MarkTruncated(out.String()), nil
	}
	return out.String(), nil
}
