//go:build record

package responses

// Records the Responses fixtures under testdata/ from the real APIs:
//
//	BOUGH_LIVE=1 go test -tags record -run TestRecord ./internal/unreal/responses/
//
// Only response bodies and request bodies are written; the
// Authorization header never reaches a file.

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

type recorder struct {
	dir, prefix string
	n           int
}

func (r *recorder) RoundTrip(req *http.Request) (*http.Response, error) {
	var body []byte
	if req.Body != nil {
		body, _ = io.ReadAll(req.Body)
		req.Body = io.NopCloser(bytes.NewReader(body))
	}
	resp, err := http.DefaultTransport.RoundTrip(req)
	if err != nil {
		return resp, err
	}
	data, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	resp.Body = io.NopCloser(bytes.NewReader(data))
	r.n++
	_ = os.WriteFile(filepath.Join(r.dir, fmt.Sprintf("%s_%d.req.json", r.prefix, r.n)), body, 0o644)
	_ = os.WriteFile(filepath.Join(r.dir, fmt.Sprintf("%s_%d.sse", r.prefix, r.n)), data, 0o644)
	return resp, nil
}

func record(t *testing.T, kind, env, model, prefix string) {
	key := live(t, env)
	rec := &recorder{dir: "testdata", prefix: prefix}
	a, err := New(Config{
		Kind:   kind,
		APIKey: func() (string, error) { return key, nil },
		Model:  func() string { return model },
		Effort: func() string { return "low" },
		HTTP:   &http.Client{Transport: rec},
	})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	req := contractRequest()
	resp, err := a.Respond(agentllm.WithSeq(ctx, 1), req, ullm.RequestOptions{CacheKey: "bough-record"})
	if err != nil {
		t.Fatal(err)
	}
	req.Input = append(req.Input, resp.Output...)
	for _, it := range resp.Output {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			req.Input = append(req.Input, contractResult(c.CallID))
		}
	}
	if _, err := a.Respond(agentllm.WithSeq(ctx, 2), req, ullm.RequestOptions{CacheKey: "bough-record"}); err != nil {
		t.Fatal(err)
	}
}

// recordReasoning keeps one reply that reasons, for the tap's
// reasoning-summary golden.
func recordReasoning(t *testing.T, kind, env, model, prefix string) {
	key := live(t, env)
	rec := &recorder{dir: "testdata", prefix: prefix}
	a, err := New(Config{
		Kind:   kind,
		APIKey: func() (string, error) { return key, nil },
		Model:  func() string { return model },
		Effort: func() string { return "high" },
		HTTP:   &http.Client{Transport: rec},
	})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	req := ullm.Request{Input: []ullm.Item{
		{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "A bat and a ball cost 1.10 in total. The bat costs 1.00 more than the ball. How much does the ball cost? Reason it through, then answer in one short sentence."}},
	}}
	if _, err := a.Respond(agentllm.WithSeq(ctx, 1), req, ullm.RequestOptions{CacheKey: "bough-record"}); err != nil {
		t.Fatal(err)
	}
}

func TestRecordOpenAIReasoning(t *testing.T) {
	recordReasoning(t, "openai", "OPENAI_API_KEY", "gpt-5.6-sol", "openai_reason")
}

func TestRecordOpenRouterReasoning(t *testing.T) {
	recordReasoning(t, "openrouter", "OPENROUTER_API_KEY", "anthropic/claude-sonnet-5", "openrouter_reason")
}

func TestRecordOpenAI(t *testing.T) {
	record(t, "openai", "OPENAI_API_KEY", "gpt-5.6-sol", "openai")
}

func TestRecordOpenRouter(t *testing.T) {
	record(t, "openrouter", "OPENROUTER_API_KEY", "anthropic/claude-sonnet-5", "openrouter")
}
