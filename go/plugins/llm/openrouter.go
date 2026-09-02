package llm

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"sync"

	"github.com/andreylukin/bough/kernel"
)

func init() {
	kernel.Register("llm-openrouter", func() kernel.Plugin { return &openrouterPlugin{} })
}

type openrouterPlugin struct{}

func (p *openrouterPlugin) Name() string     { return "llm-openrouter" }
func (p *openrouterPlugin) Inject() []string { return nil }

func (p *openrouterPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	model, ok := cfg["model"].(string)
	if !ok || model == "" {
		return fmt.Errorf("llm-openrouter: config needs model (string)")
	}
	ctx.Provide("llm", &openrouterLLM{model: model})
	return nil
}

type openrouterLLM struct {
	model string

	once sync.Once
	key  string
	err  error

	mu    sync.Mutex
	usage Usage
}

// Usage implements UsageReporter: OpenRouter returns a usage object
// on every response, cost included since the request asks for it.
func (o *openrouterLLM) Usage() Usage {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.usage
}

type orMessage struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

func (o *openrouterLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	o.once.Do(func() {
		o.key = os.Getenv("OPENROUTER_API_KEY")
		if o.key == "" {
			o.err = fmt.Errorf("llm-openrouter: OPENROUTER_API_KEY not set")
		}
	})
	if o.err != nil {
		return "", o.err
	}

	var msgs []orMessage
	if system != "" {
		msgs = append(msgs, orMessage{Role: "system", Content: system})
	}
	for _, m := range messages {
		role := m.Role
		if role != "assistant" {
			role = "user"
		}
		msgs = append(msgs, orMessage{Role: role, Content: m.Content})
	}

	body, err := json.Marshal(map[string]any{
		"model":    o.model,
		"messages": msgs,
		"usage":    map[string]any{"include": true},
	})
	if err != nil {
		return "", fmt.Errorf("llm-openrouter: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost,
		"https://openrouter.ai/api/v1/chat/completions", bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("llm-openrouter: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+o.key)
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("llm-openrouter: %w", err)
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("llm-openrouter: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return "", openrouterErr(resp.StatusCode, o.model, data)
	}

	var parsed struct {
		Choices []struct {
			Message orMessage `json:"message"`
		} `json:"choices"`
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
		Usage *struct {
			PromptTokens     int     `json:"prompt_tokens"`
			CompletionTokens int     `json:"completion_tokens"`
			Cost             float64 `json:"cost"`
		} `json:"usage"`
	}
	if err := json.Unmarshal(data, &parsed); err != nil {
		return "", fmt.Errorf("llm-openrouter: bad response: %w", err)
	}
	if parsed.Error != nil {
		return "", fmt.Errorf("llm-openrouter: %s", parsed.Error.Message)
	}
	if len(parsed.Choices) == 0 {
		return "", fmt.Errorf("llm-openrouter: no choices in response")
	}
	if u := parsed.Usage; u != nil {
		o.mu.Lock()
		o.usage.InputTokens += u.PromptTokens
		o.usage.OutputTokens += u.CompletionTokens
		o.usage.Cost += u.Cost
		o.usage.Priced = true
		o.mu.Unlock()
	}
	return parsed.Choices[0].Message.Content, nil
}

// openrouterErr formats a non-200 response. A 400/404 is almost
// always an unknown model id, so it reads as such; only the parsed
// error message is quoted, never the raw body (its metadata carries
// the account's user_id).
func openrouterErr(status int, model string, data []byte) error {
	var parsed struct {
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	msg := ""
	if json.Unmarshal(data, &parsed) == nil && parsed.Error != nil {
		msg = parsed.Error.Message
	}
	if status == http.StatusBadRequest || status == http.StatusNotFound {
		if msg != "" {
			return fmt.Errorf("llm-openrouter: model %q not found on openrouter (%s) — switch with /model", model, msg)
		}
		return fmt.Errorf("llm-openrouter: model %q not found on openrouter — switch with /model", model)
	}
	if msg == "" {
		msg = http.StatusText(status)
	}
	return fmt.Errorf("llm-openrouter: HTTP %d: %s", status, msg)
}
