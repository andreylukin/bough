package hookbridge

import (
	"context"
	"encoding/json"
	"errors"
	"reflect"
	"slices"
	"testing"

	"github.com/andreylukin/bough/internal/agenttools"
)

// firer records what each event was fired with and answers from a
// table, the way plugins/hooks.Service would after merging hook files.
type firer struct {
	answers  map[string]map[string]any
	err      error
	fired    []string
	payloads map[string]map[string]any
	session  string
	records  []map[string]any
}

func (f *firer) Fire(_ context.Context, event string, payload map[string]any) (map[string]any, error) {
	f.fired = append(f.fired, event)
	if f.payloads == nil {
		f.payloads = map[string]map[string]any{}
	}
	f.payloads[event] = payload
	f.records = append(f.records, map[string]any{"event": event, "session": f.session})
	var out map[string]any
	if a := f.answers[event]; a != nil {
		out = map[string]any{}
		for k, v := range a {
			out[k] = v
		}
	}
	return out, f.err
}

func (f *firer) TakeFireRecords() []map[string]any {
	r := f.records
	f.records = nil
	return r
}

func (f *firer) SetSession(id string) { f.session = id }

func bridge(f *firer) (*Bridge, *[][2]string) {
	var notes [][2]string
	b := New(func() (Firer, bool) { return f, true })
	b.Session = "sess-1"
	b.Notify = func(kind, text string) { notes = append(notes, [2]string{kind, text}) }
	return b, &notes
}

var call = agenttools.Call{ID: "toolu_1", Args: json.RawMessage(`{"command":"git push","timeout":5}`)}

func TestEachEventsPayloadAndHonouredKeys(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	f := &firer{answers: map[string]map[string]any{
		"session-start":      {"context": "house rules"},
		"user-prompt-submit": {"input": "rewritten"},
		"pre-code-exec":      {"args": map[string]any{"command": "git status"}},
		"post-result":        {"result": "shortened"},
		"stop":               {"block": "run the tests first"},
	}}
	b, notes := bridge(f)

	if got := b.SessionStart(ctx); got != "house rules" {
		t.Fatalf("SessionStart = %q", got)
	}
	out, block, contexts := b.PromptSubmit(ctx, "typed")
	if out != "rewritten" || block != "" || len(contexts) != 1 || contexts[0] != "hook user-prompt-submit rewrote your message\nrewritten" {
		t.Fatalf("PromptSubmit = %q %q %v", out, block, contexts)
	}
	args, deny := b.PreTool(ctx, "bash", call, "git push")
	if deny != "" || string(args) != `{"command":"git status"}` {
		t.Fatalf("PreTool = %s %q", args, deny)
	}
	r := b.PostTool(ctx, "bash", call, "git push", agenttools.Result{Text: "long", Error: "exit 1", Data: map[string]any{"exit": 1}})
	if r.Text != "shortened" || r.Error != "exit 1" || r.Data["exit"] != 1 {
		t.Fatalf("PostTool = %+v", r)
	}
	if got := b.Stop(ctx, "all done"); got != "run the tests first" {
		t.Fatalf("Stop = %q", got)
	}

	want := map[string]map[string]any{
		"session-start":      {},
		"user-prompt-submit": {"input": "typed"},
		"pre-code-exec":      {"code": "git push", "tool": "bash", "args": map[string]any{"command": "git push", "timeout": float64(5)}, "call": "toolu_1"},
		"post-result":        {"code": "git push", "tool": "bash", "call": "toolu_1", "result": "long", "error": "exit 1"},
		"stop":               {"reply": "all done"},
	}
	if !reflect.DeepEqual(f.payloads, want) {
		t.Fatalf("payloads\n got %#v\nwant %#v", f.payloads, want)
	}
	if !slices.Equal(f.fired, []string{"session-start", "user-prompt-submit", "pre-code-exec", "post-result", "stop"}) {
		t.Fatalf("fired %v", f.fired)
	}
	recs := b.Drain()
	if len(recs) != 5 || recs[0]["session"] != "sess-1" || b.Drain() != nil {
		t.Fatalf("records %v", recs)
	}
	if !reflect.DeepEqual(*notes, [][2]string{{"context", "hook user-prompt-submit rewrote your message\nrewritten"}}) {
		t.Fatalf("notes %v", *notes)
	}
}

func TestDenyAndBlock(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	f := &firer{answers: map[string]map[string]any{
		"user-prompt-submit": {"block": "not today", "input": "ignored"},
		"pre-code-exec":      {"deny": "no pushing", "args": map[string]any{"command": "x"}},
	}}
	b, _ := bridge(f)
	if out, block, _ := b.PromptSubmit(ctx, "typed"); out != "typed" || block != "not today" {
		t.Fatalf("PromptSubmit = %q %q", out, block)
	}
	if args, deny := b.PreTool(ctx, "bash", call, "git push"); deny != "no pushing" || args != nil {
		t.Fatalf("PreTool = %s %q", args, deny)
	}
	f.answers["pre-code-exec"] = map[string]any{"deny": ""}
	if _, deny := b.PreTool(ctx, "bash", call, "git push"); deny == "" {
		t.Fatal("an empty deny reason must still deny")
	}
}

// A hook that fails is shown and recorded, never fatal, and what the
// other hooks returned still applies. A notice reaches the human and
// never the model.
func TestFireErrorIsNotFatalAndNoticeDecidesNothing(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	f := &firer{err: errors.New("broken.js: ReferenceError"), answers: map[string]map[string]any{
		"post-result": {"result": "still rewritten", "notice": "trimmed output"},
		"stop":        {"notice": "just saying"},
	}}
	b, notes := bridge(f)
	r := b.PostTool(ctx, "view", call, "a.go", agenttools.Result{Text: "orig"})
	if r.Text != "still rewritten" {
		t.Fatalf("partial result dropped: %+v", r)
	}
	if got := b.Stop(ctx, "done"); got != "" {
		t.Fatalf("a notice alone continued the turn: %q", got)
	}
	want := [][2]string{
		{"error", "hook post-result: broken.js: ReferenceError"},
		{"system", "hook post-result: trimmed output"},
		{"error", "hook stop: broken.js: ReferenceError"},
		{"system", "hook stop: just saying"},
	}
	if !reflect.DeepEqual(*notes, want) {
		t.Fatalf("notes\n got %v\nwant %v", *notes, want)
	}
	if len(b.Drain()) != 2 {
		t.Fatal("failed fires must still be recorded")
	}
}

// No hooks row: every point passes through untouched.
func TestNoHooksRow(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	for _, b := range []*Bridge{New(nil), New(func() (Firer, bool) { return nil, false })} {
		if b.SessionStart(ctx) != "" || b.Stop(ctx, "x") != "" || b.Drain() != nil {
			t.Fatal("no-op points returned something")
		}
		if out, block, c := b.PromptSubmit(ctx, "hi"); out != "hi" || block != "" || c != nil {
			t.Fatal("PromptSubmit changed the line")
		}
		if a, d := b.PreTool(ctx, "bash", call, "x"); a != nil || d != "" {
			t.Fatal("PreTool decided something")
		}
		if r := b.PostTool(ctx, "bash", call, "x", agenttools.Result{Text: "t"}); r.Text != "t" {
			t.Fatal("PostTool rewrote")
		}
	}
}

// Arguments that are not a JSON object still reach the hook, as text.
func TestArgsObject(t *testing.T) {
	t.Parallel()
	if got := argsObject(nil); !reflect.DeepEqual(got, map[string]any{}) {
		t.Fatalf("empty args = %#v", got)
	}
	if got := argsObject(json.RawMessage(`[1]`)); got != "[1]" {
		t.Fatalf("non-object args = %#v", got)
	}
}

// sessionFirer is preferred over SetSession: fires go out and come back
// under the bridge's own session, and Drain leaves other sessions' fires.
type sessFirer struct {
	firer
	fired   []string
	pending map[string][]map[string]any
}

func (f *sessFirer) FireAs(ctx context.Context, session, event string, payload map[string]any) (map[string]any, error) {
	f.fired = append(f.fired, session+":"+event)
	if f.pending == nil {
		f.pending = map[string][]map[string]any{}
	}
	f.pending[session] = append(f.pending[session], map[string]any{"event": event})
	return nil, nil
}

func (f *sessFirer) TakeFireRecordsFor(session string) []map[string]any {
	r := f.pending[session]
	delete(f.pending, session)
	return r
}

func TestBridgeFiresAndDrainsUnderItsOwnSession(t *testing.T) {
	f := &sessFirer{firer: firer{answers: map[string]map[string]any{}}}
	b := New(func() (Firer, bool) { return f, true })
	b.Session = "parent-w1"
	f.pending = map[string][]map[string]any{"parent": {{"event": "stop"}}}
	b.SessionStart(context.Background())
	if !slices.Equal(f.fired, []string{"parent-w1:session-start"}) || f.session != "" {
		t.Fatalf("fired %v, SetSession %q", f.fired, f.session)
	}
	if got := b.Drain(); len(got) != 1 || got[0]["event"] != "session-start" {
		t.Fatalf("drained %v", got)
	}
	if len(f.pending["parent"]) != 1 {
		t.Fatal("the parent's fires were drained by the child")
	}
}
