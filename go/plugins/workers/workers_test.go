package workers

import (
	"context"
	"os"
	"strings"
	"sync"
	"sync/atomic"
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
	seenSys string // the system prompt of the last call
}

func (s *scriptLLM) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.seenSys = system
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
	// The reply comes back under a provenance line naming the worker
	// and its task.
	for _, want := range []string{"BEFORE", "AFTER [subagent 1 · task: do the task]\nFINAL_FROM_CHILD"} {
		if !strings.Contains(out, want) {
			t.Fatalf("parent output missing %q: %q", want, out)
		}
	}
	if strings.Contains(out, "CHILD_TOOL_RAN") {
		t.Fatalf("child console output leaked into the parent's: %q", out)
	}

	wantKinds := []string{"sub:start", "sub:assistant", "sub:code", "sub:result", "sub:assistant", "sub:done"}
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
	if evs[3].Text != "CHILD_TOOL_RAN\n" {
		t.Fatalf("sub:result text = %q", evs[3].Text)
	}
	if evs[0].Text != "do the task" {
		t.Fatalf("sub:start text = %q, want the task", evs[0].Text)
	}
	if last := evs[len(evs)-1]; last.Data["status"] != "ok" || last.Data["steps"] != 2 {
		t.Fatalf("sub:done data = %v, want status ok, steps 2", last.Data)
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
	// The section carries the POLICY (when to delegate); the call
	// signatures come from the codemode catalogue, which is where
	// every other tool documents itself too.
	if !strings.Contains(secs.Text(), "when to delegate") || !strings.Contains(secs.Text(), "tools.spawnAll") {
		t.Fatalf("section = %q, want the delegation policy", secs.Text())
	}
	cm, err := kernel.Get[*codemode.CodeMode](kctx, "codemode")
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"tools.spawn(task)", "tools.spawnAll("} {
		if !strings.Contains(cm.Catalogue(), want) {
			t.Fatalf("catalogue lacks %q:\n%s", want, cm.Catalogue())
		}
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
	if !strings.HasSuffix(out, "\nchild gave up") {
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
	if strings.Count(out, "\nok") != 2 || !strings.Contains(out, "CAPPED") || !strings.Contains(out, "spawn limit reached (2 per turn)") {
		t.Fatalf("output = %q", out)
	}

	// The loop's turn-end "done" resets the counter.
	kctx.Emit("loop/event", loop.Event{Kind: "done"})
	out, err = cm.Run(`tools.spawn("four")`)
	if err != nil || !strings.HasSuffix(out, "\nok") {
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

// stubCode is a Codemode that never runs anything (the scripted child
// replies without a code block).
type stubCode struct{}

func (stubCode) RegisterTool(name string, fn any) {}
func (stubCode) Run(code string) (string, error)  { return "", nil }

type stubSections struct{ text string }

func (s stubSections) TextExcept(...string) string { return s.text }

func (s stubSections) Set(name, text string) {}
func (s stubSections) Text() string          { return s.text }

// The child runs in the parent's VM, so it is told the parent's tools:
// the loop's base prompt (bash/view/patch), the live sections (mcp,
// skills, ...), then its own identity. Not the bare one-liner it had.
func TestChildGetsTheParentsToolPrompt(t *testing.T) {
	l := &scriptLLM{script: []string{"done"}}
	// In production the loop puts the generated tool catalogue in the
	// sections, so the child is told the same tools as the parent.
	w := &Workers{llm: l, code: &stubCode{}, secs: stubSections{text: "Available in the runtime:\n- tools.bash(cmd) -> string: run it\n- tools.patch(path, old, new) -> string: replace one occurrence\n\n## mcp\nbough mcp call graphiti/..."}, ctx: context.Background(), maxSteps: 2}
	w.emit = func(kind, text string, data map[string]any) {}
	if _, err := w.runChild(context.Background(), "count files", 1, w.code.Run, false, nil); err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"tools.bash(cmd)", "tools.patch(path, old, new)", "bough mcp call graphiti/", SubSystemPrompt} {
		if !strings.Contains(l.seenSys, want) {
			t.Fatalf("child system prompt lacks %q:\n%s", want, l.seenSys)
		}
	}
	if strings.Index(l.seenSys, "tools.bash") > strings.Index(l.seenSys, "## mcp") || strings.Index(l.seenSys, "## mcp") > strings.Index(l.seenSys, SubSystemPrompt) {
		t.Fatalf("order must be base, sections, identity:\n%s", l.seenSys)
	}
	if got := systemFor("base", ""); !strings.HasPrefix(got, "base\n\n"+SubSystemPrompt) {
		t.Fatalf("no sections: %q", got)
	}
	// The child is told where it is: it invented absolute paths and then
	// reported findings about files that never existed.
	wd, _ := os.Getwd()
	if got := systemFor("base", ""); !strings.Contains(got, wd) {
		t.Fatalf("child system prompt must name the working directory: %q", got)
	}
}

// pausingCode records whether a child run happened inside a Pause: the
// parent's script deadline must not tick while a subagent works.
type pausingCode struct {
	paused, resumed int
	ranWhilePaused  bool
}

func (*pausingCode) RegisterTool(string, any) {}

func (p *pausingCode) Pause() func() {
	p.paused++
	return func() { p.resumed++ }
}

func (p *pausingCode) Run(string) (string, error) {
	if p.paused > p.resumed {
		p.ranWhilePaused = true
	}
	return "", nil
}

func TestSpawnPausesTheParentDeadline(t *testing.T) {
	pc := &pausingCode{}
	w := &Workers{
		llm:       &scriptLLM{script: []string{"on it\n```js\nnoop()\n```", "Status: ok\nFindings: done"}},
		code:      pc,
		maxSpawns: 4,
		maxSteps:  6,
		ctx:       context.Background(),
		emit:      func(string, string, map[string]any) {},
	}
	if _, err := w.spawn("look around"); err != nil {
		t.Fatalf("spawn: %v", err)
	}
	if pc.paused != 1 || pc.resumed != 1 {
		t.Fatalf("spawn must pause the parent's deadline exactly once: %d/%d", pc.paused, pc.resumed)
	}
	if !pc.ranWhilePaused {
		t.Fatal("the child's blocks must run while the parent's deadline is paused")
	}
}

// The child never sees the spawn advert: it read it, tried to delegate,
// and got "subagent depth 1 only" — twice in one real session.
func TestChildDoesNotSeeTheSpawnAdvert(t *testing.T) {
	secs := &loop.Sections{}
	secs.Set("workers", "Subagents: tools.spawn(task) -> string runs a child")
	secs.Set("tools", "## tools\ntools.bash(cmd)")
	l := &scriptLLM{script: []string{"Status: ok\nFindings: none"}}
	w := &Workers{llm: l, code: stubCode{}, secs: secs, maxSpawns: 4, maxSteps: 6,
		ctx: context.Background(), emit: func(string, string, map[string]any) {}}
	if _, err := w.spawn("look"); err != nil {
		t.Fatalf("spawn: %v", err)
	}
	if strings.Contains(l.seenSys, "tools.spawn(task)") {
		t.Fatalf("the child must not be told it can spawn:\n%s", l.seenSys)
	}
	if !strings.Contains(l.seenSys, "tools.bash(cmd)") {
		t.Fatalf("the child keeps every other section:\n%s", l.seenSys)
	}
}

// spawnAll runs its children at once and returns their reports in the
// order the tasks were given, whatever order they finish in.
func TestSpawnAllRunsChildrenConcurrently(t *testing.T) {
	var live, peak int32
	l := &gateLLM{live: &live, peak: &peak, reply: "Status: ok\nFindings: done"}
	w := &Workers{llm: l, code: stubCode{}, maxSpawns: 8, maxSteps: 6,
		ctx: context.Background(), emit: func(string, string, map[string]any) {}}
	tasks := []string{"alpha", "beta", "gamma"}
	got, err := w.spawnAll(tasks)
	if err != nil {
		t.Fatalf("spawnAll: %v", err)
	}
	if len(got) != 3 {
		t.Fatalf("want 3 reports, got %d", len(got))
	}
	for i, want := range []string{"subagent 1 · task: alpha", "subagent 2 · task: beta", "subagent 3 · task: gamma"} {
		// Reports are `any` now (a schema makes them objects); without
		// one they are the plain text they always were.
		text, ok := got[i].(string)
		if !ok || !strings.Contains(text, want) {
			t.Fatalf("report %d out of order: %#v", i, got[i])
		}
	}
	if atomic.LoadInt32(&peak) < 2 {
		t.Fatalf("children must overlap; peak concurrency was %d", peak)
	}
	if w.spawns != 3 {
		t.Fatalf("spawnAll must charge one slot per task, got %d", w.spawns)
	}
	if _, err := w.spawnAll(nil); err == nil {
		t.Fatal("an empty task list is an error")
	}
	w.spawns = 7
	if _, err := w.spawnAll([]string{"a", "b"}); err == nil {
		t.Fatal("spawnAll must refuse to overrun the per-turn budget")
	}
}

// gateLLM holds every call until all of them have arrived, so the test
// fails unless the children really do overlap.
type gateLLM struct {
	live, peak *int32
	reply      string
	mu         sync.Mutex
}

func (g *gateLLM) Complete(context.Context, string, []llm.Message) (string, error) {
	n := atomic.AddInt32(g.live, 1)
	g.mu.Lock()
	if n > atomic.LoadInt32(g.peak) {
		atomic.StoreInt32(g.peak, n)
	}
	g.mu.Unlock()
	time.Sleep(30 * time.Millisecond) // long enough for the others to arrive
	atomic.AddInt32(g.live, -1)
	return g.reply, nil
}

// spawnAll announces every child before any starts, so the cards read
// 1..N instead of in whichever order the goroutines got going.
func TestSpawnAllAnnouncesInTaskOrder(t *testing.T) {
	var mu sync.Mutex
	var starts []int
	w := &Workers{llm: &scriptLLM{script: []string{"Status: ok\nFindings: x"}}, code: stubCode{},
		maxSpawns: 8, maxSteps: 6, ctx: context.Background(),
		emit: func(kind, _ string, data map[string]any) {
			if kind != "sub:start" {
				return
			}
			mu.Lock()
			starts = append(starts, data["worker"].(int))
			mu.Unlock()
		}}
	if _, err := w.spawnAll([]string{"a", "b", "c"}); err != nil {
		t.Fatalf("spawnAll: %v", err)
	}
	if len(starts) != 3 || starts[0] != 1 || starts[1] != 2 || starts[2] != 3 {
		t.Fatalf("cards must be announced in task order, got %v", starts)
	}
}

// slowLLM blocks until its context is cancelled: a child waiting on
// the model is where a cancel almost always lands.
type slowLLM struct {
	started chan struct{}
	once    sync.Once
}

func (s *slowLLM) Complete(ctx context.Context, system string, messages []llm.Message) (string, error) {
	s.once.Do(func() { close(s.started) })
	<-ctx.Done()
	return "", ctx.Err()
}
func (s *slowLLM) Model() string { return "slow" }
func (s *slowLLM) Usage() llm.Usage {
	return llm.Usage{}
}

// Cancelling the turn kills the children with it: spawn returns the
// cancellation instead of hanging until the child's own step budget
// runs out. (Background jobs deliberately do NOT die — they hang off
// the plugin context, see plugins/tools.)
func TestCancelKillsChild(t *testing.T) {
	l := &slowLLM{started: make(chan struct{})}
	turn, cancel := context.WithCancel(context.Background())
	w := &Workers{llm: l, code: &stubCode{}, ctx: context.Background(),
		turn: func() context.Context { return turn }, maxSpawns: 4, maxSteps: 5}
	w.emit = func(kind, text string, data map[string]any) {}

	errCh := make(chan error, 1)
	go func() {
		_, err := w.spawn("a long task")
		errCh <- err
	}()
	<-l.started
	cancel()
	select {
	case err := <-errCh:
		if err == nil || !strings.Contains(err.Error(), "cancel") {
			t.Fatalf("spawn returned %v, want a cancellation", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("spawn did not return after the turn was cancelled")
	}
}

// The same for a batch: every child dies and spawnAll reports the
// cancellation rather than returning half-written reports as findings.
func TestCancelKillsSpawnAll(t *testing.T) {
	l := &slowLLM{started: make(chan struct{})}
	turn, cancel := context.WithCancel(context.Background())
	w := &Workers{llm: l, code: &stubCode{}, ctx: context.Background(),
		turn: func() context.Context { return turn }, maxSpawns: 8, maxSteps: 5}
	w.emit = func(kind, text string, data map[string]any) {}

	errCh := make(chan error, 1)
	go func() {
		_, err := w.spawnAll([]string{"one", "two", "three"})
		errCh <- err
	}()
	<-l.started
	cancel()
	select {
	case err := <-errCh:
		if err == nil || !strings.Contains(err.Error(), "cancel") {
			t.Fatalf("spawnAll returned %v, want a cancellation", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("spawnAll did not return after the turn was cancelled")
	}
}

// A child's block runs under the turn's context, not a background one:
// without this a subagent's tools.bash outlived the cancelled turn.
func TestChildBlocksRunUnderTheTurnContext(t *testing.T) {
	seen := make(chan context.Context, 1)
	code := &ctxStubCode{seen: seen}
	turn, cancel := context.WithCancel(context.Background())
	defer cancel()
	w := &Workers{llm: &scriptLLM{script: []string{"```js\nx\n```", "Status: ok"}},
		code: code, ctx: context.Background(),
		turn: func() context.Context { return turn }, maxSpawns: 4, maxSteps: 3}
	w.emit = func(kind, text string, data map[string]any) {}
	if _, err := w.spawn("do a thing"); err != nil {
		t.Fatalf("spawn: %v", err)
	}
	select {
	case got := <-seen:
		if got != turn {
			t.Fatalf("child block ran under %v, want the turn context", got)
		}
	default:
		t.Fatal("the child never ran a block")
	}
}

type ctxStubCode struct{ seen chan context.Context }

func (c *ctxStubCode) RegisterTool(name string, fn any) {}
func (c *ctxStubCode) Run(code string) (string, error)  { return "", nil }
func (c *ctxStubCode) RunCtx(ctx context.Context, code string) (string, error) {
	select {
	case c.seen <- ctx:
	default:
	}
	return "ok", nil
}

// With a schema the child's report is checked before it counts as
// finished, and comes back as a value the parent's program can index.
func TestSpawnWithSchemaReturnsAValue(t *testing.T) {
	shape := map[string]any{
		"type":     "object",
		"required": []any{"status", "files"},
		"properties": map[string]any{
			"status": map[string]any{"type": "string", "enum": []any{"ok", "failed"}},
			"files":  map[string]any{"type": "array", "items": map[string]any{"type": "string"}},
		},
	}
	l := &scriptLLM{script: []string{
		"```stop\n{\"status\":\"maybe\",\"files\":\"one.go\"}\n```", // wrong enum, wrong type
		"```stop\n{\"status\":\"ok\",\"files\":[\"one.go\"]}\n```",
	}}
	w := &Workers{llm: l, code: &stubCode{}, ctx: context.Background(), maxSpawns: 4, maxSteps: 5}
	w.emit = func(kind, text string, data map[string]any) {}

	got, err := w.spawn("look at one.go", shape)
	if err != nil {
		t.Fatalf("spawn: %v", err)
	}
	m, ok := got.(map[string]any)
	if !ok {
		t.Fatalf("want a parsed object, got %#v", got)
	}
	if m["status"] != "ok" {
		t.Fatalf("report = %#v", m)
	}
	files, _ := m["files"].([]any)
	if len(files) != 1 || files[0] != "one.go" {
		t.Fatalf("files = %#v", m["files"])
	}
	if l.calls != 2 {
		t.Fatalf("the mismatched report was accepted (%d calls)", l.calls)
	}
	// The child was told the shape, and told its mistakes.
	if !strings.Contains(l.seenSys, `"enum"`) {
		t.Fatalf("the schema never reached the child:\n%s", l.seenSys)
	}
	var sawIssues bool
	for _, m := range l.seenMsg {
		if strings.Contains(m.Content, "does not match the schema") &&
			strings.Contains(m.Content, "must be one of") {
			sawIssues = true
		}
	}
	if !sawIssues {
		t.Fatalf("the mismatches were not handed back: %+v", l.seenMsg)
	}
}

// A child that never matches the schema fails its run rather than
// returning something the parent would have to parse anyway.
func TestSpawnWithSchemaGivesUpCleanly(t *testing.T) {
	shape := map[string]any{"type": "object", "required": []any{"n"}}
	l := &scriptLLM{script: []string{"```stop\nnot json at all\n```"}}
	w := &Workers{llm: l, code: &stubCode{}, ctx: context.Background(), maxSpawns: 4, maxSteps: 2}
	w.emit = func(kind, text string, data map[string]any) {}
	got, err := w.spawn("count things", shape)
	if err == nil {
		t.Fatalf("want an error, got %#v", got)
	}
	if !strings.Contains(err.Error(), "gave up") {
		t.Fatalf("err = %v", err)
	}
}
