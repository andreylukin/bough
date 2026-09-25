//go:build !windows

package llm

// llm-control's "api" turns: the request goes through the real Messages
// API adapter (internal/messagesapi), whose HTTP client is served here,
// in process, from the control dir. So a test drives a turn through the
// adapter's own retry loop, its delta-reset and its stop reasons (a 529
// retried, retries running out, a 400 that is a context overflow, a
// refusal, a reply cut at max_tokens) the way a provider would, one HTTP
// attempt at a time, and nothing leaves the process.
//
// The files of an api turn named N, beside N.json:
//
//	N.attempt-<k>  written by the row when attempt k reaches the "server"
//	N.stream       a fragment the held attempt streams (as for block)
//	N.answer       how the held attempt ends (controlAnswer); removed
//	               when taken, so the next attempt waits for its own
//	N.waiting      written while the adapter waits to retry
//	N.retry        lets that wait end; removed when taken

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	anthropic "github.com/anthropics/anthropic-sdk-go"
	"github.com/anthropics/anthropic-sdk-go/option"
	ullm "github.com/unreallabsai/unreal-agent/harness/llm"

	"github.com/andreylukin/bough/internal/messagesapi"
)

// controlAPIAttempts is the api turns' retry budget: two attempts, so a
// test walks one retry and then its exhaustion.
const controlAPIAttempts = 2

// controlAnswer ends one HTTP attempt of an api turn. Kind is:
//
//	ok          the reply completes: Text, then Calls when set
//	transient   529 overloaded before any output, an in-stream
//	            overloaded_error after it: retried within the budget
//	fatal       400 invalid_request_error (in-stream after output): no retry
//	overflow    400 "prompt is too long": agentllm.ErrContextOverflow
//	refused     stop_reason refusal
//	max_tokens  stop_reason max_tokens, after what was streamed and Text
type controlAnswer struct {
	Kind  string        `json:"kind"`
	Text  string        `json:"text,omitempty"`
	Calls []controlCall `json:"calls,omitempty"`
}

type controlNameKey struct{}

// api answers an api turn through messagesapi. The adapter is built per
// turn (it is cheap) so each turn's retry wait knows its own files.
func (a *controlAdapter) api(ctx context.Context, name string, turn controlTurn, r ullm.Request, o ullm.RequestOptions) (ullm.Response, error) {
	srv := &controlServer{c: a.c, name: name, first: turn.Answer}
	inner, err := messagesapi.New(messagesapi.Config{
		Client: anthropic.NewClient(
			option.WithAPIKey("llm-control"),
			option.WithBaseURL("http://llm-control.invalid/"),
			option.WithHTTPClient(&http.Client{Transport: srv}),
			option.WithMaxRetries(0),
		),
		Model:       a.Model,
		Fallbacks:   "off",
		MaxAttempts: controlAPIAttempts,
		Options:     a.opts,
		Wait:        srv.wait,
	})
	if err != nil {
		return ullm.Response{}, err
	}
	resp, err := inner.Respond(context.WithValue(ctx, controlNameKey{}, name), r, o)
	if err == nil {
		a.c.mu.Lock()
		addAgentUsage(&a.c.usage, resp.Usage)
		a.c.mu.Unlock()
	}
	return resp, err
}

// controlServer is the http.RoundTripper the api turn's client uses.
type controlServer struct {
	c     *controlLLM
	name  string
	first *controlAnswer // answers attempt 1 at once, without a hold

	mu sync.Mutex
	n  int // attempts so far
}

func (s *controlServer) file(suffix string) string {
	return filepath.Join(s.c.dir, s.name+"."+suffix)
}

// wait holds the retry until the test writes N.retry.
func (s *controlServer) wait(ctx context.Context, _ time.Duration) error {
	waiting := s.file("waiting")
	if err := os.WriteFile(waiting, nil, 0o644); err != nil {
		return err
	}
	defer os.Remove(waiting)
	tick := time.NewTicker(10 * time.Millisecond)
	defer tick.Stop()
	for {
		if err := os.Remove(s.file("retry")); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-tick.C:
		}
	}
}

// first is what the attempt decided before any byte of the body: a
// status answer, or a 200 whose body the attempt goes on writing.
type controlStart struct {
	resp *http.Response
	err  error
}

func (s *controlServer) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.Body != nil {
		io.Copy(io.Discard, req.Body)
		req.Body.Close()
	}
	s.mu.Lock()
	s.n++
	k := s.n
	s.mu.Unlock()
	if err := os.WriteFile(s.file("attempt-"+strconv.Itoa(k)), nil, 0o644); err != nil {
		return nil, err
	}
	started := make(chan controlStart, 1)
	go s.attempt(req, k, started)
	st := <-started
	return st.resp, st.err
}

// attempt holds one HTTP attempt: fragments stream as they are handed
// over, and the answer ends it.
func (s *controlServer) attempt(req *http.Request, k int, started chan<- controlStart) {
	ctx := req.Context()
	var w *io.PipeWriter
	sent := false // the 200 went out: answers are in-stream from here
	text := false // a text block is open
	id := fmt.Sprintf("msg_control_%s_%s_%d", s.c.tag, s.name, k)
	event := func(typ string, data map[string]any) {
		b, _ := json.Marshal(data)
		fmt.Fprintf(w, "event: %s\ndata: %s\n\n", typ, b)
	}
	begin := func() {
		if sent {
			return
		}
		sent = true
		var r *io.PipeReader
		r, w = io.Pipe()
		started <- controlStart{resp: &http.Response{
			StatusCode: http.StatusOK, Status: "200 OK", Request: req, Body: r,
			Header: http.Header{"Content-Type": {"text/event-stream"}},
		}}
		event("message_start", map[string]any{"type": "message_start", "message": map[string]any{
			"id": id, "type": "message", "role": "assistant", "model": "control", "content": []any{},
			"stop_reason": nil, "stop_sequence": nil, "usage": map[string]any{"input_tokens": 1, "output_tokens": 0},
		}})
	}
	delta := func(t string) {
		begin()
		if !text {
			text = true
			event("content_block_start", map[string]any{"type": "content_block_start", "index": 0, "content_block": map[string]any{"type": "text", "text": ""}})
		}
		event("content_block_delta", map[string]any{"type": "content_block_delta", "index": 0, "delta": map[string]any{"type": "text_delta", "text": t}})
	}
	status := func(code int, typ, msg string) {
		b, _ := json.Marshal(map[string]any{"type": "error", "error": map[string]any{"type": typ, "message": msg}})
		if !sent {
			started <- controlStart{resp: &http.Response{
				StatusCode: code, Status: fmt.Sprintf("%d %s", code, http.StatusText(code)), Request: req,
				Header: http.Header{"Content-Type": {"application/json"}}, Body: io.NopCloser(bytes.NewReader(b)),
			}}
			return
		}
		fmt.Fprintf(w, "event: error\ndata: %s\n\n", b)
		w.Close()
	}
	stop := func(reason string, extra map[string]any) {
		begin()
		if text {
			event("content_block_stop", map[string]any{"type": "content_block_stop", "index": 0})
		}
		d := map[string]any{"stop_reason": reason, "stop_sequence": nil}
		for k, v := range extra {
			d[k] = v
		}
		event("message_delta", map[string]any{"type": "message_delta", "delta": d, "usage": map[string]any{"output_tokens": 1}})
		event("message_stop", map[string]any{"type": "message_stop"})
		w.Close()
	}
	answer := func(an controlAnswer) {
		switch an.Kind {
		case "ok", "":
			if an.Text != "" {
				delta(an.Text)
			}
			if len(an.Calls) == 0 {
				stop("end_turn", nil)
				return
			}
			begin()
			idx := 0
			if text {
				event("content_block_stop", map[string]any{"type": "content_block_stop", "index": 0})
				idx = 1
			}
			for i, c := range an.Calls {
				cid := c.ID
				if cid == "" {
					cid = fmt.Sprintf("toolu_control_%s_%s_%d_%d", s.c.tag, s.name, k, i+1)
				}
				args := string(c.Args)
				if args == "" || args == "null" {
					args = "{}"
				}
				event("content_block_start", map[string]any{"type": "content_block_start", "index": idx, "content_block": map[string]any{"type": "tool_use", "id": cid, "name": c.Name, "input": map[string]any{}}})
				event("content_block_delta", map[string]any{"type": "content_block_delta", "index": idx, "delta": map[string]any{"type": "input_json_delta", "partial_json": args}})
				event("content_block_stop", map[string]any{"type": "content_block_stop", "index": idx})
				idx++
			}
			event("message_delta", map[string]any{"type": "message_delta", "delta": map[string]any{"stop_reason": "tool_use", "stop_sequence": nil}, "usage": map[string]any{"output_tokens": 1}})
			event("message_stop", map[string]any{"type": "message_stop"})
			w.Close()
		case "transient":
			status(529, "overloaded_error", "Overloaded")
		case "fatal":
			status(http.StatusBadRequest, "invalid_request_error", "llm-control: the request was refused")
		case "overflow":
			status(http.StatusBadRequest, "invalid_request_error", "prompt is too long: 250000 tokens > 200000 maximum")
		case "refused":
			stop("refusal", map[string]any{"stop_details": map[string]any{"type": "refusal", "category": "cyber", "explanation": "llm-control declined"}})
		case "max_tokens":
			if an.Text != "" {
				delta(an.Text)
			}
			stop("max_tokens", nil)
		default:
			status(http.StatusBadRequest, "invalid_request_error", fmt.Sprintf("llm-control: %s: unknown answer %q", s.name, an.Kind))
		}
	}
	if k == 1 && s.first != nil {
		answer(*s.first)
		return
	}
	tick := time.NewTicker(10 * time.Millisecond)
	defer tick.Stop()
	for {
		if b, err := os.ReadFile(s.file("stream")); err == nil {
			delta(string(b))
			if err := os.Remove(s.file("stream")); err != nil {
				status(http.StatusInternalServerError, "api_error", err.Error())
				return
			}
		}
		if b, err := os.ReadFile(s.file("answer")); err == nil {
			var an controlAnswer
			if err := json.Unmarshal(b, &an); err != nil {
				continue // half written: the test renames it into place
			}
			os.Remove(s.file("answer"))
			answer(an)
			return
		}
		select {
		case <-ctx.Done():
			if sent {
				w.CloseWithError(ctx.Err())
			} else {
				started <- controlStart{err: ctx.Err()}
			}
			return
		case <-tick.C:
		}
	}
}
