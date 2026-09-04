package llm

// llm-cerebras: Cerebras Inference over its OpenAI-compatible
// chat/completions endpoint. Config: model (required, e.g.
// gpt-oss-120b or qwen-3.8-27b), effort (optional, sent as
// reasoning_effort). Key: CEREBRAS_API_KEY. Cerebras reports token
// counts but no dollar cost, so the cost plugin prices the tally
// from its table.

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

// cerebrasURL is a var so a test can point the client at a stub.
var cerebrasURL = "https://api.cerebras.ai/v1/chat/completions"

func init() {
	kernel.Register("llm-cerebras", func() kernel.Plugin { return &cerebrasPlugin{} })
}

type cerebrasPlugin struct{}

func (p *cerebrasPlugin) Name() string     { return "llm-cerebras" }
func (p *cerebrasPlugin) Inject() []string { return nil }

func (p *cerebrasPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	model, ok := cfg["model"].(string)
	if !ok || model == "" {
		return fmt.Errorf("llm-cerebras: config needs model (string)")
	}
	c := &cerebrasLLM{model: model}
	if e, ok := cfg["effort"].(string); ok {
		c.effort = e
	}
	ctx.Provide(serviceKey(cfg), c)
	return nil
}

type cerebrasLLM struct {
	model  string
	effort string

	once sync.Once
	key  string
	err  error

	mu    sync.Mutex
	usage Usage
}

// Model implements Modeler.
func (c *cerebrasLLM) Model() string { return c.model }

// Usage implements UsageReporter: token counts only, never Priced.
func (c *cerebrasLLM) Usage() Usage {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.usage
}

func (c *cerebrasLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	return c.call(ctx, system, messages, nil, nil)
}

// Stream implements Streamer: stream:true with stream_options
// include_usage so the last chunk carries the token tally.
func (c *cerebrasLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	return c.call(ctx, system, messages, onDelta, nil)
}

// StreamThinking implements ThinkingStreamer (delta.reasoning).
func (c *cerebrasLLM) StreamThinking(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error) {
	return c.call(ctx, system, messages, onDelta, onThink)
}

// Effort is the reasoning level in force; SetEffort changes it (/think).
func (c *cerebrasLLM) Effort() string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.effort
}

func (c *cerebrasLLM) SetEffort(level string) error {
	if !ValidEffort(level) {
		return fmt.Errorf("llm-cerebras: unknown thinking level %q", level)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.effort = level
	return nil
}

func (c *cerebrasLLM) call(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error) {
	c.once.Do(func() {
		c.key = os.Getenv("CEREBRAS_API_KEY")
		if c.key == "" {
			c.err = fmt.Errorf("llm-cerebras: CEREBRAS_API_KEY not set")
		}
	})
	if c.err != nil {
		return "", c.err
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
		"model":    c.model,
		"messages": msgs,
		"stream":   onDelta != nil,
	}
	if onDelta != nil {
		payload["stream_options"] = map[string]any{"include_usage": true}
	}
	if e := c.Effort(); e != "" && e != "off" {
		payload["reasoning_effort"] = e
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return "", fmt.Errorf("llm-cerebras: %w", err)
	}

	delivered := false
	return withRetries(ctx, func() (string, bool, error) {
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, cerebrasURL, bytes.NewReader(body))
		if err != nil {
			return "", false, fmt.Errorf("llm-cerebras: %w", err)
		}
		req.Header.Set("Authorization", "Bearer "+c.key)
		req.Header.Set("Content-Type", "application/json")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			return "", retryableErr(err), fmt.Errorf("llm-cerebras: %w", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			data, _ := io.ReadAll(resp.Body)
			return "", retryableStatus(resp.StatusCode), cerebrasErr(resp.StatusCode, c.model, data)
		}
		if onDelta != nil {
			// A stream that already delivered text is never retried:
			// the user would see the reply twice.
			out, err := c.readStream(resp.Body, func(d string) { delivered = true; onDelta(d) }, onThink)
			return out, err != nil && !delivered && retryableErr(err), err
		}
		data, err := io.ReadAll(resp.Body)
		if err != nil {
			return "", retryableErr(err), fmt.Errorf("llm-cerebras: %w", err)
		}

		var parsed struct {
			Choices []struct {
				Message orMessage `json:"message"`
			} `json:"choices"`
			Error *struct {
				Message string `json:"message"`
			} `json:"error"`
			Usage *struct {
				PromptTokens     int `json:"prompt_tokens"`
				CompletionTokens int `json:"completion_tokens"`
			} `json:"usage"`
		}
		if err := json.Unmarshal(data, &parsed); err != nil {
			return "", false, fmt.Errorf("llm-cerebras: bad response: %w", err)
		}
		if parsed.Error != nil {
			return "", false, fmt.Errorf("llm-cerebras: %s", parsed.Error.Message)
		}
		if len(parsed.Choices) == 0 {
			return "", false, fmt.Errorf("llm-cerebras: no choices in response")
		}
		if u := parsed.Usage; u != nil {
			c.addUsage(u.PromptTokens, u.CompletionTokens)
		}
		return parsed.Choices[0].Message.Content, false, nil
	})
}

func (c *cerebrasLLM) addUsage(in, out int) {
	c.mu.Lock()
	c.usage.InputTokens += in
	c.usage.OutputTokens += out
	c.usage.LastInputTokens = in
	c.mu.Unlock()
}

// readStream decodes an SSE body: each "data: {json}" line carries a
// chunk with choices[0].delta.content; "data: [DONE]" ends it. The
// usage-only final chunk has no choices.
func (c *cerebrasLLM) readStream(body io.Reader, onDelta, onThink func(string)) (string, error) {
	var out strings.Builder
	sc := bufio.NewScanner(body)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		line := sc.Text()
		if !strings.HasPrefix(line, "data:") {
			continue
		}
		data := strings.TrimSpace(strings.TrimPrefix(line, "data:"))
		if data == "[DONE]" {
			break
		}
		var chunk struct {
			Choices []struct {
				Delta struct {
					Content   string `json:"content"`
					Reasoning string `json:"reasoning"`
				} `json:"delta"`
			} `json:"choices"`
			Error *struct {
				Message string `json:"message"`
			} `json:"error"`
			Usage *struct {
				PromptTokens     int `json:"prompt_tokens"`
				CompletionTokens int `json:"completion_tokens"`
			} `json:"usage"`
		}
		if err := json.Unmarshal([]byte(data), &chunk); err != nil {
			return "", fmt.Errorf("llm-cerebras: bad stream chunk: %w", err)
		}
		if chunk.Error != nil {
			return "", fmt.Errorf("llm-cerebras: %s", chunk.Error.Message)
		}
		for _, ch := range chunk.Choices {
			if onThink != nil && ch.Delta.Reasoning != "" {
				onThink(ch.Delta.Reasoning)
			}
			if ch.Delta.Content != "" {
				out.WriteString(ch.Delta.Content)
				onDelta(ch.Delta.Content)
			}
		}
		if u := chunk.Usage; u != nil {
			c.addUsage(u.PromptTokens, u.CompletionTokens)
		}
	}
	if err := sc.Err(); err != nil {
		return "", fmt.Errorf("llm-cerebras: stream: %w", err)
	}
	return out.String(), nil
}

// cerebrasErr formats a non-200 response; a 404 is an unknown model
// id. Only the parsed error message is quoted, never the raw body.
func cerebrasErr(status int, model string, data []byte) error {
	var parsed struct {
		Message string `json:"message"`
		Error   *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	msg := ""
	if json.Unmarshal(data, &parsed) == nil {
		if parsed.Error != nil {
			msg = parsed.Error.Message
		} else {
			msg = parsed.Message
		}
	}
	if status == http.StatusNotFound {
		if msg != "" {
			return fmt.Errorf("llm-cerebras: model %q not found on cerebras (%s) — switch with /model", model, msg)
		}
		return fmt.Errorf("llm-cerebras: model %q not found on cerebras — switch with /model", model)
	}
	if msg == "" {
		msg = http.StatusText(status)
	}
	return fmt.Errorf("llm-cerebras: HTTP %d: %s", status, msg)
}
