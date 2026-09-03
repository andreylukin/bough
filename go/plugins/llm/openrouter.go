package llm

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
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
	effort, _ := cfg["effort"].(string) // "" = the provider's default
	switch effort {
	case "", "low", "medium", "high", "xhigh":
	default:
		return fmt.Errorf("llm-openrouter: effort must be low, medium, high or xhigh, got %q", effort)
	}
	ctx.Provide("llm", &openrouterLLM{model: model, effort: effort})
	return nil
}

type openrouterLLM struct {
	model    string
	effort   string // reasoning effort sent as {"reasoning": {"effort": …}}; "" omits it
	endpoint string // tests point this at a local server; "" = OpenRouter

	once sync.Once
	key  string
	err  error

	mu    sync.Mutex
	usage Usage
}

// Usage implements UsageReporter: OpenRouter returns a usage object
// on every response, cost included since the request asks for it.
// Model implements Modeler.
func (o *openrouterLLM) Model() string { return o.model }

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
	return o.call(ctx, system, messages, nil)
}

// Stream implements Streamer: the same request with stream:true,
// decoding OpenRouter's SSE "data:" lines. The final chunk carries
// usage (requested via usage.include), so the tally works either way.
func (o *openrouterLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	return o.call(ctx, system, messages, onDelta)
}

func (o *openrouterLLM) call(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
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

	payload := map[string]any{
		"model":    o.model,
		"messages": msgs,
		"usage":    map[string]any{"include": true},
		"stream":   onDelta != nil,
	}
	if o.effort != "" {
		payload["reasoning"] = map[string]any{"effort": o.effort}
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return "", fmt.Errorf("llm-openrouter: %w", err)
	}

	endpoint := o.endpoint
	if endpoint == "" {
		endpoint = "https://openrouter.ai/api/v1/chat/completions"
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body))
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
	if resp.StatusCode != http.StatusOK {
		data, _ := io.ReadAll(resp.Body)
		return "", openrouterErr(resp.StatusCode, o.model, data)
	}
	if onDelta != nil {
		return o.readStream(resp.Body, onDelta)
	}
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("llm-openrouter: %w", err)
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
		o.addUsage(u.PromptTokens, u.CompletionTokens, u.Cost)
	}
	return parsed.Choices[0].Message.Content, nil
}

func (o *openrouterLLM) addUsage(in, out int, cost float64) {
	o.mu.Lock()
	o.usage.InputTokens += in
	o.usage.OutputTokens += out
	o.usage.Cost += cost
	o.usage.Priced = true
	o.mu.Unlock()
}

// readStream decodes an SSE body: each "data: {json}" line carries a
// chunk with choices[0].delta.content; "data: [DONE]" ends it. An
// error object mid-stream (OpenRouter sends one for provider failures)
// surfaces as the call's error, with whatever text already arrived
// discarded by the caller.
func (o *openrouterLLM) readStream(body io.Reader, onDelta func(string)) (string, error) {
	var out strings.Builder
	sc := bufio.NewScanner(body)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		line := sc.Text()
		if !strings.HasPrefix(line, "data:") {
			continue // comments (": OPENROUTER PROCESSING") and blanks
		}
		data := strings.TrimSpace(strings.TrimPrefix(line, "data:"))
		if data == "[DONE]" {
			break
		}
		var chunk struct {
			Choices []struct {
				Delta struct {
					Content string `json:"content"`
				} `json:"delta"`
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
		if err := json.Unmarshal([]byte(data), &chunk); err != nil {
			return "", fmt.Errorf("llm-openrouter: bad stream chunk: %w", err)
		}
		if chunk.Error != nil {
			return "", fmt.Errorf("llm-openrouter: %s", chunk.Error.Message)
		}
		for _, c := range chunk.Choices {
			if c.Delta.Content != "" {
				out.WriteString(c.Delta.Content)
				onDelta(c.Delta.Content)
			}
		}
		if u := chunk.Usage; u != nil {
			o.addUsage(u.PromptTokens, u.CompletionTokens, u.Cost)
		}
	}
	if err := sc.Err(); err != nil {
		return "", fmt.Errorf("llm-openrouter: stream: %w", err)
	}
	return out.String(), nil
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
