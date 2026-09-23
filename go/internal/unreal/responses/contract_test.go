//go:build !windows

package responses

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/fake"
	"github.com/andreylukin/bough/internal/unreal/wrap"
)

// The contract tape: the user asks, the model calls read_secret, the
// result comes back, the model answers with it. The fixtures under
// testdata/ are that tape played against the real OpenAI and
// OpenRouter APIs by record_test.go.
const contractSecret = "orchid-7319"

func contractRequest() ullm.Request {
	return ullm.Request{
		Input: []ullm.Item{
			{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: "You are a test harness. Call read_secret exactly once to answer the user. Once you receive its result, reply with the secret and no other text."}},
			{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "What is the secret?"}},
		},
		Tools: []ullm.Tool{{Type: ullm.ToolFunction, Name: "read_secret", Description: "Read the secret.", Parameters: map[string]any{"type": "object", "properties": map[string]any{}}}},
	}
}

func contractResult(callID string) ullm.Item {
	return ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: callID, Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: contractSecret}}}}
}

// recording wraps an adapter and keeps what it was asked, for
// fake.AssertAppendOnly on a real adapter.
type recording struct {
	agentllm.Adapter
	mu   sync.Mutex
	reqs []fake.Recorded
}

func (r *recording) Respond(ctx context.Context, req ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	r.mu.Lock()
	r.reqs = append(r.reqs, fake.Recorded{Request: req, Options: o})
	r.mu.Unlock()
	return r.Adapter.Respond(ctx, req, o)
}

// drive plays the tape the way the coordinator would: ask, append the
// output, answer every call, ask again.
func drive(t *testing.T, a agentllm.Adapter) []ullm.Response {
	t.Helper()
	ctx := t.Context()
	req := contractRequest()
	var out []ullm.Response
	for seq := uint64(1); seq <= 2; seq++ {
		resp, err := a.Respond(agentllm.WithSeq(ctx, seq), req, ullm.RequestOptions{CacheKey: "bough-record"})
		if err != nil {
			t.Fatalf("request %d: %v", seq, err)
		}
		out = append(out, resp)
		req.Input = append(req.Input, resp.Output...)
		for _, it := range resp.Output {
			if c, ok := it.Data.(ullm.ToolCall); ok {
				req.Input = append(req.Input, contractResult(c.CallID))
			}
		}
	}
	return out
}

// shape is the provider-neutral content of a response: what the
// harness acts on, without ids, reasoning bytes or usage.
func shape(r ullm.Response) []string {
	var s []string
	for _, it := range r.Output {
		switch d := it.Data.(type) {
		case ullm.ToolCall:
			s = append(s, "call "+d.Name+" "+d.Arguments)
		case ullm.Message:
			s = append(s, "text "+strings.TrimSpace(d.Text))
		}
	}
	return s
}

// replay serves recorded SSE bodies in order and keeps the request
// bodies it was sent.
type replay struct {
	t      *testing.T
	files  []string
	mu     sync.Mutex
	n      int
	bodies [][]byte
}

func (rp *replay) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, _ := io.ReadAll(r.Body)
	rp.mu.Lock()
	i := rp.n
	rp.n++
	rp.bodies = append(rp.bodies, body)
	rp.mu.Unlock()
	if i >= len(rp.files) {
		rp.t.Errorf("replay: request %d beyond the %d recorded", i+1, len(rp.files))
		http.Error(w, "no more fixtures", http.StatusInternalServerError)
		return
	}
	data, err := os.ReadFile(rp.files[i])
	if err != nil {
		rp.t.Errorf("replay: %v", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Write(data)
}

// The same tape through the scripted fake and through a recorded
// OpenAI Responses session gives the same harness-level conversation,
// both request sequences are append-only, and the second request bough
// renders from the recording is byte for byte the one the real API was
// sent when it was recorded.
func TestContractFakeMatchesRecordedOpenAI(t *testing.T) {
	t.Parallel()
	rp := &replay{t: t, files: []string{filepath.Join("testdata", "openai_1.sse"), filepath.Join("testdata", "openai_2.sse")}}
	srv := httptest.NewServer(rp)
	defer srv.Close()
	inner, err := New(Config{
		Kind:    "openai",
		APIKey:  func() (string, error) { return "test-key", nil },
		BaseURL: srv.URL,
		Model:   func() string { return "gpt-5.6-sol" },
		Effort:  func() string { return "low" },
		HTTP:    srv.Client(),
	})
	if err != nil {
		t.Fatal(err)
	}
	real := &recording{Adapter: wrap.Envelope(inner)}
	got := drive(t, real)

	callID := ""
	for _, it := range got[0].Output {
		if c, ok := it.Data.(ullm.ToolCall); ok {
			callID = c.CallID
		}
	}
	tape := fake.New(t,
		fake.Step{Want: "What is the secret?", Output: []ullm.Item{fake.Call(callID, "read_secret", "{}")}},
		fake.Step{Want: contractSecret, Output: []ullm.Item{fake.Text(contractSecret)}},
	)
	want := drive(t, wrap.Envelope(tape))

	for i := range want {
		if g, w := strings.Join(shape(got[i]), "|"), strings.Join(shape(want[i]), "|"); g != w {
			t.Errorf("response %d: recorded OpenAI %q, fake %q", i+1, g, w)
		}
	}
	fake.AssertAppendOnly(t, tape.Requests())
	fake.AssertAppendOnly(t, real.reqs)

	for i, name := range []string{"openai_1.req.json", "openai_2.req.json"} {
		recorded, err := os.ReadFile(filepath.Join("testdata", name))
		if err != nil {
			t.Fatal(err)
		}
		if string(rp.bodies[i]) != string(recorded) {
			t.Errorf("request %d differs from the one recorded against the real API\n got: %s\nwant: %s", i+1, rp.bodies[i], recorded)
		}
	}
}
