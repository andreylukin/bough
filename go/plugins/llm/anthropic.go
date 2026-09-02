package llm

import (
	"context"
	"fmt"
	"os"
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
	ctx.Provide("llm", &anthropicLLM{model: model})
	return nil
}

type anthropicLLM struct {
	model string

	once   sync.Once
	client anthropic.Client
	err    error
}

func (a *anthropicLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	a.once.Do(func() {
		key := os.Getenv("ANTHROPIC_API_KEY")
		if key == "" {
			a.err = fmt.Errorf("llm-anthropic: ANTHROPIC_API_KEY not set")
			return
		}
		a.client = anthropic.NewClient(option.WithAPIKey(key))
	})
	if a.err != nil {
		return "", a.err
	}

	params := anthropic.MessageNewParams{
		Model:     anthropic.Model(a.model),
		MaxTokens: 4096,
	}
	if system != "" {
		params.System = []anthropic.TextBlockParam{{Text: system}}
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

	resp, err := a.client.Messages.New(ctx, params)
	if err != nil {
		return "", fmt.Errorf("llm-anthropic: %w", err)
	}
	var out string
	for _, b := range resp.Content {
		if b.Type == "text" {
			out += b.Text
		}
	}
	return out, nil
}
