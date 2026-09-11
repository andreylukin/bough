package loop

// Steering, deep: several steers per boundary, steers during a
// background-job wake turn, steer then cancel, and the finish-line
// race (a steer the loop refuses must become input, never vanish).

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// steerDeepLLM replies from a script (endTurn-wrapped unless it holds
// a fence), runs hook(i) inside call i, and records every call's
// messages.
type steerDeepLLM struct {
	mu      sync.Mutex
	replies []string
	hook    func(call int)
	seen    [][]Message
}

func (s *steerDeepLLM) Complete(ctx context.Context, _ string, messages []Message) (string, error) {
	s.mu.Lock()
	i := len(s.seen)
	s.seen = append(s.seen, append([]Message(nil), messages...))
	hook := s.hook
	s.mu.Unlock()
	if hook != nil {
		hook(i)
	}
	if i >= len(s.replies) {
		i = len(s.replies) - 1
	}
	return endTurn(s.replies[i]), nil
}

func (s *steerDeepLLM) calls() [][]Message {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([][]Message(nil), s.seen...)
}

// steerDeepCount counts user messages whose content starts with text.
func steerDeepCount(msgs []Message, text string) int {
	n := 0
	for _, m := range msgs {
		if m.Role == "user" && strings.HasPrefix(m.Content, text) {
			n++
		}
	}
	return n
}

// steerDeepOrder asserts every text is present once, in order, as a
// user message.
func steerDeepOrder(t *testing.T, msgs []Message, texts []string) {
	t.Helper()
	last := -1
	for _, want := range texts {
		if n := steerDeepCount(msgs, want); n != 1 {
			t.Fatalf("steer %q reached the model %d times, want 1: %+v", want, n, msgs)
		}
		for i, m := range msgs {
			if m.Role == "user" && strings.HasPrefix(m.Content, want) {
				if i <= last {
					t.Fatalf("steer %q out of order: %+v", want, msgs)
				}
				last = i
			}
		}
	}
}

// steerDeepCode steers from inside the first block it runs.
type steerDeepCode struct {
	stubCode
	onFirst func()
}

func (c *steerDeepCode) Run(code string) (string, error) {
	if len(c.ran) == 0 && c.onFirst != nil {
		c.onFirst()
	}
	return c.stubCode.Run(code)
}

func steerDeepSteers(n int) []string {
	s := make([]string, n)
	for i := range s {
		s[i] = fmt.Sprintf("steer-%d please", i+1)
	}
	return s
}

// steerDeepFilter keeps the entry kinds the ordering cares about.
func steerDeepFilter(seq []string) []string {
	var out []string
	for _, s := range seq {
		k, _, _ := strings.Cut(s, ":")
		switch k {
		case "input", "assistant", "code", "result", "done", "cancelled":
			out = append(out, s)
		}
	}
	return out
}

// Two and five steers sent while one block runs land at the same
// boundary, in order, each once — in the call right after AND in
// every later call of the turn (the projection must not duplicate).
// History records them between the block's result and the next reply.
func TestSteerDeepManyDuringBlock(t *testing.T) {
	t.Parallel()
	for _, n := range []int{2, 5} {
		t.Run(fmt.Sprint(n), func(t *testing.T) {
			t.Parallel()
			llm := &steerDeepLLM{replies: []string{
				"```js\nconsole.log('ONE')\n```\n```js\nconsole.log('TWO')\n```",
				"```js\nconsole.log('THREE')\n```",
				"all done",
			}}
			code := &steerDeepCode{}
			hist := &memHistory{}
			steer, inputs, kinds, done := mountSteer(t, llm, code, func(k *kernel.Context) { k.Provide("history", History(hist)) })
			texts := steerDeepSteers(n)
			code.onFirst = func() {
				for _, s := range texts {
					if !steer(s) {
						t.Errorf("steer %q refused mid-block", s)
					}
				}
			}
			inputs <- "go"
			waitDone(t, done, kinds)
			calls := llm.calls()
			if len(calls) != 3 {
				t.Fatalf("model calls = %d, want 3", len(calls))
			}
			steerDeepOrder(t, calls[1], texts)
			steerDeepOrder(t, calls[2], texts)
			if got := strings.Count(strings.Join(kinds(), " "), "steer"); got != n {
				t.Fatalf("steer events = %d, want %d: %v", got, n, kinds())
			}
			if len(code.ran) != 2 || strings.Contains(strings.Join(code.ran, ""), "TWO") {
				t.Fatalf("ran %q, want ONE then THREE", code.ran)
			}
			var seq []string
			for _, e := range hist.Entries() {
				s := e.Kind
				if e.Kind == "input" {
					s += ":" + fmt.Sprint(e.Data["text"])
					if e.Data["steer"] == true {
						s += ":steer"
					}
				}
				seq = append(seq, s)
			}
			want := []string{"input:go", "assistant", "code", "result"}
			for _, s := range texts {
				want = append(want, "input:"+s+":steer")
			}
			want = append(want, "assistant", "code", "result", "assistant", "done")
			if got := steerDeepFilter(seq); strings.Join(got, "|") != strings.Join(want, "|") {
				t.Fatalf("history\n got %v\nwant %v", got, want)
			}
		})
	}
}

// Two steers sent while the model writes its final reply both land
// inside the turn, in order; one extra call, one done.
func TestSteerDeepManyDuringFinalReply(t *testing.T) {
	t.Parallel()
	llm := &steerDeepLLM{replies: []string{"first answer", "second answer"}}
	var steer func(string) bool
	texts := steerDeepSteers(2)
	llm.hook = func(i int) {
		if i == 0 {
			for _, s := range texts {
				if !steer(s) {
					t.Errorf("steer %q refused mid-reply", s)
				}
			}
		}
	}
	steer, inputs, kinds, done := mountSteer(t, llm, &stubCode{})
	inputs <- "go"
	waitDone(t, done, kinds)
	if got := strings.Join(kinds(), " "); got != "assistant steer steer assistant done" {
		t.Fatalf("events = %q", got)
	}
	calls := llm.calls()
	if len(calls) != 2 {
		t.Fatalf("model calls = %d, want 2", len(calls))
	}
	steerDeepOrder(t, calls[1], texts)
}

// steerDeepNotices is a job-notices seam with one queued notice.
type steerDeepNotices struct {
	mu   sync.Mutex
	news []string
	wake chan struct{}
}

func (n *steerDeepNotices) Take() []string {
	n.mu.Lock()
	defer n.mu.Unlock()
	s := n.news
	n.news = nil
	return s
}
func (n *steerDeepNotices) Wake() <-chan struct{} { return n.wake }

// A background job's wake starts a turn nobody typed; it is a turn
// like any other, so a steer sent while its block runs lands in it.
func TestSteerDeepDuringJobWakeTurn(t *testing.T) {
	t.Parallel()
	llm := &steerDeepLLM{replies: []string{"```js\nconsole.log('LOOK')\n```\n```js\nconsole.log('MORE')\n```", "ok"}}
	code := &steerDeepCode{}
	no := &steerDeepNotices{news: []string{"job 1 finished: exit 0"}, wake: make(chan struct{}, 1)}
	steer, _, kinds, done := mountSteer(t, llm, code, func(k *kernel.Context) { k.Provide("job-notices", Notices(no)) })
	var ok atomic.Bool
	code.onFirst = func() { ok.Store(steer("wake steer")) }
	no.wake <- struct{}{}
	waitDone(t, done, kinds)
	if !ok.Load() {
		t.Fatal("steer refused during a job-wake turn")
	}
	calls := llm.calls()
	if len(calls) != 2 {
		t.Fatalf("model calls = %d, want 2: %v", len(calls), kinds())
	}
	steerDeepOrder(t, calls[1], []string{"wake steer"})
	if len(code.ran) != 1 {
		t.Fatalf("ran %q, want only the first block", code.ran)
	}
}

// steerDeepBlocking runs its first block until the turn is cancelled
// (a slow bash the user gives up on).
type steerDeepBlocking struct {
	stubCode
	started chan struct{}
}

func (c *steerDeepBlocking) RunCtx(ctx context.Context, code string) (string, error) {
	c.ran = append(c.ran, code)
	if len(c.ran) == 1 {
		close(c.started)
		<-ctx.Done()
		return "", ctx.Err()
	}
	return "CODE!", nil
}

// Steer twice while a block runs, then esc: both steers are kept
// (history, under the cancelled note — nothing runs on their behalf)
// and the NEXT turn's model call sees each exactly once, in order,
// before the new input.
func TestSteerDeepThenCancelDuringBlock(t *testing.T) {
	t.Parallel()
	llm := &steerDeepLLM{replies: []string{"```js\nwork()\n```", "fresh reply"}}
	code := &steerDeepBlocking{started: make(chan struct{})}
	var kctx *kernel.Context
	steer, inputs, kinds, done := mountSteer(t, llm, code, func(k *kernel.Context) { kctx = k })
	cancel, err := kernel.Get[func()](kctx, "cancel")
	if err != nil {
		t.Fatal(err)
	}
	inputs <- "go"
	<-code.started
	texts := steerDeepSteers(2)
	for _, s := range texts {
		if !steer(s) {
			t.Fatalf("steer %q refused mid-block", s)
		}
	}
	cancel()
	waitDone(t, done, kinds)
	select {
	case <-done:
		t.Fatal("a pending steer ran a turn of its own after the cancel")
	case <-time.After(100 * time.Millisecond):
	}
	if n := len(llm.calls()); n != 1 {
		t.Fatalf("model calls after cancel = %d, want 1", n)
	}
	if steer("idle") {
		t.Fatal("steer accepted while idle")
	}
	inputs <- "next"
	waitDone(t, done, kinds)
	calls := llm.calls()
	steerDeepOrder(t, calls[1], append(texts, "next"))
	if got := strings.Count(strings.Join(kinds(), " "), "steer"); got != 2 {
		t.Fatalf("steer events = %d, want 2: %v", got, kinds())
	}
}

// The finish-line race: a steerer hammers while turns start and end.
// Every line is either accepted (then it must land in some turn as a
// steer input, before that turn's done) or refused (then the sender
// queues it as ordinary input, like the ui does). Across the run each
// line reaches history exactly once; none is lost. BOUGH_STEER_LONG=1
// runs 5000 lines.
func TestSteerDeepFinishRace(t *testing.T) {
	t.Parallel()
	n := 200
	if os.Getenv("BOUGH_STEER_LONG") != "" {
		n = 5000
	}
	llm := &steerDeepLLM{replies: []string{"ok"}}
	llm.hook = func(int) { time.Sleep(50 * time.Microsecond) }
	hist := &memHistory{}
	steer, inputs, _, done := mountSteer(t, llm, &stubCode{}, func(k *kernel.Context) { k.Provide("history", History(hist)) })
	var pending atomic.Int64
	go func() {
		for range done {
			pending.Add(-1)
		}
	}()
	refused := 0
	for i := range n {
		line := fmt.Sprintf("line-%04d", i)
		if i%10 == 0 || !steer(line) {
			if i%10 != 0 {
				refused++
			}
			pending.Add(1)
			inputs <- line
		}
		if i%7 == 0 {
			time.Sleep(20 * time.Microsecond)
		}
	}
	deadline := time.Now().Add(20 * time.Second)
	for pending.Load() > 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if pending.Load() != 0 {
		t.Fatalf("%d turns never finished", pending.Load())
	}
	seen := map[string]int{}
	open := false
	for _, e := range hist.Entries() {
		switch e.Kind {
		case "input":
			txt := fmt.Sprint(e.Data["text"])
			seen[txt]++
			if e.Data["steer"] == true {
				if !open {
					t.Fatalf("steer %q recorded outside any turn", txt)
				}
			} else {
				open = true
			}
		case "done":
			open = false
		}
	}
	for i := range n {
		line := fmt.Sprintf("line-%04d", i)
		if seen[line] != 1 {
			t.Fatalf("%s recorded %d times, want 1 (refused=%d)", line, seen[line], refused)
		}
	}
	t.Logf("%d lines, %d refused at the finish line", n, refused)
}
