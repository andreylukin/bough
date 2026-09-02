package ask

import (
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/history"
)

// fakeCode records tool registrations; Pause is a counted no-op.
type fakeCode struct {
	mu     sync.Mutex
	tools  map[string]any
	paused int
}

func (f *fakeCode) RegisterTool(name string, fn any) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.tools == nil {
		f.tools = map[string]any{}
	}
	f.tools[name] = fn
}

func (f *fakeCode) Pause() func() {
	f.mu.Lock()
	f.paused++
	f.mu.Unlock()
	return func() {}
}

// fakeHist records appended entries.
type fakeHist struct {
	mu      sync.Mutex
	entries []history.Entry
}

func (f *fakeHist) Append(kind string, data map[string]any) history.Entry {
	f.mu.Lock()
	defer f.mu.Unlock()
	e := history.Entry{Seq: int64(len(f.entries) + 1), At: time.Now(), Kind: kind, Data: data}
	f.entries = append(f.entries, e)
	return e
}

func (f *fakeHist) all() []history.Entry {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]history.Entry(nil), f.entries...)
}

// mount applies the ask plugin onto a context with fake codemode and
// history, returning the registered tool, the Asker, the fakes, and
// the context (for event subscriptions).
func mount(t *testing.T, cfg map[string]any) (func(string, ...string) (string, error), *Asker, *fakeCode, *fakeHist, *kernel.Context) {
	t.Helper()
	ctx := kernel.NewContext()
	code := &fakeCode{}
	hist := &fakeHist{}
	ctx.Provide("codemode", code)
	ctx.Provide("history", hist)
	if err := (plugin{}).Apply(ctx, cfg); err != nil {
		t.Fatalf("apply: %v", err)
	}
	a, err := kernel.Get[*Asker](ctx, "ask-answers")
	if err != nil {
		t.Fatalf("ask-answers service: %v", err)
	}
	fn, ok := code.tools["ask"].(func(string, ...string) (string, error))
	if !ok {
		t.Fatalf("tools.ask not registered as func(string, ...string) (string, error): %T", code.tools["ask"])
	}
	return fn, a, code, hist, ctx
}

func TestAskBlocksUntilAnswered(t *testing.T) {
	t.Parallel()
	fn, a, code, hist, ctx := mount(t, nil)

	// The emitted loop event carries the pending id; answer through it.
	events := make(chan Event, 1)
	ctx.On("loop/event", func(p any) {
		if ev, ok := p.(Event); ok {
			events <- ev
		}
	})
	go func() {
		ev := <-events
		if ev.Kind != "ask" || ev.Text != "fav color?" || len(ev.Options) != 2 {
			t.Errorf("bad ask event: %+v", ev)
		}
		if err := a.Answer(ev.ID, "blue"); err != nil {
			t.Errorf("answer: %v", err)
		}
	}()

	got, err := fn("fav color?", "red", "blue")
	if err != nil {
		t.Fatalf("ask: %v", err)
	}
	if got != "blue" {
		t.Fatalf("ask returned %q, want blue", got)
	}
	if code.paused != 1 {
		t.Errorf("ask should Pause the codemode timer once, got %d", code.paused)
	}

	entries := hist.all()
	if len(entries) != 2 || entries[0].Kind != "ask" || entries[1].Kind != "ask/answer" {
		t.Fatalf("want ask + ask/answer entries, got %+v", entries)
	}
	if q, _ := entries[0].Data["question"].(string); q != "fav color?" {
		t.Errorf("ask entry question = %q", q)
	}
	if opts, _ := entries[0].Data["options"].([]string); len(opts) != 2 || opts[1] != "blue" {
		t.Errorf("ask entry options = %v", entries[0].Data["options"])
	}
	id, _ := entries[0].Data["id"].(string)
	if aid, _ := entries[1].Data["id"].(string); id == "" || aid != id {
		t.Errorf("answer id %q does not match ask id %q", aid, id)
	}
	if txt, _ := entries[1].Data["text"].(string); txt != "blue" {
		t.Errorf("answer entry text = %q", txt)
	}
}

func TestAskTimeoutErrors(t *testing.T) {
	t.Parallel()
	fn, a, _, hist, _ := mount(t, nil)
	a.timeout = 30 * time.Millisecond // test override; config is minutes
	_, err := fn("anyone there?")
	if err == nil || !strings.Contains(err.Error(), "no answer") {
		t.Fatalf("want 'no answer' error, got %v", err)
	}
	// The pending ask is gone: a late answer errors.
	entries := hist.all()
	if len(entries) != 1 || entries[0].Kind != "ask" {
		t.Fatalf("timeout should leave only the ask entry, got %+v", entries)
	}
	id, _ := entries[0].Data["id"].(string)
	if err := a.Answer(id, "late"); err == nil {
		t.Fatal("answering a timed-out ask should error")
	}
}

func TestAnswerUnknownIDErrors(t *testing.T) {
	t.Parallel()
	_, a, _, _, _ := mount(t, nil)
	if err := a.Answer("nope", "text"); err == nil {
		t.Fatal("unknown id should error")
	}
}

func TestTimeoutConfig(t *testing.T) {
	t.Parallel()
	_, a, _, _, _ := mount(t, map[string]any{"timeout_minutes": 3})
	if a.timeout != 3*time.Minute {
		t.Fatalf("timeout = %v, want 3m", a.timeout)
	}
	// Default.
	_, a, _, _, _ = mount(t, nil)
	if a.timeout != 10*time.Minute {
		t.Fatalf("default timeout = %v, want 10m", a.timeout)
	}
	// Bad values fail the mount loudly.
	for _, bad := range []any{"soon", 0, -2, 1.5} {
		ctx := kernel.NewContext()
		ctx.Provide("codemode", &fakeCode{})
		if err := (plugin{}).Apply(ctx, map[string]any{"timeout_minutes": bad}); err == nil {
			t.Errorf("timeout_minutes=%v should fail Apply", bad)
		}
	}
}

func TestAskWithoutHistoryStillWorks(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	code := &fakeCode{}
	ctx.Provide("codemode", code)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatalf("apply without history: %v", err)
	}
	a, _ := kernel.Get[*Asker](ctx, "ask-answers")
	fn := code.tools["ask"].(func(string, ...string) (string, error))
	events := make(chan Event, 1)
	ctx.On("loop/event", func(p any) { events <- p.(Event) })
	go func() {
		ev := <-events
		_ = a.Answer(ev.ID, "ok")
	}()
	got, err := fn("q?")
	if err != nil || got != "ok" {
		t.Fatalf("got %q, %v", got, err)
	}
}
