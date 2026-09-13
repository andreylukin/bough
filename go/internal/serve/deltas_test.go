package serve

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// recv reads one event, or fails: a delta that never arrives is the
// bug this whole file is about.
func recv(t *testing.T, ch <-chan Event) Event {
	t.Helper()
	select {
	case ev, ok := <-ch:
		if !ok {
			t.Fatal("subscription closed")
		}
		return ev
	case <-time.After(5 * time.Second):
		t.Fatal("no event reached the subscriber")
	}
	return Event{}
}

func TestDeltasReachSubscriber(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	ch, unsub := f.sup.Subscribe("s-delta")
	defer unsub()

	fake := &child{id: "s-delta", done: make(chan struct{})}
	f.sup.emit(fake, "thinking-delta", "let me ", nil)
	f.sup.emit(fake, "thinking-delta", "think", nil)
	f.sup.emit(fake, "assistant-delta", "hi ", nil)
	f.sup.emit(fake, "assistant-delta", "there", nil)

	// A burst inside one window arrives as one frame per run, in
	// arrival order, with the fragments joined.
	if ev := recv(t, ch); ev.Kind != "thinking-delta" || ev.Text != "let me think" {
		t.Fatalf("first delta = %+v, want the coalesced thinking run", ev)
	}
	ev := recv(t, ch)
	if ev.Kind != "assistant-delta" || ev.Text != "hi there" {
		t.Fatalf("second delta = %+v, want the coalesced assistant run", ev)
	}
	if ev.Seq != 0 {
		t.Errorf("delta Seq = %d, want 0: a delta must not take a supervisor sequence number", ev.Seq)
	}
	if ev.Session != "s-delta" {
		t.Errorf("delta session = %q", ev.Session)
	}
}

// A delta that arrived before the recorded reply must not be delivered
// after it, or the browser appends a duplicate tail to text it has
// already finished rendering.
func TestDeltasFlushBeforeRecordedEvent(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	ch, unsub := f.sup.Subscribe("s-order")
	defer unsub()

	fake := &child{id: "s-order", done: make(chan struct{})}
	f.sup.emit(fake, "assistant-delta", "hi there", nil)
	f.sup.emit(fake, "assistant", "hi there", nil)

	if ev := recv(t, ch); ev.Kind != "assistant-delta" {
		t.Fatalf("first event = %+v, want the delta", ev)
	}
	if ev := recv(t, ch); ev.Kind != "assistant" || ev.Seq != 1 {
		t.Fatalf("second event = %+v, want the recorded assistant line at seq 1", ev)
	}
}

// Deltas are ephemeral: they are not in the replay ring, they do not
// consume the session's sequence, and nothing writes them to history.
func TestDeltasAreNotRecorded(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "s-eph", history.Entry{
		Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": f.home},
	}, history.Entry{
		Seq: 2, At: time.Now(), Kind: "assistant", Data: map[string]any{"text": "hi there"},
	})

	fake := &child{id: "s-eph", done: make(chan struct{})}
	for _, frag := range []string{"hi ", "there"} {
		f.sup.emit(fake, "assistant-delta", frag, nil)
		f.sup.emit(fake, "thinking-delta", frag, nil)
	}
	f.sup.emit(fake, "done", "", nil)

	for _, ev := range f.sup.Recent("s-eph") {
		if isDelta(ev.Kind) {
			t.Fatalf("the replay ring holds a delta: %+v", ev)
		}
	}
	if got := f.sup.Recent("s-eph"); len(got) != 1 || got[0].Seq != 1 {
		t.Fatalf("ring = %+v, want just the recorded done at seq 1", got)
	}
	entries, err := f.sup.Entries("s-eph")
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		if isDelta(e.Kind) {
			t.Fatalf("history holds a delta: %+v", e)
		}
	}
	if len(entries) != 2 {
		t.Fatalf("history = %d entries, want the 2 that were seeded", len(entries))
	}
}

// The late joiner: someone who opens the page after a turn finished
// gets the transcript from history through the existing ?since= path,
// and the events stream replays no half-written text at them.
func TestLateSubscriberStillGetsTranscript(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.seed(t, "s-late", history.Entry{
		Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": f.home},
	}, history.Entry{
		Seq: 2, At: time.Now(), Kind: "input", Data: map[string]any{"text": "hey"},
	}, history.Entry{
		Seq: 3, At: time.Now(), Kind: "assistant", Data: map[string]any{"text": "hi there"},
	}, history.Entry{
		Seq: 4, At: time.Now(), Kind: "done", Data: map[string]any{},
	})

	// The turn streamed and then finished, all before anyone watched.
	fake := &child{id: "s-late", done: make(chan struct{})}
	f.sup.emit(fake, "thinking-delta", "hmm", nil)
	f.sup.emit(fake, "assistant-delta", "hi ", nil)
	f.sup.emit(fake, "assistant-delta", "there", nil)
	f.sup.emit(fake, "assistant", "hi there", nil)
	f.sup.emit(fake, "done", "", nil)

	code, body := f.do(t, "GET", "/api/sessions/s-late?since=1", "")
	if code != http.StatusOK {
		t.Fatalf("GET session = %d: %v", code, body)
	}
	lines, _ := body["entries"].([]any)
	var kinds []string
	for _, l := range lines {
		kinds = append(kinds, l.(map[string]any)["kind"].(string))
	}
	if strings.Join(kinds, ",") != "input,assistant,done" {
		t.Fatalf("transcript kinds = %v, want the finished turn unchanged", kinds)
	}

	// And the stream it opens next replays the recorded events only.
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	req, _ := http.NewRequestWithContext(ctx, "GET", f.srv.URL+"/api/sessions/s-late/events", nil)
	resp, err := f.srv.Client().Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	frames := readFrames(t, resp.Body, 3)
	for _, fr := range frames {
		if strings.Contains(fr, "-delta") {
			t.Fatalf("a late joiner was replayed a delta: %q", fr)
		}
	}
	var ev Event
	data := frames[2][strings.Index(frames[2], "data: ")+len("data: "):]
	if err := json.Unmarshal([]byte(data), &ev); err != nil {
		t.Fatalf("frame %q: %v", data, err)
	}
	if ev.Kind != "done" {
		t.Errorf("last replayed frame = %+v, want the recorded done", ev)
	}
}
