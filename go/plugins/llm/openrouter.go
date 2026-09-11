package llm

import (
	"bufio"
	"bytes"
	"cmp"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
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
	case "", "off", "low", "medium", "high", "xhigh":
	default:
		return fmt.Errorf("llm-openrouter: effort must be off, low, medium, high or xhigh, got %q", effort)
	}
	jsTool := strings.HasPrefix(model, "google/")
	if v, ok := cfg["js_tool"].(bool); ok {
		jsTool = v
	}
	ctx.Provide(serviceKey(cfg), &openrouterLLM{model: model, effort: effort, jsTool: jsTool})
	return nil
}

type openrouterLLM struct {
	model    string
	effort   string // reasoning effort sent as {"reasoning": {"effort": …}}; "" omits it
	endpoint string // tests point this at a local server; "" = OpenRouter
	// jsTool declares one native function, js(code), and turns the
	// model's call into the ```js fence the loop reads. Gemini plans a
	// function call in its thinking and, with no function to call,
	// emits one Google rejects (MALFORMED_FUNCTION_CALL); a declared
	// function makes that the happy path. On for google/ models.
	jsTool bool

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
	Role      string `json:"role"`
	Content   string `json:"content"`
	ToolCalls []struct {
		Function struct {
			Name      string `json:"name"`
			Arguments string `json:"arguments"`
		} `json:"function"`
	} `json:"tool_calls"`
}

// orRequestMessage is an outgoing message. Content is the usual
// string, or OpenRouter's array of text parts when the system prompt
// is marked for caching (see orPart).
type orRequestMessage struct {
	Role    string `json:"role"`
	Content any    `json:"content"`
}

// orPart is one text part of a request message; CacheControl marks it
// as a cache breakpoint, OpenRouter's normalised form — translated to
// prompt_cache_breakpoint toward OpenAI models, and for Anthropic
// models the only way anything is cached at all.
type orPart struct {
	Type         string          `json:"type"`
	Text         string          `json:"text,omitempty"`
	CacheControl *orCacheControl `json:"cache_control,omitempty"`
	ImageURL     *orImageURL     `json:"image_url,omitempty"`
}

type orImageURL struct {
	URL string `json:"url"`
}

type orCacheControl struct {
	Type string `json:"type"`
}

func (o *openrouterLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	return o.call(ctx, system, messages, nil, nil)
}

// Stream implements Streamer: the same request with stream:true,
// decoding OpenRouter's SSE "data:" lines. The final chunk carries
// usage (requested via usage.include), so the tally works either way.
func (o *openrouterLLM) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	return o.call(ctx, system, messages, onDelta, nil)
}

// StreamThinking implements ThinkingStreamer: OpenRouter puts the
// model's reasoning in delta.reasoning (some providers send
// reasoning_content), separate from the reply, so it can be shown
// without ever entering the conversation.
func (o *openrouterLLM) StreamThinking(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error) {
	return o.call(ctx, system, messages, onDelta, onThink)
}

// Effort is the reasoning level in force; "" means the provider's own.
func (o *openrouterLLM) Effort() string {
	o.mu.Lock()
	defer o.mu.Unlock()
	return o.effort
}

// SetEffort changes it for the next request (/think).
func (o *openrouterLLM) SetEffort(level string) error {
	if !ValidEffort(level) {
		return fmt.Errorf("llm-openrouter: unknown thinking level %q", level)
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	o.effort = level
	return nil
}

// init reads the API key once. Separate from call so Ready can ask
// whether this provider is usable without making a request.
func (o *openrouterLLM) init() error {
	o.once.Do(func() {
		o.key = os.Getenv("OPENROUTER_API_KEY")
		if o.key == "" {
			o.err = MissingKey("llm-openrouter", "OPENROUTER_API_KEY")
		}
	})
	return o.err
}

// Ready reports whether this provider is configured (see llm.Ready).
func (o *openrouterLLM) Ready() error { return o.init() }

func (o *openrouterLLM) call(ctx context.Context, system string, messages []Message, onDelta, onThink func(string)) (string, error) {
	if err := o.init(); err != nil {
		return "", err
	}

	// build marshals the request; nudge, when set, is one more user
	// line appended for a retry after a malformed-call failure.
	build := func(nudge string) ([]byte, error) {
		var msgs []orRequestMessage
		if system != "" {
			// cache_control on the system prompt as text parts: the same
			// marker llm-anthropic sends natively. Anthropic models behind
			// OpenRouter cache nothing without it; providers that cache
			// automatically ignore or translate it.
			msgs = append(msgs, orRequestMessage{Role: "system", Content: []orPart{{
				Type:         "text",
				Text:         system,
				CacheControl: &orCacheControl{Type: "ephemeral"},
			}}})
		}
		for _, m := range messages {
			role := m.Role
			if role != "assistant" {
				role = "user"
			}
			var content any = m.Content
			if imgs := loadImages(m.Images); role == "user" && len(imgs) > 0 {
				parts := []orPart{{Type: "text", Text: m.Content}}
				for _, img := range imgs {
					parts = append(parts, orPart{Type: "image_url", ImageURL: &orImageURL{URL: img.dataURL()}})
				}
				content = parts
			}
			msgs = append(msgs, orRequestMessage{Role: role, Content: content})
		}
		if nudge != "" {
			msgs = append(msgs, orRequestMessage{Role: "user", Content: nudge})
		}
		payload := map[string]any{
			"model":    o.model,
			"messages": msgs,
			"usage":    map[string]any{"include": true},
			"stream":   onDelta != nil,
			// bough is code-mode: the reply is a code fence, never a
			// native tool call. Gemini still tries one now and then and
			// Google ends the stream with MALFORMED_FUNCTION_CALL; "none"
			// maps to its function-calling mode NONE.
			"tool_choice": "none",
		}
		if o.jsTool {
			payload["tools"] = []any{jsToolDecl}
			payload["tool_choice"] = "auto"
		}
		switch effort := o.Effort(); effort {
		case "":
		case "off":
			payload["reasoning"] = map[string]any{"exclude": true, "enabled": false}
		default:
			payload["reasoning"] = map[string]any{"effort": effort}
		}
		return json.Marshal(payload)
	}
	var lastErr error

	endpoint := o.endpoint
	if endpoint == "" {
		endpoint = "https://openrouter.ai/api/v1/chat/completions"
	}
	if e := os.Getenv("OPENROUTER_ENDPOINT"); e != "" {
		endpoint = e // a logging proxy, when debugging what a provider does with a request
	}
	// A stream that has already delivered deltas is never retried (the
	// caller has seen partial text); everything before the first delta is.
	delivered := false
	return withRetries(ctx, func() (string, bool, error) {
		nudge := ""
		if lastErr != nil && strings.Contains(lastErr.Error(), "MALFORMED_FUNCTION_CALL") {
			// The same request would likely fail the same way: tell the
			// model what happened and what shape the reply must take.
			nudge = malformedNudge
		}
		body, err := build(nudge)
		if err != nil {
			return "", false, fmt.Errorf("llm-openrouter: %w", err)
		}
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body))
		if err != nil {
			return "", false, fmt.Errorf("llm-openrouter: %w", err)
		}
		req.Header.Set("Authorization", "Bearer "+o.key)
		req.Header.Set("Content-Type", "application/json")
		resp, err := httpClient.Do(req)
		if err != nil {
			return "", retryableErr(err), fmt.Errorf("llm-openrouter: %w", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			data, _ := io.ReadAll(resp.Body)
			return "", retryableStatus(resp.StatusCode), withRetryAfter(openrouterErr(resp.StatusCode, o.model, data), resp.Header)
		}
		if onDelta != nil {
			out, err := o.readStream(guardStalls(resp.Body, stallTimeout), func(d string) { delivered = true; onDelta(d) }, onThink)
			lastErr = err
			return out, err != nil && !delivered && retryableErr(err), err
		}
		data, err := io.ReadAll(resp.Body)
		if err != nil {
			return "", retryableErr(err), fmt.Errorf("llm-openrouter: %w", err)
		}
		return o.parse(data)
	})
}

func sortedKeys(m map[int]string) []int {
	keys := make([]int, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Ints(keys)
	return keys
}

// jsToolDecl is the one function a jsTool model may call. The loop
// runs the code exactly as it runs a ```js fence.
var jsToolDecl = map[string]any{
	"type": "function",
	"function": map[string]any{
		"name":        "js",
		"description": "Run JavaScript in the sandbox described in the system prompt (the tools.* API). Equivalent to writing the code in a ```js fence; use either.",
		"parameters": map[string]any{
			"type":       "object",
			"properties": map[string]any{"code": map[string]any{"type": "string", "description": "the JavaScript to run"}},
			"required":   []string{"code"},
		},
	},
}

// jsFence renders a js tool call's arguments as the fence the loop
// reads; "" when the arguments carry no code.
func jsFence(args string) string {
	var a struct {
		Code string `json:"code"`
	}
	if err := json.Unmarshal([]byte(args), &a); err != nil || strings.TrimSpace(a.Code) == "" {
		return ""
	}
	return "\n```js\n" + strings.TrimRight(a.Code, "\n") + "\n```\n"
}

// malformedNudge is appended to a retry after Google rejected the
// model's reply as a malformed function call.
const malformedNudge = "(Your previous reply was rejected by the provider as a malformed function call. There are no native functions here: reply in plain text, and put any code in a single ```js fence.)"

// parse reads a non-streaming completion body: the reply text, usage.
func (o *openrouterLLM) parse(data []byte) (string, bool, error) {

	var parsed struct {
		Choices []struct {
			Message orMessage `json:"message"`
		} `json:"choices"`
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
		Usage *struct {
			PromptTokens        int                    `json:"prompt_tokens"`
			CompletionTokens    int                    `json:"completion_tokens"`
			Cost                float64                `json:"cost"`
			PromptTokensDetails *orPromptTokensDetails `json:"prompt_tokens_details"`
		} `json:"usage"`
	}
	if err := json.Unmarshal(data, &parsed); err != nil {
		return "", false, fmt.Errorf("llm-openrouter: bad response: %w", err)
	}
	if parsed.Error != nil {
		return "", false, fmt.Errorf("llm-openrouter: %s", parsed.Error.Message)
	}
	if len(parsed.Choices) == 0 {
		return "", false, fmt.Errorf("llm-openrouter: no choices in response")
	}
	if u := parsed.Usage; u != nil {
		o.addUsage(u.PromptTokens, u.CompletionTokens, u.Cost, u.PromptTokensDetails)
	}
	msg := parsed.Choices[0].Message
	text := msg.Content
	for _, tc := range msg.ToolCalls {
		text += jsFence(tc.Function.Arguments)
	}
	return text, false, nil
}

// orPromptTokensDetails is OpenRouter's cache counters, counted
// inside prompt_tokens: cached_tokens read from the cache,
// cache_write_tokens written to it (the first request of a
// conversation).
type orPromptTokensDetails struct {
	CachedTokens     int `json:"cached_tokens"`
	CacheWriteTokens int `json:"cache_write_tokens"`
}

func (o *openrouterLLM) addUsage(in, out int, cost float64, d *orPromptTokensDetails) {
	o.mu.Lock()
	o.usage.InputTokens += in
	o.usage.OutputTokens += out
	o.usage.LastInputTokens = in
	o.usage.Cost += cost
	o.usage.Priced = true
	if d != nil {
		o.usage.CacheReadTokens += d.CachedTokens
		o.usage.CacheCreationTokens += d.CacheWriteTokens
	}
	o.mu.Unlock()
}

// readStream decodes an SSE body: each "data: {json}" line carries a
// chunk with choices[0].delta.content; "data: [DONE]" ends it. An
// error object mid-stream (OpenRouter sends one for provider failures)
// surfaces as the call's error, with whatever text already arrived
// discarded by the caller.
func (o *openrouterLLM) readStream(body io.Reader, onDelta, onThink func(string)) (string, error) {
	calls := map[int]string{} // native js tool calls by index, arguments so far
	var out strings.Builder
	truncated := false
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
					Content   string `json:"content"`
					ToolCalls []struct {
						Index    int `json:"index"`
						Function struct {
							Name      string `json:"name"`
							Arguments string `json:"arguments"`
						} `json:"function"`
					} `json:"tool_calls"`
					// OpenRouter normalises reasoning to "reasoning";
					// providers proxied raw (deepseek, glm) send
					// "reasoning_content". Take whichever arrives.
					Reasoning        string `json:"reasoning"`
					ReasoningContent string `json:"reasoning_content"`
				} `json:"delta"`
				FinishReason       string `json:"finish_reason"`
				NativeFinishReason string `json:"native_finish_reason"`
				// OpenRouter puts a provider's mid-stream failure here
				// (Google overloaded, a safety stop) with finish_reason
				// "error"; the message is the only clue.
				Error *struct {
					Message string `json:"message"`
					Code    any    `json:"code"`
				} `json:"error"`
			} `json:"choices"`
			Error *struct {
				Message string `json:"message"`
			} `json:"error"`
			Usage *struct {
				PromptTokens        int                    `json:"prompt_tokens"`
				CompletionTokens    int                    `json:"completion_tokens"`
				Cost                float64                `json:"cost"`
				PromptTokensDetails *orPromptTokensDetails `json:"prompt_tokens_details"`
			} `json:"usage"`
		}
		if err := json.Unmarshal([]byte(data), &chunk); err != nil {
			return "", fmt.Errorf("llm-openrouter: bad stream chunk: %w", err)
		}
		if chunk.Error != nil {
			return "", fmt.Errorf("llm-openrouter: %s", chunk.Error.Message)
		}
		for _, c := range chunk.Choices {
			if onThink != nil {
				if t := cmp.Or(c.Delta.Reasoning, c.Delta.ReasoningContent); t != "" {
					onThink(t)
				}
			}
			if c.Delta.Content != "" {
				out.WriteString(c.Delta.Content)
				onDelta(c.Delta.Content)
			}
			for _, tc := range c.Delta.ToolCalls {
				// Arguments stream in pieces; keep them by index and
				// render the fence when the stream ends.
				calls[tc.Index] += tc.Function.Arguments
			}
			// A stream that stops for a reason other than the model
			// finishing (provider "error", "content_filter") would
			// otherwise come back as a silent empty reply.
			switch fr := c.FinishReason; {
			case fr == "length":
				// Cut off at the output cap. The text so far is real
				// and worth keeping, but it is not an answer, so it is
				// marked rather than returned as if complete.
				truncated = true
			case fr == "error":
				// A provider failure, not an answer: say what the
				// provider said, and let the caller retry the request
				// when nothing of the reply was delivered yet.
				detail := c.NativeFinishReason
				if c.Error != nil && c.Error.Message != "" {
					detail = c.Error.Message
					if c.Error.Code != nil {
						detail = fmt.Sprintf("%v %s", c.Error.Code, detail)
					}
				}
				return "", fmt.Errorf("llm-openrouter: provider error mid-stream: %s", strings.TrimSpace(detail))
			case fr != "" && fr != "stop" && fr != "tool_calls":
				return "", fmt.Errorf("llm-openrouter: stream ended: %s", fr)
			}
			if fr := c.FinishReason; fr == "stop" || fr == "tool_calls" {
				for _, i := range sortedKeys(calls) {
					if f := jsFence(calls[i]); f != "" {
						out.WriteString(f)
						onDelta(f)
					}
				}
				calls = map[int]string{}
			}
		}
		if u := chunk.Usage; u != nil {
			o.addUsage(u.PromptTokens, u.CompletionTokens, u.Cost, u.PromptTokensDetails)
		}
	}
	if err := sc.Err(); err != nil {
		return "", fmt.Errorf("llm-openrouter: stream: %w", err)
	}
	if truncated {
		return MarkTruncated(out.String()), nil
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
