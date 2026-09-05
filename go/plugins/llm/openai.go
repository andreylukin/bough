package llm

// llm-openai: OpenAI models over the Responses API (the gpt-5 family
// cannot combine reasoning with chat/completions). Stateless: every
// call replays the projected history as input items. Config: model
// (required), effort ("low" … "xhigh", optional, sent as
// reasoning.effort), base_url (default https://api.openai.com). Key:
// OPENAI_API_KEY. Streams via SSE: response.output_text.delta feeds
// the ui, response.completed carries the whole text and usage.

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
	kernel.Register("llm-openai", func() kernel.Plugin { return &openaiPlugin{} })
}

type openaiPlugin struct{}

func (p *openaiPlugin) Name() string     { return "llm-openai" }
func (p *openaiPlugin) Inject() []string { return nil }

func (p *openaiPlugin) Apply(ctx *kernel.Context, cfg map[string]any) error {
	model, ok := cfg["model"].(string)
	if !ok || model == "" {
		return fmt.Errorf("llm-openai: config needs model (string)")
	}
	o := &openaiLLM{model: model, base: "https://api.openai.com"}
	if e, ok := cfg["effort"].(string); ok {
		o.effort = e
	}
	if b, ok := cfg["base_url"].(string); ok && b != "" {
		o.base = strings.TrimRight(b, "/")
	}
	ctx.Provide(serviceKey(cfg), o)
	return nil
}

type openaiLLM struct {
	model  string
	effort string
	base   string

	once sync.Once
	key  string
	err  error

	mu    sync.Mutex
	usage Usage
}

// Model implements Modeler.
func (o *openaiLLM) Model() string { return o.model }

func (o *openaiLLM) Usage() Usage {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.usage
}

func (o *openaiLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	return o.call(ctx, system, messages, nil)
}

func (o *openaiLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	return o.call(ctx, system, messages, onDelta)
}

// body builds the Responses API request. Pure, so tests can pin it.
func (o *openaiLLM) body(system string, messages []Message, stream bool) map[string]any {
	input := make([]map[string]any, 0, len(messages))
	for _, m := range messages {
		role := "user"
		if m.Role == "assistant" {
			role = "assistant"
		}
		input = append(input, map[string]any{"role": role, "content": m.Content})
	}
	b := map[string]any{
		"model":  o.model,
		"input":  input,
		"store":  false,
		"stream": stream,
	}
	if system != "" {
		b["instructions"] = system
	}
	if e := o.Effort(); e != "" && e != "off" {
		b["reasoning"] = map[string]any{"effort": e}
	}
	return b
}

// Effort is the reasoning level in force; SetEffort changes it
// (/think). The Responses API streams reasoning SUMMARIES as their own
// event type, which is not wired up: /think changes how hard it
// thinks, but openai's thinking is not shown.
func (o *openaiLLM) Effort() string {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.effort
}

func (o *openaiLLM) SetEffort(level string) error {
	if !ValidEffort(level) {
		return fmt.Errorf("llm-openai: unknown thinking level %q", level)
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	o.effort = level
	return nil
}

// init reads the API key once. Separate from call so Ready can ask
// whether this provider is usable without making a request.
func (o *openaiLLM) init() error {
	o.once.Do(func() {
		o.key = os.Getenv("OPENAI_API_KEY")
		if o.key == "" {
			o.err = MissingKey("llm-openai", "OPENAI_API_KEY")
		}
	})
	return o.err
}

// Ready reports whether this provider is configured (see llm.Ready).
func (o *openaiLLM) Ready() error { return o.init() }

func (o *openaiLLM) call(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	if err := o.init(); err != nil {
		return "", err
	}
	body, err := json.Marshal(o.body(system, messages, onDelta != nil))
	if err != nil {
		return "", fmt.Errorf("llm-openai: %w", err)
	}
	delivered := false
	return withRetries(ctx, func() (string, bool, error) {
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, o.base+"/v1/responses", bytes.NewReader(body))
		if err != nil {
			return "", false, fmt.Errorf("llm-openai: %w", err)
		}
		req.Header.Set("Authorization", "Bearer "+o.key)
		req.Header.Set("Content-Type", "application/json")
		resp, err := httpClient.Do(req)
		if err != nil {
			return "", retryableErr(err), fmt.Errorf("llm-openai: %w", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			data, _ := io.ReadAll(resp.Body)
			return "", retryableStatus(resp.StatusCode), openaiErr(resp.StatusCode, o.model, data)
		}
		if onDelta != nil {
			out, err := o.readStream(guardStalls(resp.Body, stallTimeout), func(d string) { delivered = true; onDelta(d) })
			return out, err != nil && !delivered && retryableErr(err), err
		}
		data, err := io.ReadAll(resp.Body)
		if err != nil {
			return "", retryableErr(err), fmt.Errorf("llm-openai: %w", err)
		}
		out, err := o.parse(data)
		return out, false, err
	})
}

// parse reads a non-streaming Responses body.
func (o *openaiLLM) parse(data []byte) (string, error) {
	var r openaiResponse
	if err := json.Unmarshal(data, &r); err != nil {
		return "", fmt.Errorf("llm-openai: bad response: %w", err)
	}
	return o.finish(&r)
}

// openaiResponse is the slice of a Responses API response object we read.
type openaiResponse struct {
	Status string `json:"status"`
	Output []struct {
		Type    string `json:"type"`
		Content []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"content"`
	} `json:"output"`
	IncompleteDetails *struct {
		Reason string `json:"reason"`
	} `json:"incomplete_details"`
	Usage *struct {
		InputTokens  int `json:"input_tokens"`
		OutputTokens int `json:"output_tokens"`
		// OpenAI caches prompts automatically; cached_tokens is the
		// share served from the cache, counted inside input_tokens.
		InputTokensDetails *struct {
			CachedTokens int `json:"cached_tokens"`
		} `json:"input_tokens_details"`
	} `json:"usage"`
	Error *struct {
		Message string `json:"message"`
	} `json:"error"`
}

// finish extracts the text and tallies usage from a completed response.
func (o *openaiLLM) finish(r *openaiResponse) (string, error) {
	if r.Error != nil {
		return "", fmt.Errorf("llm-openai: %s", r.Error.Message)
	}
	// A Responses reply can be several message items — gpt-5.6 sends a
	// short preamble and then the answer — and each item several text
	// parts. Concatenated bare they run together mid-sentence ("…open
	// the PR.I need the target image tag…"), which reads as one
	// confused paragraph and hides that the model said two things.
	var parts []string
	for _, item := range r.Output {
		if item.Type != "message" {
			continue
		}
		for _, c := range item.Content {
			if c.Type == "output_text" && c.Text != "" {
				parts = append(parts, c.Text)
			}
		}
	}
	var out strings.Builder
	out.WriteString(strings.Join(parts, "\n\n"))
	if u := r.Usage; u != nil {
		cached := 0
		if u.InputTokensDetails != nil {
			cached = u.InputTokensDetails.CachedTokens
		}
		o.mu.Lock()
		o.usage.InputTokens += u.InputTokens
		o.usage.OutputTokens += u.OutputTokens
		o.usage.LastInputTokens = u.InputTokens
		o.usage.CacheReadTokens += cached
		o.mu.Unlock()
	}
	if r.Status == "incomplete" && r.IncompleteDetails != nil && r.IncompleteDetails.Reason == "max_output_tokens" {
		return out.String(), fmt.Errorf("llm-openai: reply cut at max_output_tokens")
	}
	return out.String(), nil
}

// readStream decodes the SSE event stream: output_text deltas feed
// onDelta; the terminal response.completed/incomplete event carries
// the response object the reply and usage come from; response.failed
// or error events surface as the call's error.
func (o *openaiLLM) readStream(body io.Reader, onDelta func(string)) (string, error) {
	sc := bufio.NewScanner(body)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	var streamed strings.Builder
	for sc.Scan() {
		line := sc.Text()
		if !strings.HasPrefix(line, "data:") {
			continue
		}
		data := strings.TrimSpace(strings.TrimPrefix(line, "data:"))
		if data == "[DONE]" {
			break
		}
		var ev struct {
			Type     string          `json:"type"`
			Delta    string          `json:"delta"`
			Response *openaiResponse `json:"response"`
			Error    *struct {
				Message string `json:"message"`
			} `json:"error"`
		}
		if err := json.Unmarshal([]byte(data), &ev); err != nil {
			return "", fmt.Errorf("llm-openai: bad stream event: %w", err)
		}
		switch ev.Type {
		case "response.output_item.added":
			// A second message item starts: separate it as finish()
			// does, or the live view runs two sentences together.
			if streamed.Len() > 0 {
				streamed.WriteString("\n\n")
				onDelta("\n\n")
			}
		case "response.output_text.delta":
			if ev.Delta != "" {
				streamed.WriteString(ev.Delta)
				onDelta(ev.Delta)
			}
		case "response.completed", "response.incomplete":
			if ev.Response != nil {
				return o.finish(ev.Response)
			}
			return streamed.String(), nil
		case "response.failed":
			msg := "response failed"
			if ev.Response != nil && ev.Response.Error != nil {
				msg = ev.Response.Error.Message
			}
			return "", fmt.Errorf("llm-openai: %s", msg)
		case "error":
			msg := "stream error"
			if ev.Error != nil {
				msg = ev.Error.Message
			}
			return "", fmt.Errorf("llm-openai: %s", msg)
		}
	}
	if err := sc.Err(); err != nil {
		return "", fmt.Errorf("llm-openai: stream: %w", err)
	}
	return "", fmt.Errorf("llm-openai: stream ended without response.completed")
}

// openaiErr formats a non-200: a 404 is an unknown model; only the
// parsed error message is quoted, never the raw body.
func openaiErr(status int, model string, data []byte) error {
	var parsed struct {
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	msg := ""
	if json.Unmarshal(data, &parsed) == nil && parsed.Error != nil {
		msg = parsed.Error.Message
	}
	if status == http.StatusNotFound {
		return fmt.Errorf("llm-openai: model %q not found on openai — switch with /model", model)
	}
	if msg == "" {
		msg = http.StatusText(status)
	}
	return fmt.Errorf("llm-openai: HTTP %d: %s", status, msg)
}
