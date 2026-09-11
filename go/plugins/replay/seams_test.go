package replay

import (
	"context"
	"errors"
	"testing"

	"github.com/andreylukin/bough/plugins/llm"
)

// seamsTape is two turns: the first answered with recorded usage and a
// model, the second a model call that failed.
func seamsTape(t *testing.T) *Tape {
	t.Helper()
	tp, err := Load(tapeMechanicsWrite(t, t.TempDir(),
		`{"seq":1,"kind":"input","data":{"text":"one"}}`,
		`{"seq":2,"kind":"assistant","data":{"text":"a1","model":"m-1","provider":"p"}}`,
		`{"seq":3,"kind":"result","data":{"code":"x","text":"ok"}}`,
		`{"seq":4,"kind":"error","data":{"text":"hook blocked: not a model failure"}}`,
		`{"seq":5,"kind":"assistant","data":{"text":"a2","model":"m-2"}}`,
		`{"seq":6,"kind":"done","data":{"usage":{"in":100,"out":10,"cache_read":5,"last_in":60,"cost":0.5}}}`,
		`{"seq":7,"kind":"input","data":{"text":"two"}}`,
		`{"seq":8,"kind":"error","data":{"text":"429 rate limited"}}`,
		`{"seq":9,"kind":"done","data":{"text":""}}`,
		`{"seq":10,"kind":"input","data":{"text":"three"}}`,
		`{"seq":11,"kind":"assistant","data":{"text":"a3"}}`,
		`{"seq":12,"kind":"done","data":{"usage":{"in":1,"out":2,"last_in":61,"cost":0.25}}}`,
	))
	if err != nil {
		t.Fatal(err)
	}
	return tp
}

func TestModelRecordsMessages(t *testing.T) {
	t.Parallel()
	m := &Model{tape: seamsTape(t)}
	msgs := []llm.Message{{Role: "user", Content: "one"}}
	if _, err := m.Complete(t.Context(), "sys", msgs); err != nil {
		t.Fatal(err)
	}
	msgs[0].Content = "mutated after the call"
	if _, err := m.Stream(t.Context(), "sys", []llm.Message{{Role: "user", Content: "one"}, {Role: "assistant", Content: "a1"}}, func(string) {}); err != nil {
		t.Fatal(err)
	}
	got := m.Messages()
	if len(got) != 2 || len(got[0]) != 1 || got[0][0].Content != "one" || len(got[1]) != 2 || got[1][1].Content != "a1" {
		t.Fatalf("Messages() = %+v", got)
	}
}

func TestModelReportsRecordedUsageAndModel(t *testing.T) {
	t.Parallel()
	var m any = &Model{tape: seamsTape(t)}
	rep, ok := m.(llm.UsageReporter)
	if !ok {
		t.Fatal("replay.Model is not an llm.UsageReporter")
	}
	mod, ok := m.(llm.Modeler)
	if !ok {
		t.Fatal("replay.Model is not an llm.Modeler")
	}
	model := m.(*Model)
	if u := rep.Usage(); u != (llm.Usage{}) || mod.Model() != "" {
		t.Fatalf("before any call: %+v %q", u, mod.Model())
	}
	model.Complete(t.Context(), "", nil) // a1: its turn's done is not reached yet
	if u := rep.Usage(); u.InputTokens != 0 || mod.Model() != "m-1" {
		t.Fatalf("after a1: %+v %q", u, mod.Model())
	}
	model.Complete(t.Context(), "", nil) // a2 ends turn one
	want := llm.Usage{InputTokens: 100, OutputTokens: 10, CacheReadTokens: 5, LastInputTokens: 60, Cost: 0.5, Priced: true}
	if u := rep.Usage(); u != want || mod.Model() != "m-2" {
		t.Fatalf("after turn one: %+v %q, want %+v m-2", u, mod.Model(), want)
	}
	model.Complete(t.Context(), "", nil) // the failed call
	model.Complete(t.Context(), "", nil) // a3
	want = llm.Usage{InputTokens: 101, OutputTokens: 12, CacheReadTokens: 5, LastInputTokens: 61, Cost: 0.75, Priced: true}
	if u := rep.Usage(); u != want || mod.Model() != "m-2" {
		t.Fatalf("after turn three: %+v %q, want %+v m-2", u, mod.Model(), want)
	}
}

func TestModelReturnsRecordedError(t *testing.T) {
	t.Parallel()
	tp := seamsTape(t)
	if len(tp.Replies) != 3 {
		t.Fatalf("replies = %q", tp.Replies)
	}
	m := &Model{tape: tp}
	for _, want := range []string{"a1", "a2"} {
		if r, err := m.Complete(t.Context(), "", nil); err != nil || r != want {
			t.Fatalf("got %q %v, want %q (a mid-turn error entry is not a model failure)", r, err, want)
		}
	}
	if _, err := m.Stream(t.Context(), "", nil, func(string) {}); err == nil || err.Error() != "429 rate limited" {
		t.Fatalf("turn two: want the recorded error, got %v", err)
	}
	if r, err := m.Complete(t.Context(), "", nil); err != nil || r != "a3" {
		t.Fatalf("after the error: %q %v", r, err)
	}
}

func TestRunCtxHonoursCancel(t *testing.T) {
	t.Parallel()
	rt := &Runtime{tape: tapeMechanicsResults(t, "a", "b")}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := rt.RunCtx(ctx, "a"); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled RunCtx: %v", err)
	}
	if out, err := rt.RunCtx(t.Context(), "a"); err != nil || out != "out a" {
		t.Fatalf("the cancelled block consumed the tape: %q %v", out, err)
	}
}
