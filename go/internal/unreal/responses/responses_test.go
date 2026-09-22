package responses

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	ullm "github.com/unreallabsai/unreal-agent/harness/llm"
	hollama "github.com/unreallabsai/unreal-agent/harness/llm/clients/ollama"
	hopenai "github.com/unreallabsai/unreal-agent/harness/llm/clients/openai"
	hopenrouter "github.com/unreallabsai/unreal-agent/harness/llm/clients/openrouter"

	"github.com/andreylukin/bough/internal/agentllm"
)

var update = flag.Bool("update", false, "rewrite the tap goldens from the recorded fixtures")

// deltaLog renders a delta stream compactly: consecutive fragments of
// one kind are joined, so the golden reads as what the UI would show.
func deltaLog(ds []agentllm.Delta) string {
	var b strings.Builder
	var kind agentllm.DeltaKind
	for _, d := range ds {
		switch d.Kind {
		case agentllm.DeltaToolStart:
			fmt.Fprintf(&b, "\n[tool_start %s %s]", d.Name, d.CallID)
			kind = ""
		default:
			if d.Kind != kind {
				fmt.Fprintf(&b, "\n[%s] ", d.Kind)
				kind = d.Kind
			}
			b.WriteString(d.Text)
		}
	}
	return strings.TrimPrefix(b.String(), "\n") + "\n"
}

// The tap turns the recorded streams of both providers into the deltas
// the UI shows, stamped with the request's Seq, and the text it streams
// is exactly the text of the final message.
func TestTapGoldens(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ fixture, kind, model string }{
		{"openai_1", "openai", "gpt-5.6-sol"},
		{"openai_2", "openai", "gpt-5.6-sol"},
		{"openai_reason_1", "openai", "gpt-5.6-sol"},
		{"openrouter_1", "openrouter", "anthropic/claude-sonnet-5"},
		{"openrouter_reason_1", "openrouter", "anthropic/claude-sonnet-5"},
	} {
		t.Run(tc.fixture, func(t *testing.T) {
			t.Parallel()
			rp := &replay{t: t, files: []string{filepath.Join("testdata", tc.fixture+".sse")}}
			srv := httptest.NewServer(rp)
			defer srv.Close()
			var mu sync.Mutex
			var ds []agentllm.Delta
			a, err := New(Config{
				Kind: tc.kind, APIKey: func() (string, error) { return "k", nil }, BaseURL: srv.URL,
				Model: func() string { return tc.model }, HTTP: srv.Client(),
				Options: agentllm.Options{Sink: func(d agentllm.Delta) { mu.Lock(); ds = append(ds, d); mu.Unlock() }},
			})
			if err != nil {
				t.Fatal(err)
			}
			req := ullm.Request{Input: []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "x"}}}}
			resp, err := a.Respond(agentllm.WithSeq(t.Context(), 7), req, ullm.RequestOptions{})
			if err != nil {
				t.Fatal(err)
			}
			var streamed, final strings.Builder
			for _, d := range ds {
				if d.Seq != 7 || d.Attempt != 1 {
					t.Errorf("delta %+v: want Seq 7, Attempt 1", d)
				}
				if d.Kind == agentllm.DeltaText {
					streamed.WriteString(d.Text)
				}
			}
			for _, it := range resp.Output {
				if m, ok := it.Data.(ullm.Message); ok {
					final.WriteString(m.Text)
				}
			}
			if streamed.String() != final.String() {
				t.Errorf("streamed text %q, final text %q", streamed.String(), final.String())
			}
			golden(t, filepath.Join("testdata", tc.fixture+".deltas"), deltaLog(ds))
		})
	}
}

func golden(t *testing.T, path, got string) {
	t.Helper()
	if *update {
		if err := os.WriteFile(path, []byte(got), 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run with -update to create it)", err)
	}
	if string(want) != got {
		t.Errorf("%s differs:\n got: %s\nwant: %s", path, got, want)
	}
}

// Every round trip is an attempt, a failed one included: the stream
// after a retried 500 carries Attempt 2, so a consumer drops the text
// of a stream that died instead of showing it twice.
func TestTapAttemptCountsRetries(t *testing.T) {
	t.Parallel()
	sse, err := os.ReadFile(filepath.Join("testdata", "openai_2.sse"))
	if err != nil {
		t.Fatal(err)
	}
	var n int
	var mu sync.Mutex
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		io.Copy(io.Discard, r.Body)
		mu.Lock()
		n++
		first := n == 1
		mu.Unlock()
		if first {
			w.Header().Set("Retry-After", "1")
			http.Error(w, `{"error":{"message":"upstream hiccup","type":"server_error"}}`, http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		w.Write(sse)
	}))
	defer srv.Close()
	var ds []agentllm.Delta
	a, err := New(Config{
		Kind: "openai", APIKey: func() (string, error) { return "k", nil }, BaseURL: srv.URL,
		Model: func() string { return "gpt-5.6-sol" }, HTTP: srv.Client(), MaxAttempts: 2,
		Options: agentllm.Options{Sink: func(d agentllm.Delta) { ds = append(ds, d) }},
	})
	if err != nil {
		t.Fatal(err)
	}
	req := ullm.Request{Input: []ullm.Item{{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: "x"}}}}
	if _, err := a.Respond(agentllm.WithSeq(t.Context(), 3), req, ullm.RequestOptions{}); err != nil {
		t.Fatal(err)
	}
	if len(ds) == 0 {
		t.Fatal("no deltas")
	}
	for _, d := range ds {
		if d.Attempt != 2 || d.Seq != 3 {
			t.Errorf("delta %+v: want Attempt 2, Seq 3", d)
		}
	}
}

// capture serves a minimal completed response and keeps each request.
type capture struct {
	mu      sync.Mutex
	bodies  []string
	headers []http.Header
}

func (c *capture) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	b, _ := io.ReadAll(r.Body)
	c.mu.Lock()
	c.bodies = append(c.bodies, string(b))
	c.headers = append(c.headers, r.Header.Clone())
	c.mu.Unlock()
	w.Header().Set("Content-Type", "text/event-stream")
	io.WriteString(w, "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[]}}\n\n")
}

func parityRequest() ullm.Request {
	r := contractRequest()
	r.Model = ullm.Model{ID: "m-1", ReasoningEffort: ullm.ReasoningEffortHigh}
	r.Input = append(r.Input,
		ullm.Item{Type: ullm.ItemToolCall, Data: ullm.ToolCall{CallID: "c1", Name: "read_secret", Arguments: `{"b":1,"a":2}`}},
		ullm.Item{Type: ullm.ItemToolResult, Data: ullm.ToolResult{CallID: "c1", Output: []ullm.ToolResultOutput{{Kind: ullm.ToolResultText, Value: "ok"}}}},
	)
	return r
}

// bough builds the Responses adapter itself (to own the http.Client), so
// it must send exactly what the harness's own clients send at the pin:
// the same body bytes, the same auth and cache-key placement.
func TestRequestBodyMatchesHarnessClients(t *testing.T) {
	t.Parallel()
	for _, kind := range []string{"openai", "openrouter", "ollama"} {
		t.Run(kind, func(t *testing.T) {
			t.Parallel()
			theirs, ours := &capture{}, &capture{}
			s1, s2 := httptest.NewServer(theirs), httptest.NewServer(ours)
			defer s1.Close()
			defer s2.Close()
			one := 1
			var h ullm.Adapter
			switch kind {
			case "openai":
				c, err := hopenai.NewClient(hopenai.Config{APIKey: "k", BaseURL: s1.URL, MaxAttempts: &one})
				if err != nil {
					t.Fatal(err)
				}
				defer c.Close()
				h = c
			case "openrouter":
				c, err := hopenrouter.NewClient(hopenrouter.Config{APIKey: "k", BaseURL: s1.URL, MaxAttempts: &one})
				if err != nil {
					t.Fatal(err)
				}
				defer c.Close()
				h = c
			case "ollama":
				c, err := hollama.NewClient(hollama.Config{BaseURL: s1.URL, MaxAttempts: &one})
				if err != nil {
					t.Fatal(err)
				}
				defer c.Close()
				h = c
			}
			a, err := New(Config{
				Kind: kind, APIKey: func() (string, error) { return "k", nil }, BaseURL: s2.URL,
				Model: func() string { return "m-1" }, Effort: func() string { return "high" },
				HTTP: s2.Client(), MaxAttempts: 1,
			})
			if err != nil {
				t.Fatal(err)
			}
			defer a.Close()
			opts := ullm.RequestOptions{CacheKey: "session-1"}
			if _, err := h.Respond(t.Context(), parityRequest(), opts); err != nil {
				t.Fatal(err)
			}
			if _, err := a.Respond(t.Context(), parityRequest(), opts); err != nil {
				t.Fatal(err)
			}
			if theirs.bodies[0] != ours.bodies[0] {
				t.Errorf("body differs from the harness client\nharness: %s\n  bough: %s", theirs.bodies[0], ours.bodies[0])
			}
			for _, name := range []string{"Authorization", "Content-Type", "Accept", "X-Session-Id"} {
				if g, w := ours.headers[0].Get(name), theirs.headers[0].Get(name); g != w {
					t.Errorf("header %s = %q, the harness client sends %q", name, g, w)
				}
			}
		})
	}
}

func TestProviderNamesTheEnvelopeFamily(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ kind, model, want string }{
		{"openai", "gpt-5.6-sol", "openai"},
		{"openrouter", "anthropic/claude-sonnet-5", "openrouter:anthropic"},
		{"openrouter", "openai/gpt-5.6-sol", "openrouter:openai"},
		{"ollama", "qwen3:8b", "ollama"},
	} {
		a, err := New(Config{Kind: tc.kind, APIKey: func() (string, error) { return "k", nil }, Model: func() string { return tc.model }})
		if err != nil {
			t.Fatal(err)
		}
		if got := a.Provider(); got != tc.want {
			t.Errorf("%s %s: Provider() = %q, want %q", tc.kind, tc.model, got, tc.want)
		}
		if a.Model() != tc.model {
			t.Errorf("Model() = %q, want %q", a.Model(), tc.model)
		}
	}
}

func TestNewRefusesBadConfig(t *testing.T) {
	t.Parallel()
	model := func() string { return "m" }
	if _, err := New(Config{Kind: "cohere", Model: model}); err == nil {
		t.Error("an unknown kind should be refused")
	}
	if _, err := New(Config{Kind: "openrouter", CacheTTL: "2h", APIKey: func() (string, error) { return "k", nil }, Model: model}); err == nil || !strings.Contains(err.Error(), "llm-openrouter: cache_ttl") {
		t.Errorf("a bad cache_ttl should name the row and key, got %v", err)
	}
	missing := errors.New("llm-openai: OPENAI_API_KEY is not set")
	if _, err := New(Config{Kind: "openai", APIKey: func() (string, error) { return "", missing }, Model: model}); !errors.Is(err, missing) {
		t.Errorf("the key lookup's error should come back as is, got %v", err)
	}
}

func TestEffortFor(t *testing.T) {
	t.Parallel()
	for level, want := range map[string]ullm.ReasoningEffort{
		"": "", "off": "low", "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh", "max": "max", "bogus": "",
	} {
		if got := EffortFor(level); got != want {
			t.Errorf("EffortFor(%q) = %q, want %q", level, got, want)
		}
	}
}

// Frames split across reads and CRLF delimiters still produce deltas.
func TestTeeSplitsFramesAcrossReads(t *testing.T) {
	t.Parallel()
	stream := "data: {\"type\":\"response.output_text.delta\",\"delta\":\"he\"}\r\n\r\n" +
		"event: x\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"llo\"}\n\n" +
		"data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"c9\",\"name\":\"bash\"}}\n\n" +
		"data: [DONE]\n\n"
	var ds []agentllm.Delta
	tr := &tee{ReadCloser: io.NopCloser(&slowReader{s: stream}), emit: func(d agentllm.Delta) { ds = append(ds, d) }}
	if _, err := io.ReadAll(tr); err != nil {
		t.Fatal(err)
	}
	if got := deltaLog(ds); got != "[text] hello\n[tool_start bash c9]\n" {
		t.Errorf("deltas = %q", got)
	}
}

type slowReader struct {
	s string
	i int
}

func (r *slowReader) Read(p []byte) (int, error) {
	if r.i >= len(r.s) {
		return 0, io.EOF
	}
	n := min(3, len(p), len(r.s)-r.i)
	copy(p, r.s[r.i:r.i+n])
	r.i += n
	return n, nil
}
