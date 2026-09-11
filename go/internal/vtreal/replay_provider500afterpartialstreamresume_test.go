package vtreal

// A provider that streams half a reply and then fails with a 500 the
// retry budget cannot save (deltas were delivered, so the call is not
// retried). The partial text must stay on screen marked as an error,
// the history log must record the partial text with the error rather
// than a completed assistant entry, and after quit + resume the
// transcript must match and the next request must not carry the half
// reply as a plain assistant message with no error note.
//
// The replay llm cannot fail mid-stream, and the request it saw lives
// in the child process, so a fake OpenAI Responses server stands in:
// it records every request body for the test to inspect.

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"

	"github.com/andreylukin/bough/plugins/history"
)

const (
	provider500AfterPartialStreamResumeHalf  = "PARTIALALPHA streamed before the provider died"
	provider500AfterPartialStreamResumeErr   = "HTTP 500: upstream exploded"
	provider500AfterPartialStreamResumeFixed = "RECOVEREDOMEGA"
	provider500AfterPartialStreamResumeGate  = "BOUGH_KNOWN_PROVIDER500AFTERPARTIALSTREAMRESUME"
)

// provider500AfterPartialStreamResumeServer fails the first call after
// its deltas and answers every later call cleanly; bodies holds every
// request body in order.
type provider500AfterPartialStreamResumeServer struct {
	*httptest.Server
	mu     sync.Mutex
	bodies []map[string]any
}

func provider500AfterPartialStreamResumeStart(t *testing.T) *provider500AfterPartialStreamResumeServer {
	s := &provider500AfterPartialStreamResumeServer{}
	s.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/responses" {
			http.NotFound(w, r)
			return
		}
		raw, _ := io.ReadAll(r.Body)
		var body map[string]any
		_ = json.Unmarshal(raw, &body)
		s.mu.Lock()
		s.bodies = append(s.bodies, body)
		n := len(s.bodies)
		s.mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		fl, _ := w.(http.Flusher)
		ev := func(v map[string]any) {
			j, _ := json.Marshal(v)
			fmt.Fprintf(w, "data: %s\n\n", j)
			if fl != nil {
				fl.Flush()
			}
		}
		if n == 1 {
			for _, word := range strings.SplitAfter(provider500AfterPartialStreamResumeHalf, " ") {
				ev(map[string]any{"type": "response.output_text.delta", "delta": word})
				time.Sleep(20 * time.Millisecond)
			}
			ev(map[string]any{"type": "error", "code": "500", "error": map[string]any{"message": provider500AfterPartialStreamResumeErr}})
			return
		}
		text := provider500AfterPartialStreamResumeFixed + " done.\n\n```stop\nDone.\n```"
		ev(map[string]any{"type": "response.output_text.delta", "delta": text})
		ev(map[string]any{"type": "response.completed", "response": map[string]any{
			"status": "completed",
			"output": []any{map[string]any{"type": "message",
				"content": []any{map[string]any{"type": "output_text", "text": text}}}},
			"usage": map[string]any{"input_tokens": 10, "output_tokens": 5},
		}})
		fmt.Fprint(w, "data: [DONE]\n\n")
	}))
	t.Cleanup(s.Close)
	return s
}

func (s *provider500AfterPartialStreamResumeServer) requests() []map[string]any {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]map[string]any(nil), s.bodies...)
}

// provider500AfterPartialStreamResumeInput is the request's input
// messages as role + flattened text.
func provider500AfterPartialStreamResumeInput(body map[string]any) [][2]string {
	var out [][2]string
	items, _ := body["input"].([]any)
	for _, it := range items {
		m, _ := it.(map[string]any)
		role, _ := m["role"].(string)
		var text string
		switch c := m["content"].(type) {
		case string:
			text = c
		case []any:
			for _, p := range c {
				pm, _ := p.(map[string]any)
				if s, ok := pm["text"].(string); ok {
					text += s
				}
			}
		}
		out = append(out, [2]string{role, text})
	}
	return out
}

// provider500AfterPartialStreamResumeQuit presses ctrl+c twice and
// waits for the process to exit.
func provider500AfterPartialStreamResumeQuit(t *testing.T, a *app) {
	t.Helper()
	a.key('c', uv.ModCtrl)
	a.waitFor("ctrl+c")
	a.key('c', uv.ModCtrl)
	done := make(chan error, 1)
	go func() { done <- a.term.Wait(a.cmd) }()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatalf("bough did not exit after two ctrl+c:\n%s", a.text())
	}
}

func provider500AfterPartialStreamResumeKnown(t *testing.T, bug string) {
	if os.Getenv(provider500AfterPartialStreamResumeGate) == "" {
		t.Skip("known bug: " + bug + "; set " + provider500AfterPartialStreamResumeGate + "=1 to run")
	}
}

func TestProvider500AfterPartialStreamResume(t *testing.T) {
	t.Parallel()
	srv := provider500AfterPartialStreamResumeStart(t)
	home, _ := costHome(t)
	log := filepath.Join(home, "session.jsonl")
	yml := costConfig(srv.URL, log)

	a := costStart(t, home, yml)
	a.typeText("tell me a story")
	a.key(uv.KeyEnter, 0)
	a.waitFor("upstream exploded")
	deadline := time.Now().Add(30 * time.Second)
	for resumeDones(log) < 1 && time.Now().Before(deadline) {
		time.Sleep(50 * time.Millisecond)
	}
	if resumeDones(log) < 1 {
		t.Fatalf("failed turn never recorded done:\n%s", a.text())
	}
	a.check("failed turn")
	pre := a.settled()
	before := resumeTranscript(a)
	if n := len(srv.requests()); n != 1 {
		t.Errorf("provider called %d times, want 1 (deltas were delivered: no retry)", n)
	}

	t.Run("partial_visible_marked_error", func(t *testing.T) {
		// The partial reply is dropped on error by design (8b9984ad): history
		// cannot redraw it on resume. Only the error marker must show.
		if !strings.Contains(pre, "✗") || !strings.Contains(pre, "upstream exploded") {
			t.Errorf("no error marker for the failed stream:\n%s", pre)
		}
	})

	entries, err := history.Read(log)
	if err != nil {
		t.Fatal(err)
	}
	t.Run("no_completed_assistant_entry", func(t *testing.T) {
		for _, e := range entries {
			if e.Kind == "assistant" {
				t.Errorf("a failed stream recorded a completed assistant entry: %v", e.Data)
			}
		}
	})
	t.Run("history_records_partial_and_error", func(t *testing.T) {
		sawErr, sawPartial := false, false
		for _, e := range entries {
			text, _ := e.Data["text"].(string)
			if e.Kind == "error" && strings.Contains(text, "upstream exploded") {
				sawErr = true
			}
			if strings.Contains(text, "PARTIALALPHA") {
				sawPartial = true
			}
		}
		if !sawErr {
			t.Errorf("history has no error entry for the 500: %s", crashResumeIntegrityKinds(entries))
		}
		if !sawPartial {
			provider500AfterPartialStreamResumeKnown(t, "loop.Run drops the streamed partial reply on a model error (only the error entry is written)")
			t.Errorf("history lost the partial text: %s", crashResumeIntegrityKinds(entries))
		}
	})

	provider500AfterPartialStreamResumeQuit(t, a)

	b := costStart(t, home, yml)
	b.waitFor("resumed ")
	b.check("resumed boot")

	t.Run("resumed_transcript_identical", func(t *testing.T) {
		after := resumeTranscript(b)
		if strings.Join(after, "\n") != strings.Join(before, "\n") {
			provider500AfterPartialStreamResumeKnown(t, "the partial reply is not in history, so a resume cannot redraw it")
			t.Errorf("resumed transcript differs.\npre-quit:\n%s\n\nresumed:\n%s", strings.Join(before, "\n"), strings.Join(after, "\n"))
		}
	})

	b.typeText("try again")
	b.key(uv.KeyEnter, 0)
	b.waitFor(provider500AfterPartialStreamResumeFixed)
	b.check("follow-up turn")

	t.Run("next_request_has_no_dangling_half_reply", func(t *testing.T) {
		reqs := srv.requests()
		if len(reqs) != 2 {
			t.Fatalf("provider saw %d requests, want 2", len(reqs))
		}
		msgs := provider500AfterPartialStreamResumeInput(reqs[1])
		if len(msgs) == 0 || msgs[len(msgs)-1][0] != "user" || !strings.Contains(msgs[len(msgs)-1][1], "try again") {
			t.Errorf("last message is not the new prompt: %q", msgs)
		}
		sawPrompt := false
		for _, m := range msgs {
			if strings.Contains(m[1], "tell me a story") {
				sawPrompt = true
			}
			if m[0] == "assistant" && strings.Contains(m[1], "PARTIALALPHA") && !strings.Contains(m[1], "upstream exploded") {
				t.Errorf("request carries the half reply as a plain assistant message: %q", m[1])
			}
		}
		if !sawPrompt {
			t.Errorf("resumed request lost the failed turn's prompt: %q", msgs)
		}
	})
}
