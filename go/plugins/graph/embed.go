package graph

// Embeddings are an accelerator for Search, computed at write time over
// entity titles and edge claims. The embedder is any OpenAI-compatible
// /embeddings endpoint; without a key the graph is FTS-only and every
// verb still works.

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"time"
)

// Embedder turns text into a vector.
type Embedder interface {
	Embed(ctx context.Context, text string) ([]float32, error)
	Model() string
}

// HTTPEmbedder calls an OpenAI-compatible /embeddings endpoint.
type HTTPEmbedder struct {
	BaseURL string
	Key     string
	Name    string // model id in the provider's naming
	Client  *http.Client
}

// EmbedderFromEnv picks the endpoint from the keys in the process
// environment (~/.bough/env is loaded at boot): OpenRouter first, since
// it proxies the OpenAI embedding models on the one key bough already
// uses, then OpenAI. nil when neither key is set.
func EmbedderFromEnv() Embedder {
	if k := os.Getenv("OPENROUTER_API_KEY"); k != "" {
		return &HTTPEmbedder{BaseURL: "https://openrouter.ai/api/v1", Key: k, Name: "openai/text-embedding-3-small"}
	}
	if k := os.Getenv("OPENAI_API_KEY"); k != "" {
		return &HTTPEmbedder{BaseURL: "https://api.openai.com/v1", Key: k, Name: "text-embedding-3-small"}
	}
	return nil
}

func (h *HTTPEmbedder) Model() string { return h.Name }

func (h *HTTPEmbedder) Embed(ctx context.Context, text string) ([]float32, error) {
	if len(text) > 8000 {
		text = text[:8000]
	}
	body, _ := json.Marshal(map[string]any{"model": h.Name, "input": text})
	req, err := http.NewRequestWithContext(ctx, "POST", h.BaseURL+"/embeddings", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+h.Key)
	req.Header.Set("Content-Type", "application/json")
	c := h.Client
	if c == nil {
		c = &http.Client{Timeout: 20 * time.Second}
	}
	resp, err := c.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 4<<20))
	if err != nil {
		return nil, err
	}
	if resp.StatusCode != 200 {
		return nil, fmt.Errorf("embeddings: http %d: %s", resp.StatusCode, truncate(string(data), 200))
	}
	var parsed struct {
		Data []struct {
			Embedding []float32 `json:"embedding"`
		} `json:"data"`
	}
	if err := json.Unmarshal(data, &parsed); err != nil {
		return nil, err
	}
	if len(parsed.Data) == 0 {
		return nil, fmt.Errorf("embeddings: empty response")
	}
	return parsed.Data[0].Embedding, nil
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n] + "…"
}
