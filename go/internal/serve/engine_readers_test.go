package serve

import (
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// Readers of an engine session (go/docs/unreal-engine.md §10.4): its
// tool rows are native "call" entries with no code block around them.

// A turn that opens on a native call is a turn: a Stop arriving then
// must reach the child, not be held for holdLimit as if the prompt were
// still unread.
func TestInTurnOnCall(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	for _, kind := range []string{"call", "call-delta"} {
		ch := &child{id: "s-" + kind, done: make(chan struct{}), unread: true}
		f.sup.emit(ch, kind, "go test ./...", map[string]any{"id": "c1", "tool": "bash", "phase": "start"})
		f.sup.mu.Lock()
		unread, in := ch.unread, ch.inTurn
		f.sup.mu.Unlock()
		if unread || !in {
			t.Errorf("%s: unread=%v inTurn=%v, want the prompt taken and a turn open", kind, unread, in)
		}
	}
}

// Call output streams like reply text, coalesced per call: two calls
// running at once must not merge into one row's tail.
func TestCallDeltasCoalescePerCall(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	sub, unsub := f.sup.Subscribe("s-cd")
	defer unsub()
	ch := &child{id: "s-cd", done: make(chan struct{})}
	f.sup.emit(ch, "call-delta", "a1 ", map[string]any{"id": "a"})
	f.sup.emit(ch, "call-delta", "a2 ", map[string]any{"id": "a"})
	f.sup.emit(ch, "call-delta", "b1", map[string]any{"id": "b"})
	f.sup.emit(ch, "call-delta", "a3", map[string]any{"id": "a"})

	want := []struct{ id, text string }{{"a", "a1 a2 "}, {"b", "b1"}, {"a", "a3"}}
	for _, w := range want {
		ev := recv(t, sub)
		if ev.Kind != "call-delta" || ev.Text != w.text || ev.Extra["id"] != w.id || ev.Seq != 0 {
			t.Fatalf("event = %+v, want call-delta %q for %s at seq 0", ev, w.text, w.id)
		}
	}
	if got := f.sup.Recent("s-cd"); len(got) != 0 {
		t.Fatalf("ring = %+v, want no call-delta in it", got)
	}
}

// A delta-reset drops the buffered reply text it supersedes, keeps call
// output, and is live only.
func TestDeltaResetDropsBufferedText(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	sub, unsub := f.sup.Subscribe("s-reset")
	defer unsub()
	ch := &child{id: "s-reset", done: make(chan struct{})}
	f.sup.emit(ch, "assistant-delta", "Hel", nil)
	f.sup.emit(ch, "call-delta", "out", map[string]any{"id": "c"})
	f.sup.emit(ch, "delta-reset", "", map[string]any{"seq": 1.0})
	f.sup.emit(ch, "assistant-delta", "Hello", nil)

	if ev := recv(t, sub); ev.Kind != "call-delta" || ev.Text != "out" {
		t.Fatalf("first = %+v, want the call output kept", ev)
	}
	if ev := recv(t, sub); ev.Kind != "delta-reset" || ev.Seq != 0 {
		t.Fatalf("second = %+v, want the live reset", ev)
	}
	if ev := recv(t, sub); ev.Kind != "assistant-delta" || ev.Text != "Hello" {
		t.Fatalf("third = %+v, want only the retried text", ev)
	}
	if got := f.sup.Recent("s-reset"); len(got) != 0 {
		t.Fatalf("ring = %+v, want nothing recorded", got)
	}
}

func TestTurnTestsFromCalls(t *testing.T) {
	t.Parallel()
	got := turnTests([]history.Entry{
		{Kind: "call", Data: map[string]any{"tool": "bash", "text": "cd go", "cmd": "cd go && go test ./...", "exit": float64(1)}},
		{Kind: "done"},
		// A loop session's per-call row has no cmd: its block counts, not it.
		{Kind: "call", Data: map[string]any{"tool": "bash", "text": "go test ./...", "exit": float64(0)}},
		{Kind: "done"},
		{Kind: "call", Data: map[string]any{"tool": "view", "cmd": "go test", "exit": 0}},
		{Kind: "done"},
	})
	if got[1] == nil || got[1].Exit != 1 || got[1].Cmd != "go test" {
		t.Fatalf("turn 1 = %+v", got[1])
	}
	if got[2] != nil || got[3] != nil {
		t.Fatalf("turns 2/3 = %+v %+v, want none", got[2], got[3])
	}
}

func TestLastTestFromCall(t *testing.T) {
	t.Parallel()
	es := entries(
		ent(1, "input", text("run it")),
		ent(2, "call", map[string]any{"tool": "bash", "text": "go test", "cmd": "go test ./...", "exit": 1.0}),
		ent(3, "call", map[string]any{"tool": "bash", "text": "ls", "cmd": "ls", "exit": 0.0}),
	)
	failed, at := lastTest(es)
	if !failed || !at.Equal(es[1].At) {
		t.Fatalf("lastTest = %v %v, want the failed go test at seq 2", failed, at)
	}
}

// An engine turn records the cache TTL it asked for; the model default
// (5m for Sonnet) would count a 1h prefix cold after five minutes.
func TestLastCacheUsesRecordedTTL(t *testing.T) {
	t.Parallel()
	done := func(ttl any) history.Entry {
		u := map[string]any{"in": 1000.0, "cache_read": 800.0}
		if ttl != nil {
			u["ttl"] = ttl
		}
		return ent(1, "done", map[string]any{"usage": u})
	}
	model := "anthropic/claude-sonnet-5"
	base := LastCache(entries(done(nil)), model).TTL
	for _, tc := range []struct {
		ttl  any
		want int
	}{{"1h", 3600}, {"5m", 300}, {3600.0, 3600}, {"junk", base}} {
		if got := LastCache(entries(done(tc.ttl)), model).TTL; got != tc.want {
			t.Errorf("ttl %v: TTL = %d, want %d", tc.ttl, got, tc.want)
		}
	}
}

// The engine's native ask ends as a call. An answer clears Needs you,
// and so does the call ending without one (a timeout).
func TestStatusOfEngineAsk(t *testing.T) {
	t.Parallel()
	ask := ent(3, "ask", map[string]any{"id": "q1", "question": "which?", "options": []any{"a", "b"}})
	base := entries(ent(1, "input", text("x")), ent(2, "call", map[string]any{"tool": "bash", "text": "ls", "id": "c0", "ms": 3.0}), ask)
	if st, _ := StatusOf(base, true); st != StatusNeedsYou {
		t.Fatalf("armed: %s, want needs-you", st)
	}
	answered := append(append([]history.Entry{}, base...), ent(4, "ask/answer", map[string]any{"id": "q1", "text": "a"}))
	if st, _ := StatusOf(answered, true); st != StatusRunning {
		t.Fatalf("answered: %s, want running", st)
	}
	timedOut := append(append([]history.Entry{}, base...), ent(4, "call", map[string]any{"tool": "ask", "text": "which?", "id": "c1", "error": "ask: no answer after 10m0s"}))
	if st, _ := StatusOf(timedOut, true); st != StatusRunning {
		t.Fatalf("timed out: %s, want running", st)
	}
}

// The native secret is the same shape: its call ending without an
// answer (a timeout) leaves nothing waiting on you. Found by
// tests/model/mbt/ask_answer_test.go (AskSecret then Resolve).
func TestStatusOfEngineSecretTimeout(t *testing.T) {
	t.Parallel()
	ask := ent(3, "ask", map[string]any{"id": "ask-2", "question": "Secret TOKEN for p1: why", "secret": true})
	base := entries(ent(1, "input", text("x")), ask)
	if st, _ := StatusOf(base, true); st != StatusNeedsYou {
		t.Fatalf("armed: %s, want needs-you", st)
	}
	timedOut := append(append([]history.Entry{}, base...), ent(4, "call", map[string]any{"tool": "secret", "text": "TOKEN", "id": "c1", "error": "secret: ask: no answer after 10m0s"}))
	if st, a := StatusOf(timedOut, true); st != StatusRunning || a != nil {
		t.Fatalf("timed out: %s %+v, want running with no ask", st, a)
	}
}

func TestLastModelFromEngineEntry(t *testing.T) {
	t.Parallel()
	es := entries(
		ent(1, "engine", map[string]any{"engine": "unreal", "model": "claude-opus-5-5", "provider": "anthropic"}),
		ent(2, "input", text("hi")),
	)
	if got := lastModel(es); got != "claude-opus-5-5" {
		t.Fatalf("lastModel = %q, want the engine entry's model", got)
	}
	es = append(es, history.Entry{Seq: 3, At: time.Now(), Kind: "assistant", Data: map[string]any{"text": "x", "model": "gpt-5.6-sol"}})
	if got := lastModel(es); got != "gpt-5.6-sol" {
		t.Fatalf("lastModel = %q, want the later reply's model", got)
	}
}
