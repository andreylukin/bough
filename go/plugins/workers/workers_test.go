package workers

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

// scriptLLM replies from a script, one entry per Complete call (last
// entry repeats). It records every message list it saw.
type scriptLLM struct {
	mu      sync.Mutex
	script  []string
	calls   int
	seenMsg []llm.Message
}

func (s *scriptLLM) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.seenMsg = append(s.seenMsg, messages...)
	i := s.calls
	s.calls++
	if i >= len(s.script) {
		i = len(s.script) - 1
	}
	return s.script[i], nil
}

func (s *scriptLLM) saw(sub string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, m := range s.seenMsg {
		if strings.Contains(m.Content, sub) {
			return true
		}
	}
	return false
}

// memHist records Append calls (the History seam).
type memHist struct {
	mu      sync.Mutex
	entries []history.Entry
}

func (m *memHist) Append(kind string, data map[string]any) history.Entry {
	m.mu.Lock()
	defer m.mu.Unlock()
	e := history.Entry{Seq: int64(len(m.entries) + 1), At: time.Now(), Kind: kind, Data: data}
	m.entries = append(m.entries, e)
	return e
}

func (m *memHist) kinds() []string {
	m.mu.Lock()
	defer m.mu.Unlock()
	var ks []string
	for _, e := range m.entries {
		ks = append(ks, e.Kind)
	}
	return ks
}

// mount wires a real codemode VM, the scripted llm, a recording history
// and an event collector, then Applies the workers plugin.
func mount(t *testing.T, cfg map[string]any, script ...string) (*kernel.Context, *codemode.CodeMode, *scriptLLM, *memHist, func() []loop.Event) {
	t.Helper()
	kctx := kernel.NewContext()
	cm := codemode.New(5 * time.Second)
	l := &scriptLLM{script: script}
	h := &memHist{}
	kctx.Provide("llm", l)
	kctx.Provide("codemode", cm)
	kctx.Provide("history", h)

	var mu sync.Mutex
	var events []loop.Event
	kctx.On("loop/event", func(p any) {
		ev, ok := p.(loop.Event)
		if !ok {
			t.Errorf("payload is %T, want loop.Event", p)
			return
		}
		mu.Lock()
		events = append(events, ev)
		mu.Unlock()
	})

	if err := (plugin{}).Apply(kctx, cfg); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	return kctx, cm, l, h, func() []loop.Event {
		mu.Lock()
		defer mu.Unlock()
		return append([]loop.Event(nil), events...)
	}
}

func TestSpawnRunsChild(t *testing.T) {
	_, cm, _, h, events := mount(t, nil,
		"on it\n```js\nconsole.log(tools.mark())\n```",
		"FINAL_FROM_CHILD",
	)
	cm.RegisterTool("mark", func() (string, error) { return "CHILD_TOOL_RAN", nil })

	// The parent's block: console output around the spawn must survive
	// the child's nested Run (out-buffer save/restore).
	out, err := cm.Run(`console.log("BEFORE"); var r = tools.spawn("do the task"); console.log("AFTER " + r)`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	for _, want := range []string{"BEFORE", "AFTER FINAL_FROM_CHILD"} {
		if !strings.Contains(out, want) {
			t.Fatalf("parent output missing %q: %q", want, out)
		}
	}
	if strings.Contains(out, "CHILD_TOOL_RAN") {
		t.Fatalf("child console output leaked into the parent's: %q", out)
	}

	wantKinds := []string{"sub:assistant", "sub:code", "sub:result", "sub:assistant", "sub:done"}
	evs := events()
	if len(evs) != len(wantKinds) {
		t.Fatalf("events %+v, want kinds %v", evs, wantKinds)
	}
	for i, ev := range evs {
		if ev.Kind != wantKinds[i] {
			t.Fatalf("event[%d].Kind = %q, want %q", i, ev.Kind, wantKinds[i])
		}
		if n, ok := ev.Data["worker"].(int); !ok || n != 1 {
			t.Fatalf("event[%d].Data = %v, want worker 1", i, ev.Data)
		}
	}
	if evs[2].Text != "CHILD_TOOL_RAN\n" {
		t.Fatalf("sub:result text = %q", evs[2].Text)
	}

	hk := h.kinds()
	for i, k := range wantKinds {
		if i >= len(hk) || hk[i] != k {
			t.Fatalf("history kinds %v, want %v", hk, wantKinds)
		}
	}
}

func TestSpawnDocumentedInPromptSections(t *testing.T) {
	kctx := kernel.NewContext()
	kctx.Provide("llm", &scriptLLM{script: []string{"ok"}})
	kctx.Provide("codemode", codemode.New(5*time.Second))
	secs := &loop.Sections{}
	kctx.Provide("prompt-sections", secs)
	if err := (plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	if !strings.Contains(secs.Text(), "tools.spawn(task) -> string") {
		t.Fatalf("section = %q, want tools.spawn documented", secs.Text())
	}
	kctx.Unmount()
	if secs.Text() != "" {
		t.Fatalf("section not withdrawn on unmount: %q", secs.Text())
	}
}

func TestSpawnDepthRefused(t *testing.T) {
	_, cm, l, _, _ := mount(t, nil,
		"trying\n```js\ntools.spawn('nested')\n```",
		"child gave up",
	)
	out, err := cm.Run(`tools.spawn("outer task")`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if out != "child gave up" {
		t.Fatalf("spawn returned %q", out)
	}
	// The nested spawn threw; the child saw the refusal as tool output.
	if !l.saw("subagent depth 1 only") {
		t.Fatalf("child never saw the depth refusal; messages: %+v", l.seenMsg)
	}
}

func TestSpawnCapAndReset(t *testing.T) {
	kctx, cm, _, _, _ := mount(t, map[string]any{"max_spawns": 2}, "ok")
	out, err := cm.Run(`
		var a = tools.spawn("one");
		var b = tools.spawn("two");
		var capped = "";
		try { tools.spawn("three") } catch (e) { capped = "CAPPED " + e }
		a + b + capped
	`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if !strings.Contains(out, "okok") || !strings.Contains(out, "CAPPED") || !strings.Contains(out, "spawn limit reached (2 per turn)") {
		t.Fatalf("output = %q", out)
	}

	// The loop's turn-end "done" resets the counter.
	kctx.Emit("loop/event", loop.Event{Kind: "done"})
	out, err = cm.Run(`tools.spawn("four")`)
	if err != nil || out != "ok" {
		t.Fatalf("spawn after reset = %q, %v", out, err)
	}
}

func TestConfigValidation(t *testing.T) {
	for _, cfg := range []map[string]any{
		{"bogus": 1},
		{"max_spawns": "many"},
		{"max_steps": 0},
		{"max_spawns": 1.5},
	} {
		kctx := kernel.NewContext()
		kctx.Provide("llm", &scriptLLM{script: []string{"ok"}})
		kctx.Provide("codemode", codemode.New(time.Second))
		if err := (plugin{}).Apply(kctx, cfg); err == nil {
			t.Fatalf("Apply(%v): want error, got nil", cfg)
		}
	}
	// Ints survive a yaml round-trip shape and a --set string.
	kctx := kernel.NewContext()
	kctx.Provide("llm", &scriptLLM{script: []string{"ok"}})
	kctx.Provide("codemode", codemode.New(time.Second))
	if err := (plugin{}).Apply(kctx, map[string]any{"max_spawns": "3", "max_steps": 2}); err != nil {
		t.Fatalf("Apply: %v", err)
	}
}

func TestChildMaxStepsIsAnError(t *testing.T) {
	// Every reply carries a block, so the child never finishes.
	_, cm, _, _, _ := mount(t, map[string]any{"max_steps": 2},
		"```js\nconsole.log('spin')\n```",
	)
	out, err := cm.Run(`try { tools.spawn("forever") } catch (e) { "THREW " + e }`)
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	if !strings.Contains(out, "THREW") || !strings.Contains(out, "gave up after 2 steps") {
		t.Fatalf("output = %q", out)
	}
}

// DefaultProject must ignore sub:* kinds: subagent transcript entries
// never reach the parent's model context.
func TestDefaultProjectIgnoresSubKinds(t *testing.T) {
	entries := []history.Entry{
		{Kind: "input", Data: map[string]any{"text": "hi"}},
		{Kind: "sub:assistant", Data: map[string]any{"text": "child says"}},
		{Kind: "sub:code", Data: map[string]any{"text": "code"}},
		{Kind: "sub:result", Data: map[string]any{"text": "out"}},
		{Kind: "sub:error", Data: map[string]any{"text": "boom"}},
		{Kind: "sub:done", Data: map[string]any{"text": ""}},
		{Kind: "result", Data: map[string]any{"text": "parent result"}},
	}
	msgs := loop.DefaultProject(entries)
	if len(msgs) != 2 {
		t.Fatalf("messages = %+v, want input + result only", msgs)
	}
	if msgs[0].Content != "hi" || !strings.Contains(msgs[1].Content, "parent result") {
		t.Fatalf("messages = %+v", msgs)
	}
}
