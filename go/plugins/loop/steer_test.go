package loop

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// steerLLM replies with two js blocks first (or, with hook set, runs
// it and replies plain text), then records what every later call saw
// and replies with plain text, ending the turn.
type steerLLM struct {
	mu    sync.Mutex
	calls int
	seen  [][]Message // messages of every call after the first
	// hook, when set, runs inside the first completion: a steer sent
	// while the model is still writing its final reply.
	hook func()
}

func (s *steerLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	if s.calls == 1 {
		if s.hook != nil {
			s.hook()
			return "plain reply, no code", nil
		}
		return "two blocks:\n```js\nconsole.log('ONE')\n```\nand\n```js\nconsole.log('TWO')\n```", nil
	}
	s.seen = append(s.seen, messages)
	return "steered reply", nil
}

// steerCode steers the running turn from inside the first block.
type steerCode struct {
	stubCode
	steer func(string) bool
	ok    bool
}

func (c *steerCode) Run(code string) (string, error) {
	if len(c.ran) == 0 {
		c.ok = c.steer("use B instead")
	}
	return c.stubCode.Run(code)
}

// mountSteer mounts the loop over llm/code and returns the steer
// service, the inputs channel, an event-kind recorder and a channel
// that ticks on every done event. provide adds optional seams (hooks,
// skills) before the mount.
func mountSteer(t *testing.T, llm LLM, code Codemode, provide ...func(*kernel.Context)) (steer func(string) bool, inputs chan string, kinds func() []string, turnDone chan struct{}) {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", code)
	for _, p := range provide {
		p(kctx)
	}
	var mu sync.Mutex
	var ks []string
	turnDone = make(chan struct{}, 4)
	kctx.On("loop/event", func(p any) {
		ev := p.(Event)
		mu.Lock()
		ks = append(ks, ev.Kind)
		mu.Unlock()
		if ev.Kind == "done" {
			turnDone <- struct{}{}
		}
	})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	steer, err := kernel.Get[func(string) bool](kctx, "steer")
	if err != nil {
		t.Fatalf("steer service: %v", err)
	}
	inputs, _ = kernel.Get[chan string](kctx, "inputs")
	kinds = func() []string {
		mu.Lock()
		defer mu.Unlock()
		return append([]string(nil), ks...)
	}
	return steer, inputs, kinds, turnDone
}

func waitDone(t *testing.T, ch chan struct{}, kinds func() []string) {
	t.Helper()
	select {
	case <-ch:
	case <-time.After(3 * time.Second):
		t.Fatalf("no done event; events %v", kinds())
	}
}

// A steer sent while block one runs lands at the boundary after it:
// block two never runs, the steer is a user message in the next
// model call, and a "steer" event tells the UI.
func TestSteerBetweenBlocksDropsRestOfReply(t *testing.T) {
	t.Parallel()
	llm := &steerLLM{}
	code := &steerCode{}
	steer, inputs, kinds, done := mountSteer(t, llm, code)
	code.steer = steer
	if steer("too early") {
		t.Fatal("steer must be false while idle")
	}
	inputs <- "go"
	waitDone(t, done, kinds)

	if !code.ok {
		t.Fatal("steer returned false during a running turn")
	}
	if len(code.ran) != 1 || !strings.Contains(code.ran[0], "ONE") {
		t.Fatalf("codemode ran %q, want only block ONE", code.ran)
	}
	want := []string{"assistant", "code", "result", "steer", "assistant", "done"}
	if got := kinds(); strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("events = %v, want %v", got, want)
	}
	llm.mu.Lock()
	defer llm.mu.Unlock()
	if len(llm.seen) != 1 {
		t.Fatalf("model calls after the steer = %d, want 1", len(llm.seen))
	}
	msgs := llm.seen[0]
	last := msgs[len(msgs)-1]
	if last.Role != "user" || last.Content != "use B instead" {
		t.Fatalf("last message = %+v, want the steer as a user message", last)
	}
	if strings.Contains(msgs[len(msgs)-2].Content, "TWO") {
		t.Fatalf("block TWO leaked into context: %+v", msgs)
	}
}

// A steer sent while the model writes its final reply (the turn's
// longest phase) lands inside the turn: the model is asked again
// before the one and only done.
func TestSteerDuringFinalReplyLandsBeforeDone(t *testing.T) {
	t.Parallel()
	llm := &steerLLM{}
	var steer func(string) bool
	llm.hook = func() {
		if !steer("and then this") {
			t.Error("steer returned false mid-completion")
		}
	}
	var inputs chan string
	var kinds func() []string
	var done chan struct{}
	steer, inputs, kinds, done = mountSteer(t, llm, &stubCode{})
	inputs <- "go"
	waitDone(t, done, kinds)
	want := []string{"assistant", "steer", "assistant", "done"}
	if got := kinds(); strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("events = %v, want %v", got, want)
	}
	llm.mu.Lock()
	defer llm.mu.Unlock()
	msgs := llm.seen[0]
	if last := msgs[len(msgs)-1]; last.Role != "user" || last.Content != "and then this" {
		t.Fatalf("second call's last message = %+v, want the steer", last)
	}
	if steer("idle again") {
		t.Fatal("steer must be false once the turn is done")
	}
}

// inputHooks answers user-prompt-submit per input text: a canned
// result for one input, nothing for the rest.
type inputHooks struct {
	input  string
	result map[string]any
}

func (h *inputHooks) Fire(ctx context.Context, event string, payload map[string]any) (map[string]any, error) {
	if event == "user-prompt-submit" && payload["input"] == h.input {
		return h.result, nil
	}
	return nil, nil
}

// A steer is admitted like any input: the user-prompt-submit hook
// may rewrite it, skills inject, and the entry is marked a steer.
func TestSteerGoesThroughHookAndSkills(t *testing.T) {
	t.Parallel()
	llm := &steerLLM{}
	code := &steerCode{}
	hist := &memHistory{}
	steer, inputs, kinds, done := mountSteer(t, llm, code, func(kctx *kernel.Context) {
		kctx.Provide("hooks", &inputHooks{input: "use B instead", result: map[string]any{"input": "use C instead"}})
		kctx.Provide("skills", &stubSkills{blocks: []string{"[skill: s]\nspin"}})
		kctx.Provide("history", hist)
	})
	code.steer = steer
	inputs <- "go"
	waitDone(t, done, kinds)
	llm.mu.Lock()
	defer llm.mu.Unlock()
	msgs := llm.seen[0]
	if last := msgs[len(msgs)-1]; last.Role != "user" || last.Content != "use C instead\n\n[skill: s]\nspin" {
		t.Fatalf("last message = %+v, want the rewritten steer with its skill block", last)
	}
	var steers int
	for _, e := range hist.Entries() {
		if e.Kind == "input" && e.Data["steer"] == true {
			steers++
		}
	}
	if steers != 1 {
		t.Fatalf("history marks %d steer inputs, want 1: %v", steers, hist.Entries())
	}
}

// A steer the hook blocks never reaches the model: the turn carries on
// as if nothing was sent, the reason shows as an error, and the ui
// still gets its "steer" event (the row stops pending). Only the
// reply's first block ever runs, so the turn goes back to the model
// after it.
func TestSteerBlockedByHook(t *testing.T) {
	t.Parallel()
	llm := &steerLLM{}
	code := &steerCode{}
	steer, inputs, kinds, done := mountSteer(t, llm, code, func(kctx *kernel.Context) {
		kctx.Provide("hooks", &inputHooks{input: "use B instead", result: map[string]any{"block": "policy-says-no"}})
	})
	code.steer = steer
	inputs <- "go"
	waitDone(t, done, kinds)
	want := []string{"assistant", "code", "result", "steer", "error", "assistant", "done"}
	if got := kinds(); strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("events = %v, want %v", got, want)
	}
	if len(code.ran) != 1 {
		t.Fatalf("codemode ran %d blocks, want 1 (only the first runs)", len(code.ran))
	}
	llm.mu.Lock()
	defer llm.mu.Unlock()
	for _, m := range llm.seen[0] {
		if strings.Contains(m.Content, "use B instead") {
			t.Fatalf("blocked steer reached the model: %+v", m)
		}
	}
}

// Esc with a steer still pending: the steer is recorded under the
// [cancelled] note (the next turn sees it, nothing runs on its own)
// and the turn ends with one done.
func TestCancelWithPendingSteerRecordsItAndStops(t *testing.T) {
	t.Parallel()
	kctx := kernel.NewContext()
	kctx.Provide("llm", slowLLM{})
	kctx.Provide("codemode", &stubCode{})
	var mu sync.Mutex
	var kinds []string
	done := make(chan struct{}, 2)
	kctx.On("loop/event", func(p any) {
		ev := p.(Event)
		mu.Lock()
		kinds = append(kinds, ev.Kind)
		mu.Unlock()
		if ev.Kind == "done" {
			done <- struct{}{}
		}
	})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	inputs, _ := kernel.Get[chan string](kctx, "inputs")
	cancel, _ := kernel.Get[func()](kctx, "cancel")
	steer, _ := kernel.Get[func(string) bool](kctx, "steer")
	inputs <- "go"
	time.Sleep(50 * time.Millisecond) // let the turn reach Complete
	if !steer("stop doing X") {
		t.Fatal("steer refused during the model call")
	}
	cancel()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatalf("turn did not end after cancel; events %v", kinds)
	}
	select {
	case <-done:
		t.Fatal("the pending steer ran as a turn of its own")
	case <-time.After(100 * time.Millisecond):
	}
	mu.Lock()
	got := strings.Join(kinds, " ")
	mu.Unlock()
	if got != "steer cancelled done" {
		t.Fatalf("events = %q, want steer cancelled done", got)
	}
	r, _ := kernel.Get[*runner](kctx, "runner")
	msgs := r.project()
	if last := msgs[len(msgs)-1]; last.Role != "user" || !strings.HasPrefix(last.Content, "stop doing X\n\n[cancelled]") {
		t.Fatalf("last message = %+v, want the steer under the cancelled note", last)
	}
	if steer("idle") {
		t.Fatal("steer must be false once the turn is done")
	}
}
