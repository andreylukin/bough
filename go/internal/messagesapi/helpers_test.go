package messagesapi

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"flag"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/agentllm"
)

var update = flag.Bool("update", false, "rewrite the goldens")

func golden(t *testing.T, path string, got []byte) {
	t.Helper()
	if *update {
		if err := os.WriteFile(path, got, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run with -update to create it)", err)
	}
	if string(want) != string(got) {
		t.Errorf("%s differs:\n got: %s\nwant: %s", path, got, want)
	}
}

// pretty re-indents any JSON with sorted keys, so goldens diff well.
func pretty(t *testing.T, b []byte) []byte {
	t.Helper()
	var v any
	if err := json.Unmarshal(b, &v); err != nil {
		t.Fatalf("pretty: %v\n%s", err, b)
	}
	out, err := json.Marshal(v, json.Deterministic(true), jsontext.WithIndent("  "))
	if err != nil {
		t.Fatal(err)
	}
	return append(out, '\n')
}

// reply is one scripted HTTP answer: an SSE fixture, or a status with
// a body and headers.
type reply struct {
	sse     string // file under testdata/sse
	status  int
	body    string
	headers map[string]string
	hang    time.Duration // write the fixture's first half, then go silent this long
}

// script serves replies in order and keeps every request body.
type script struct {
	t       *testing.T
	mu      sync.Mutex
	replies []reply
	n       int
	bodies  []string
	betas   []string
}

func (s *script) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	b, _ := io.ReadAll(r.Body)
	s.mu.Lock()
	i := s.n
	s.n++
	s.bodies = append(s.bodies, string(b))
	s.betas = append(s.betas, strings.Join(r.Header.Values("anthropic-beta"), ","))
	s.mu.Unlock()
	if i >= len(s.replies) {
		s.t.Errorf("script: request %d beyond the %d scripted", i+1, len(s.replies))
		http.Error(w, "unscripted", http.StatusTeapot)
		return
	}
	rp := s.replies[i]
	for k, v := range rp.headers {
		w.Header().Set(k, v)
	}
	if rp.sse == "" {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(rp.status)
		io.WriteString(w, rp.body)
		return
	}
	data, err := os.ReadFile(filepath.Join("testdata", "sse", rp.sse))
	if err != nil {
		s.t.Errorf("script: %v", err)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	if rp.hang > 0 {
		w.Write(data[:len(data)/2])
		w.(http.Flusher).Flush()
		select {
		case <-time.After(rp.hang):
		case <-r.Context().Done():
		}
		return
	}
	w.Write(data)
}

func (s *script) requests() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.n
}

// testAdapter is an adapter against a local server, with its clock
// replaced: waits are recorded, not slept.
type testAdapter struct {
	*adapter
	srv    *httptest.Server
	s      *script
	mu     sync.Mutex
	waits  []time.Duration
	deltas []agentllm.Delta
}

func newTestAdapter(t *testing.T, c Config, replies ...reply) *testAdapter {
	t.Helper()
	s := &script{t: t, replies: replies}
	srv := httptest.NewServer(s)
	t.Cleanup(srv.Close)
	ta := &testAdapter{srv: srv, s: s}
	c.Client = anthropic.NewClient(option.WithAPIKey("test-key"), option.WithBaseURL(srv.URL), option.WithMaxRetries(0), option.WithHTTPClient(srv.Client()))
	if c.Model == nil {
		c.Model = func() string { return "claude-sonnet-5" }
	}
	c.Options.Sink = func(d agentllm.Delta) {
		ta.mu.Lock()
		ta.deltas = append(ta.deltas, d)
		ta.mu.Unlock()
	}
	a, err := New(c)
	if err != nil {
		t.Fatal(err)
	}
	ta.adapter = a.(*adapter)
	ta.adapter.dropped = &sync.Map{}
	ta.adapter.now = func() time.Time { return time.Date(2026, 9, 22, 12, 0, 0, 0, time.UTC) }
	ta.adapter.wait = func(ctx context.Context, d time.Duration) error {
		ta.mu.Lock()
		ta.waits = append(ta.waits, d)
		ta.mu.Unlock()
		return ctx.Err()
	}
	return ta
}

func userText(s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleUser, Text: s}}
}

func system(s string) ullm.Item {
	return ullm.Item{Type: ullm.ItemMessage, Data: ullm.Message{Role: ullm.RoleSystem, Text: s}}
}

func simpleRequest() ullm.Request {
	return ullm.Request{Input: []ullm.Item{system("be brief"), userText("hi")}}
}
