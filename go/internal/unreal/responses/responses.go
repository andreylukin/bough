// Package responses builds the harness Responses API adapter for the
// llm rows that speak it: OpenAI, OpenRouter and Ollama. bough builds
// the adapter itself rather than through the harness clients because
// those hardcode their own http.Client, and bough needs its bounded one
// with a tap on the SSE body, so replies stream to the UI while the
// harness reads the same bytes for the final response.
package responses

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"net/http"
	"strings"
	"sync/atomic"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	"github.com/unreallabsai/unreal-agent/harness/llm/responsesapi"
	"github.com/unreallabsai/unreal-agent/harness/primitives"

	"github.com/andreylukin/bough/internal/agentllm"
)

// Config is one row's adapter for one session.
type Config struct {
	Kind        string                 // "openai" | "openrouter" | "ollama"
	APIKey      func() (string, error) // nil for ollama
	BaseURL     string                 // "" = the kind's default
	Model       func() string
	Effort      func() string
	MaxTokens   int64
	CacheTTL    string // openrouter Extensions cache_control ttl; "1h" default
	MaxAttempts int    // 0 = harness default 5
	HTTP        *http.Client
	Options     agentllm.Options
}

// Default endpoints, as the harness clients at the pin have them.
const (
	OpenAIBase     = "https://api.openai.com/v1"
	OpenRouterBase = "https://openrouter.ai/api/v1"
	OllamaBase     = "http://localhost:11434/v1"
)

type adapter struct {
	c      Config
	inner  ullm.Adapter
	remote *primitives.RemoteClient
}

// New builds the adapter. The key is read once, here: a /connect
// remounts the llm row, which builds a new adapter.
func New(c Config) (agentllm.Adapter, error) {
	row := "llm-" + c.Kind
	if c.Model == nil {
		return nil, fmt.Errorf("%s: the adapter needs the row's model", row)
	}
	if c.Effort == nil {
		c.Effort = func() string { return "" }
	}
	base := strings.TrimRight(strings.TrimSpace(c.BaseURL), "/")
	cfg := responsesapi.Config{Headers: map[string][]string{"Content-Type": {"application/json"}}}
	switch c.Kind {
	case "openai":
		if base == "" {
			base = OpenAIBase
		}
		cfg.CacheKeyPlacement = responsesapi.CacheKeyPlacement{UsePromptCacheKeyField: true}
	case "openrouter":
		if base == "" {
			base = OpenRouterBase
		}
		ttl := c.CacheTTL
		switch ttl {
		case "":
			ttl = "1h"
		case "1h", "5m":
		default:
			return nil, fmt.Errorf("%s: cache_ttl must be 1h or 5m, got %q", row, ttl)
		}
		// Same routing as the harness client: every turn of a session on
		// one warm upstream, and an explicit breakpoint, without which an
		// Anthropic model behind OpenRouter caches nothing.
		cfg.CacheKeyPlacement = responsesapi.CacheKeyPlacement{Header: "x-session-id"}
		cfg.Extensions = map[string]jsontext.Value{
			"cache_control": jsontext.Value(`{"type":"ephemeral","ttl":"` + ttl + `"}`),
		}
	case "ollama":
		if base == "" {
			base = OllamaBase
		}
	default:
		return nil, fmt.Errorf("responses: unknown kind %q (have openai, openrouter, ollama)", c.Kind)
	}
	if c.Kind != "ollama" {
		if c.APIKey == nil {
			return nil, fmt.Errorf("%s: no API key lookup", row)
		}
		key, err := c.APIKey()
		if err != nil {
			return nil, err
		}
		if strings.TrimSpace(key) == "" {
			return nil, fmt.Errorf("%s: the API key is empty", row)
		}
		cfg.Headers["Authorization"] = []string{"Bearer " + key}
	}
	cfg.Endpoint = base + "/responses"
	if c.MaxAttempts > 0 {
		n := c.MaxAttempts
		cfg.MaxAttempts = &n
	}
	a := &adapter{c: c}
	if fn := c.Options.Trace; fn != nil {
		cfg.Trace = func(ex responsesapi.Exchange) {
			fn(agentllm.Exchange{
				Provider: a.Provider(),
				Status:   ex.StatusCode,
				Request:  append([]byte(nil), ex.RequestBody...),
				Response: append([]byte(nil), ex.ResponseBody...),
			})
		}
	}
	client := c.HTTP
	if client == nil {
		client = &http.Client{}
	}
	tapped := *client
	base0 := client.Transport
	if base0 == nil {
		base0 = http.DefaultTransport
	}
	tapped.Transport = &tap{base: base0, sink: c.Options.Sink}
	a.remote = primitives.NewRemoteClientWithHTTPClient(&tapped)
	inner, err := responsesapi.NewAdapter(a.remote, cfg)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", row, err)
	}
	a.inner = inner
	return a, nil
}

// Provider is the envelope family. OpenRouter is keyed by the upstream
// vendor as well, because OpenRouter hands each vendor's reasoning back
// in that vendor's format: an anthropic/* signature means nothing to
// openai/*.
func (a *adapter) Provider() string {
	if a.c.Kind == "openrouter" {
		vendor, _, ok := strings.Cut(a.c.Model(), "/")
		if !ok {
			vendor = "unknown"
		}
		return "openrouter:" + vendor
	}
	return a.c.Kind
}

func (a *adapter) Model() string { return a.c.Model() }

func (a *adapter) Close() error { return a.remote.Close() }

// Respond takes model, effort and output cap from the row as it is now,
// so /model and /think apply to the next request without a rebuild.
func (a *adapter) Respond(ctx context.Context, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	r.Model.ID = a.c.Model()
	r.Model.ReasoningEffort = EffortFor(a.c.Effort())
	r.Model.MaxOutputTokens = nil
	if a.c.MaxTokens > 0 {
		n := a.c.MaxTokens
		r.Model.MaxOutputTokens = &n
	}
	ctx = context.WithValue(ctx, attemptKey{}, new(atomic.Int32))
	resp, err := a.inner.Respond(ctx, r, o)
	if err != nil {
		return resp, fmt.Errorf("llm-%s: %w", a.c.Kind, err)
	}
	return resp, nil
}

// EffortFor maps a bough level onto the Responses API. The harness enum
// has no "none", so off asks for the least reasoning there is; the row
// has already clamped max to what the model's catalogue entry accepts.
func EffortFor(level string) ullm.ReasoningEffort {
	switch level {
	case "":
		return ""
	case "off":
		return ullm.ReasoningEffortLow
	}
	if e := ullm.ReasoningEffort(level); e.Valid() {
		return e
	}
	return ""
}
