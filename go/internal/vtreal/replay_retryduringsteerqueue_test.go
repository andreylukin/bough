package vtreal

// A steer sent while the provider is being retried: the first two model
// calls answer 429 and llm.withRetries backs off (1 s, then 3 s); the
// user presses enter on a steer during that wait. The steer must reach
// the model exactly once — in the retried request, or in the request
// the loop makes after landing it at the reply's boundary — never
// doubled inside a request or recorded twice, and the retry notice
// must not be left on the status bar once the call succeeded.
//
// Like replay_providerretryvisible_test.go this uses a fake OpenAI
// server: retries live in the provider's HTTP path, which the replay
// plugin skips.

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

const (
	retryDuringSteerQueueSteer = "STEERQX7 use tabs"
	retryDuringSteerQueueMark  = "STEERQX7"
	retryDuringSteerQueueWord  = "QUASAR19"
	retryDuringSteerQueueNote  = "provider hiccup"
)

// retryDuringSteerQueueServer fails the first len(fails) calls with
// those statuses (no "rate limited" wording, so the short 1 s / 3 s
// budget applies), then answers every call with a final reply. Every
// request body is kept.
func retryDuringSteerQueueServer(t *testing.T, fails ...int) (*httptest.Server, func() []string) {
	var mu sync.Mutex
	var bodies []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/responses" {
			http.NotFound(w, r)
			return
		}
		b, _ := io.ReadAll(r.Body)
		mu.Lock()
		i := len(bodies)
		bodies = append(bodies, string(b))
		mu.Unlock()
		if i < len(fails) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(fails[i])
			fmt.Fprint(w, `{"error":{"message":"transient hiccup"}}`)
			return
		}
		text := fmt.Sprintf("%s call %d.\n\n```stop\nDone.\n```", retryDuringSteerQueueWord, i)
		completed, _ := json.Marshal(map[string]any{
			"type": "response.completed",
			"response": map[string]any{
				"status": "completed",
				"output": []any{map[string]any{"type": "message",
					"content": []any{map[string]any{"type": "output_text", "text": text}}}},
				"usage": map[string]any{"input_tokens": costIn, "output_tokens": costOut},
			},
		})
		delta, _ := json.Marshal(map[string]any{"type": "response.output_text.delta", "delta": text})
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprintf(w, "data: %s\n\ndata: %s\n\ndata: [DONE]\n\n", delta, completed)
	}))
	t.Cleanup(srv.Close)
	return srv, func() []string {
		mu.Lock()
		defer mu.Unlock()
		return append([]string(nil), bodies...)
	}
}

// retryDuringSteerQueueBar is the status bar row (the one with "? keys").
func retryDuringSteerQueueBar(s string) string {
	bar := ""
	for _, l := range strings.Split(s, "\n") {
		if strings.Contains(l, "? keys") {
			bar = l
		}
	}
	return bar
}

func TestRetryDuringSteerQueue(t *testing.T) {
	t.Parallel()
	srv, bodies := retryDuringSteerQueueServer(t, http.StatusTooManyRequests, http.StatusTooManyRequests)
	home, _ := costHome(t)
	a := costStart(t, home, costConfig(srv.URL, ""))
	n := a.doneCount() + 1

	a.typeText("hello")
	a.key(uv.KeyEnter, 0)
	a.waitFor(retryDuringSteerQueueNote) // backing off: the steer goes in now
	a.typeText(retryDuringSteerQueueSteer)
	a.key(uv.KeyEnter, 0)
	if !a.waitDone(n, 30*time.Second) {
		t.Fatalf("turn never finished:\n%s", a.text())
	}
	a.waitFor(retryDuringSteerQueueWord)
	s := a.settled()

	bs := bodies()
	if len(bs) < 3 {
		t.Fatalf("provider called %d times, want at least 3 (429, 429, success)", len(bs))
	}
	t.Logf("provider calls: %d", len(bs))
	carried := 0
	for i, b := range bs {
		switch c := strings.Count(b, retryDuringSteerQueueMark); {
		case c > 1:
			t.Errorf("request %d carries the steer %d times, want at most 1:\n%s", i, c, b)
		case c == 1 && i >= 2:
			carried++
		}
	}
	if carried == 0 {
		t.Errorf("no request after the steer carried it (%d calls)", len(bs))
	}
	// 3 calls: the retried request carried the steer. 4: it landed at
	// the reply's boundary and the model was asked once more.
	if len(bs) > 4 {
		t.Errorf("provider called %d times, want 3 or 4", len(bs))
	}
	ins := providerRetryVisibleEntries(t, home, "input")
	steers := 0
	for _, e := range ins {
		if e.Data["steer"] == true {
			steers++
		}
	}
	if len(ins) != 2 || steers != 1 {
		t.Errorf("history has %d input entries (%d steers), want 2 (1 steer): %v", len(ins), steers, ins)
	}
	if c := strings.Count(s, retryDuringSteerQueueMark); c != 1 {
		t.Errorf("steer shown %d times, want 1:\n%s", c, s)
	}
	if strings.Contains(s, "pending") {
		t.Errorf("steer still pending after the turn ended:\n%s", s)
	}
	if strings.Contains(s, "✗") || strings.Contains(s, "HTTP 429") {
		t.Errorf("a retried 429 surfaced as an error:\n%s", s)
	}
	if strings.Contains(retryDuringSteerQueueBar(s), retryDuringSteerQueueNote) {
		t.Errorf("status bar still shows the retry notice after success:\n%s", s)
	}
	a.check("steer during retry")
}
