package workers

// Failure paths of tools.spawn / tools.spawnAll, checked from three
// sides: what the parent's program gets back (its "model feed"), what
// the transcript cards say (sub:* events), and that the plugin is still
// usable afterwards (inChild reset, spawn budget sane, no hang).
//
// Known product bugs are gated: set BOUGH_KNOWN_TOOLS_SUBAGENTS=1 to run
// them.

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

func subagentsKnownBug(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_TOOLS_SUBAGENTS") != "1" {
		t.Skip("known bug (BOUGH_KNOWN_TOOLS_SUBAGENTS=1 to run): " + bug)
	}
}

// subagentsTaskLLM routes each child by its task (the first message):
// spawnAll's children share one llm, so a per-task tape keeps the order
// deterministic. A reply "ERR:<msg>" is returned as an llm error;
// "HANG" blocks until the context ends.
type subagentsTaskLLM struct {
	mu      sync.Mutex
	tapes   map[string][]string
	pos     map[string]int
	started chan string
}

func (s *subagentsTaskLLM) Complete(ctx context.Context, system string, msgs []llm.Message) (string, error) {
	task := msgs[0].Content
	s.mu.Lock()
	tape := s.tapes[task]
	i := s.pos[task]
	s.pos[task]++
	s.mu.Unlock()
	if len(tape) == 0 {
		return "", fmt.Errorf("no tape for task %q", task)
	}
	if i >= len(tape) {
		i = len(tape) - 1
	}
	r := tape[i]
	if r == "HANG" {
		if s.started != nil {
			s.started <- task
		}
		<-ctx.Done()
		return "", ctx.Err()
	}
	if e, ok := strings.CutPrefix(r, "ERR:"); ok {
		return "", fmt.Errorf("%s", e)
	}
	return r, nil
}
func (s *subagentsTaskLLM) Model() string    { return "tape" }
func (s *subagentsTaskLLM) Usage() llm.Usage { return llm.Usage{} }

type subagentsRig struct {
	kctx *kernel.Context
	cm   *codemode.CodeMode
	mu   sync.Mutex
	evs  []loop.Event
}

func (r *subagentsRig) events() []loop.Event {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]loop.Event(nil), r.evs...)
}

// done returns the "status" of worker id's last sub:done card and how
// many it has.
func (r *subagentsRig) done(id int) (string, int) {
	n, st := 0, ""
	for _, ev := range r.events() {
		if ev.Kind == "sub:done" && ev.Data["worker"] == id {
			n++
			st, _ = ev.Data["status"].(string)
		}
	}
	return st, n
}

// subagentsMount wires the real codemode (with its own timeout) and the
// real workers plugin around l.
func subagentsMount(t *testing.T, timeout time.Duration, cfg map[string]any, l llm.LLM) *subagentsRig {
	t.Helper()
	r := &subagentsRig{kctx: kernel.NewContext(), cm: codemode.New(timeout)}
	r.kctx.Provide("llm", l)
	r.kctx.Provide("codemode", r.cm)
	r.kctx.On("loop/event", func(p any) {
		if ev, ok := p.(loop.Event); ok {
			r.mu.Lock()
			r.evs = append(r.evs, ev)
			r.mu.Unlock()
		}
	})
	if err := (plugin{}).Apply(r.kctx, cfg); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	return r
}

func subagentsTape(tapes map[string][]string) *subagentsTaskLLM {
	return &subagentsTaskLLM{tapes: tapes, pos: map[string]int{}}
}

// subagentsAlive: after whatever happened, a fresh spawn in a new turn
// still works (inChild reset, no stuck lock).
func subagentsAlive(t *testing.T, r *subagentsRig, l *subagentsTaskLLM) {
	t.Helper()
	l.mu.Lock()
	l.tapes["alive?"] = []string{"Status: ok\nALIVE"}
	l.mu.Unlock()
	r.kctx.Emit("loop/event", loop.Event{Kind: "done"})
	out, err := r.cm.Run(`tools.spawn("alive?")`)
	if err != nil || !strings.Contains(out, "ALIVE") {
		t.Fatalf("plugin unusable afterwards: %q, %v", out, err)
	}
}

func subagentsHasEvent(r *subagentsRig, kind, sub string) bool {
	for _, ev := range r.events() {
		if ev.Kind == kind && strings.Contains(ev.Text, sub) {
			return true
		}
	}
	return false
}

func TestSubagentsChildBlockThrows(t *testing.T) {
	l := subagentsTape(map[string][]string{"t": {
		"```js\nthrow new Error('KABOOM')\n```",
		"Status: ok\nall good",
	}})
	r := subagentsMount(t, 5*time.Second, nil, l)
	out, err := r.cm.Run(`tools.spawn("t")`)
	if err != nil {
		t.Fatalf("parent block died: %v", err)
	}
	if !strings.Contains(out, "all good") {
		t.Fatalf("parent got %q", out)
	}
	if !subagentsHasEvent(r, "sub:error", "KABOOM") {
		t.Fatalf("no sub:error card with the throw: %+v", r.events())
	}
	// Every block failed, so the child's "ok" is overruled.
	if st, n := r.done(1); st != "failed" || n != 1 {
		t.Fatalf("done card = %q x%d, want failed x1", st, n)
	}
	subagentsAlive(t, r, l)
}

func TestSubagentsChildSyntaxError(t *testing.T) {
	l := subagentsTape(map[string][]string{"t": {"```js\nfunction (\n```", "Status: failed\nno"}})
	r := subagentsMount(t, 5*time.Second, nil, l)
	out, err := r.cm.Run(`tools.spawn("t")`)
	if err != nil || !strings.Contains(out, "Status: failed") {
		t.Fatalf("got %q, %v", out, err)
	}
	if !subagentsHasEvent(r, "sub:error", "SyntaxError") {
		t.Fatalf("no SyntaxError card: %+v", r.events())
	}
	if st, _ := r.done(1); st != "failed" {
		t.Fatalf("done = %q", st)
	}
	subagentsAlive(t, r, l)
}

func TestSubagentsChildInfiniteLoopTimesOut(t *testing.T) {
	l := subagentsTape(map[string][]string{"t": {"```js\nwhile(true){}\n```", "Status: failed\nlooped"}})
	r := subagentsMount(t, 300*time.Millisecond, nil, l)
	done := make(chan struct{})
	var out string
	var err error
	go func() { out, err = r.cm.Run(`tools.spawn("t")`); close(done) }()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("child's infinite loop hung the parent")
	}
	if err != nil || !strings.Contains(out, "looped") {
		t.Fatalf("got %q, %v", out, err)
	}
	if !subagentsHasEvent(r, "sub:error", "timeout") {
		t.Fatalf("child never saw the timeout: %+v", r.events())
	}
	subagentsAlive(t, r, l)
}

func TestSubagentsMaxStepsCard(t *testing.T) {
	l := subagentsTape(map[string][]string{"t": {"```js\n1\n```"}})
	r := subagentsMount(t, 5*time.Second, map[string]any{"max_steps": 3}, l)
	out, err := r.cm.Run(`try { tools.spawn("t") } catch (e) { "THREW " + e }`)
	if err != nil || !strings.Contains(out, "gave up after 3 steps") {
		t.Fatalf("got %q, %v", out, err)
	}
	if st, n := r.done(1); st != "error" || n != 1 {
		t.Fatalf("done = %q x%d", st, n)
	}
	subagentsAlive(t, r, l)
}

func TestSubagentsSpawnAllOverCapLeavesBudget(t *testing.T) {
	l := subagentsTape(map[string][]string{"a": {"A"}, "b": {"B"}})
	r := subagentsMount(t, 5*time.Second, map[string]any{"max_spawns": 2}, l)
	out, err := r.cm.Run(`var m=""; try { tools.spawnAll(["a","b","a"]) } catch(e) { m = "REFUSED " + e }
		m + "|" + tools.spawnAll(["a","b"]).join("|")`)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "REFUSED") || !strings.Contains(out, "only 2 spawn(s) left") ||
		!strings.Contains(out, "\nA") || !strings.Contains(out, "\nB") {
		t.Fatalf("got %q", out)
	}
	out, _ = r.cm.Run(`try { tools.spawn("a") } catch(e) { "CAP " + e }`)
	if !strings.Contains(out, "spawn limit reached") {
		t.Fatalf("third spawn = %q", out)
	}
}

func TestSubagentsSpawnAllOneOfNFails(t *testing.T) {
	l := subagentsTape(map[string][]string{
		"alpha": {"Status: ok\nALPHA"},
		"beta":  {"ERR:provider 500"},
		"gamma": {"```js\n1\n```"}, // never finishes: max_steps
		"delta": {"Status: ok\nDELTA"},
	})
	r := subagentsMount(t, 5*time.Second, map[string]any{"max_steps": 2}, l)
	out, err := r.cm.Run(`tools.spawnAll(["alpha","beta","gamma","delta"]).join("\n=====\n")`)
	if err != nil {
		t.Fatalf("one failing child killed the batch: %v", err)
	}
	parts := strings.Split(out, "\n=====\n")
	if len(parts) != 4 {
		t.Fatalf("want 4 reports, got %d: %q", len(parts), out)
	}
	if !strings.Contains(parts[0], "ALPHA") || !strings.Contains(parts[3], "DELTA") {
		t.Fatalf("siblings lost: %q", out)
	}
	if !strings.Contains(parts[1], "Status: failed") || !strings.Contains(parts[1], "provider 500") {
		t.Fatalf("beta report = %q", parts[1])
	}
	if !strings.Contains(parts[2], "Status: failed") || !strings.Contains(parts[2], "gave up after 2 steps") {
		t.Fatalf("gamma report = %q", parts[2])
	}
	for id, want := range map[int]string{1: "ok", 2: "error", 3: "error", 4: "ok"} {
		if st, n := r.done(id); st != want || n != 1 {
			t.Fatalf("worker %d done = %q x%d, want %q", id, st, n, want)
		}
	}
	subagentsAlive(t, r, l)
}

func TestSubagentsChildCallsSpawnAll(t *testing.T) {
	l := subagentsTape(map[string][]string{"outer": {
		"```js\ntools.spawnAll(['x','y'])\n```", "Status: failed\ncould not nest"}})
	r := subagentsMount(t, 5*time.Second, nil, l)
	out, err := r.cm.Run(`tools.spawn("outer")`)
	if err != nil || !strings.Contains(out, "could not nest") {
		t.Fatalf("got %q, %v", out, err)
	}
	if !subagentsHasEvent(r, "sub:error", "depth 1 only") {
		t.Fatal("nested spawnAll not refused")
	}
	for _, ev := range r.events() {
		if ev.Kind == "sub:start" && ev.Data["worker"] != 1 {
			t.Fatalf("a nested child started: %+v", ev)
		}
	}
	subagentsAlive(t, r, l)
}

func TestSubagentsBackgroundChildCannotSpawnAll(t *testing.T) {
	subagentsKnownBug(t, "spawnAll lacks spawn's w.bg>0 guard (plugins/workers/workers.go spawnAll): a background child's block can fan out children, breaking depth one")
	l := subagentsTape(map[string][]string{
		"bg":  {"```js\ntools.spawnAll(['sub'])\n```", "Status: ok\ndone"},
		"sub": {"NESTED_RAN"},
	})
	r := subagentsMount(t, 5*time.Second, nil, l)
	bg, err := kernel.Get[func(context.Context, string, map[string]any, func(string, string)) (any, error)](r.kctx, "spawn-background")
	if err != nil {
		t.Fatal(err)
	}
	var mu sync.Mutex
	var steps []string
	if _, err := bg(context.Background(), "bg", nil, func(k, txt string) { mu.Lock(); steps = append(steps, k+": "+txt); mu.Unlock() }); err != nil {
		t.Fatal(err)
	}
	for _, s := range steps {
		if strings.Contains(s, "NESTED_RAN") {
			t.Fatalf("a background child spawned a grandchild: %q", steps)
		}
	}
}

func TestSubagentsEmptyTask(t *testing.T) {
	subagentsKnownBug(t, `spawn("")/spawnAll([""]) are not validated (plugins/workers/workers.go spawn/spawnAll): the child is sent an empty user message, which real providers reject`)
	l := subagentsTape(map[string][]string{"": {"CHILD_RAN_ON_EMPTY"}, "   ": {"CHILD_RAN_ON_EMPTY"}})
	r := subagentsMount(t, 5*time.Second, nil, l)
	for _, js := range []string{`tools.spawn("")`, `tools.spawn("   ")`, `tools.spawnAll([""])`} {
		out, err := r.cm.Run(`try { ` + js + ` } catch(e) { "THREW " + e }`)
		if err != nil || !strings.Contains(out, "THREW") || strings.Contains(out, "CHILD_RAN") {
			t.Fatalf("%s = %q, %v; want a thrown refusal", js, out, err)
		}
	}
}

func TestSubagentsEnormousTask(t *testing.T) {
	task := strings.Repeat("word ", 400_000) // ~2 MB, one line
	l := subagentsTape(map[string][]string{task: {"Status: ok\nBIG"}})
	r := subagentsMount(t, 5*time.Second, nil, l)
	r.cm.RegisterTool("bigTask", func() string { return task })
	out, err := r.cm.Run(`tools.spawn(tools.bigTask())`)
	if err != nil || !strings.Contains(out, "BIG") {
		t.Fatalf("got %.200q, %v", out, err)
	}
	head, _, _ := strings.Cut(out, "\n")
	if len(head) > 300 {
		t.Fatalf("provenance line not cut: %d bytes", len(head))
	}
}

func TestSubagentsEmptyFinalReply(t *testing.T) {
	subagentsKnownBug(t, "a child whose final reply is empty (or only an empty stop fence) is carded ok and returns an empty body (plugins/workers/workers.go runChildTo, the len(blocks)==0 branch) — the parent gets a bare provenance line and no hint the child produced nothing")
	l := subagentsTape(map[string][]string{"t": {""}, "u": {"```stop\n```"}})
	r := subagentsMount(t, 5*time.Second, nil, l)
	for i, task := range []string{"t", "u"} {
		out, err := r.cm.Run(`tools.spawn("` + task + `")`)
		if err != nil {
			t.Fatal(err)
		}
		_, body, _ := strings.Cut(out, "\n")
		if strings.TrimSpace(body) == "" {
			t.Fatalf("task %q: parent got an empty report %q", task, out)
		}
		if st, _ := r.done(i + 1); st == "ok" {
			t.Fatalf("task %q: empty report carded ok", task)
		}
	}
}

func TestSubagentsHangCancelledSpawnCard(t *testing.T) {
	l := subagentsTape(map[string][]string{"t": {"HANG"}})
	l.started = make(chan string, 1)
	r := subagentsMount(t, 5*time.Second, nil, l)
	turn, cancel := context.WithCancel(context.Background())
	errc := make(chan error, 1)
	go func() {
		_, err := r.cm.RunCtx(turn, `tools.spawn("t")`)
		errc <- err
	}()
	<-l.started
	cancel()
	select {
	case err := <-errc:
		if err == nil || !strings.Contains(err.Error(), "cancel") {
			t.Fatalf("parent block = %v; want a cancellation", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("esc did not stop a stalled child")
	}
	if _, n := r.done(1); n != 1 {
		t.Fatalf("worker 1 has %d done cards", n)
	}
	t.Run("card says cancelled", func(t *testing.T) {
		subagentsKnownBug(t, `a child cancelled mid-llm-call is carded status "error" with a sub:error "context canceled" (plugins/workers/workers.go runChildTo llm-error branch never checks ctx.Err()); spawn also refunds the slot as if the provider failed`)
		if st, _ := r.done(1); st != "cancelled" {
			t.Fatalf("done card = %q, want cancelled", st)
		}
	})
	subagentsAlive(t, r, l)
}

func TestSubagentsParentCancelledDuringSpawnAll(t *testing.T) {
	l := subagentsTape(map[string][]string{
		"fast": {"Status: ok\nFAST"},
		"h1":   {"HANG"},
		"h2":   {"HANG"},
	})
	l.started = make(chan string, 2)
	r := subagentsMount(t, 5*time.Second, nil, l)
	turn, cancel := context.WithCancel(context.Background())
	errc := make(chan error, 1)
	go func() {
		_, err := r.cm.RunCtx(turn, `tools.spawnAll(["fast","h1","h2"])`)
		errc <- err
	}()
	<-l.started
	<-l.started
	// "fast" may still be finishing; give it its card first.
	deadline := time.Now().Add(5 * time.Second)
	for {
		if _, n := r.done(1); n == 1 || time.Now().After(deadline) {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	cancel()
	select {
	case err := <-errc:
		if err == nil || !strings.Contains(err.Error(), "cancel") {
			t.Fatalf("spawnAll = %v, want cancellation", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("spawnAll hung after cancel")
	}
	for id := 1; id <= 3; id++ {
		if _, n := r.done(id); n != 1 {
			t.Fatalf("worker %d has %d done cards", id, n)
		}
	}
	if st, _ := r.done(1); st != "ok" {
		t.Fatalf("finished sibling carded %q", st)
	}
	t.Run("hung children carded cancelled", func(t *testing.T) {
		subagentsKnownBug(t, `children cancelled mid-llm-call are carded "error", not "cancelled" (plugins/workers/workers.go runChildTo llm-error branch ignores ctx.Err())`)
		for id := 2; id <= 3; id++ {
			if st, _ := r.done(id); st != "cancelled" {
				t.Fatalf("worker %d done = %q, want cancelled", id, st)
			}
		}
	})
	subagentsAlive(t, r, l)
}
