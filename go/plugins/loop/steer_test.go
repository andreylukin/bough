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
// that ticks on every done event.
func mountSteer(t *testing.T, llm LLM, code Codemode) (steer func(string) bool, inputs chan string, kinds func() []string, turnDone chan struct{}) {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", code)
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

// A steer sent while the model writes its final reply (no boundary
// left in the turn) is not lost: it runs as the next turn's input.
func TestSteerDuringFinalReplyBecomesNextTurn(t *testing.T) {
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
	waitDone(t, done, kinds) // the first turn
	waitDone(t, done, kinds) // the late steer's turn
	want := []string{"assistant", "done", "steer", "assistant", "done"}
	if got := kinds(); strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("events = %v, want %v", got, want)
	}
	llm.mu.Lock()
	defer llm.mu.Unlock()
	msgs := llm.seen[0]
	if last := msgs[len(msgs)-1]; last.Role != "user" || last.Content != "and then this" {
		t.Fatalf("next turn's last message = %+v, want the late steer", last)
	}
	if steer("idle again") {
		t.Fatal("steer must be false once every turn is done")
	}
}
