package responses

import (
	"bytes"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"io"
	"mime"
	"net/http"
	"sync/atomic"

	"github.com/andreylukin/bough/internal/agentllm"
)

// attemptKey carries the per-Respond round-trip counter: the harness
// retries inside its adapter, and every retry restarts the stream, so a
// consumer must reset its partial text when Attempt changes.
type attemptKey struct{}

// tap is a RoundTripper that tees an event-stream body into deltas. It
// is best effort and UI-only: the harness parses the same bytes for the
// response, so if the tap misreads a frame a GPT turn only goes quiet
// until it completes.
type tap struct {
	base http.RoundTripper
	sink func(agentllm.Delta)
}

func (t *tap) RoundTrip(req *http.Request) (*http.Response, error) {
	// Counted before the response is known: a 500 is an attempt too, and
	// the stream after it must read as a new one.
	attempt := 1
	if n, ok := req.Context().Value(attemptKey{}).(*atomic.Int32); ok {
		attempt = int(n.Add(1))
	}
	resp, err := t.base.RoundTrip(req)
	if err != nil || t.sink == nil || resp == nil || resp.Body == nil {
		return resp, err
	}
	if mt, _, _ := mime.ParseMediaType(resp.Header.Get("Content-Type")); mt != "text/event-stream" {
		return resp, nil
	}
	resp.Body = &tee{
		ReadCloser: resp.Body,
		emit: func(d agentllm.Delta) {
			d.Seq = agentllm.SeqOf(req.Context())
			d.Attempt = attempt
			t.sink(d)
		},
	}
	return resp, nil
}

// tee splits the SSE body into frames as it is read.
type tee struct {
	io.ReadCloser
	emit func(agentllm.Delta)
	buf  []byte
}

// maxPending bounds what the tee holds while waiting for a frame to
// end; a frame bigger than this (a huge completed item) is skipped, and
// only the deltas it would have carried are lost.
const maxPending = 1 << 20

func (t *tee) Read(p []byte) (int, error) {
	n, err := t.ReadCloser.Read(p)
	if n > 0 {
		t.buf = append(t.buf, p[:n]...)
		t.frames()
		if len(t.buf) > maxPending {
			t.buf = t.buf[:0]
		}
	}
	return n, err
}

func (t *tee) frames() {
	for {
		i, width := frameEnd(t.buf)
		if i < 0 {
			return
		}
		frame := t.buf[:i]
		t.buf = t.buf[i+width:]
		Frame(frame, t.emit)
	}
}

func frameEnd(b []byte) (int, int) {
	i := bytes.Index(b, []byte("\n\n"))
	j := bytes.Index(b, []byte("\r\n\r\n"))
	switch {
	case i < 0:
		if j < 0 {
			return -1, 0
		}
		return j, 4
	case j >= 0 && j < i:
		return j, 4
	}
	return i, 2
}

// streamEvent is the slice of a Responses stream event the tap reads.
type streamEvent struct {
	Type  string `json:"type"`
	Delta string `json:"delta"`
	Item  struct {
		Type   string `json:"type"`
		CallID string `json:"call_id"`
		Name   string `json:"name"`
	} `json:"item"`
}

// Frame reads one SSE frame and emits the delta it carries, if any.
func Frame(frame []byte, emit func(agentllm.Delta)) {
	var data []byte
	for _, line := range bytes.Split(frame, []byte("\n")) {
		line = bytes.TrimRight(line, "\r")
		if v, ok := bytes.CutPrefix(line, []byte("data:")); ok {
			data = append(data, bytes.TrimPrefix(v, []byte(" "))...)
		}
	}
	if len(data) == 0 || !jsontext.Value(data).IsValid() {
		return
	}
	var ev streamEvent
	if json.Unmarshal(data, &ev) != nil {
		return
	}
	switch ev.Type {
	case "response.output_text.delta":
		if ev.Delta != "" {
			emit(agentllm.Delta{Kind: agentllm.DeltaText, Text: ev.Delta})
		}
	// OpenAI streams a reasoning summary; OpenRouter serving an
	// Anthropic model streams the (summarized) thinking itself as
	// reasoning_text, which is what a recorded session shows.
	case "response.reasoning_summary_text.delta", "response.reasoning_text.delta":
		if ev.Delta != "" {
			emit(agentllm.Delta{Kind: agentllm.DeltaThinking, Text: ev.Delta})
		}
	case "response.output_item.added":
		if ev.Item.Type == "function_call" {
			emit(agentllm.Delta{Kind: agentllm.DeltaToolStart, CallID: ev.Item.CallID, Name: ev.Item.Name})
		}
	}
}
