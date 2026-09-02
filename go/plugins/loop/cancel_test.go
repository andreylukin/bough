package loop

import (
	"context"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

// slowLLM blocks until ctx is cancelled (or a long timeout), like a
// provider mid-stream.
type slowLLM struct{ reply string }

func (l slowLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	if l.reply != "" {
		r := l.reply
		l.reply = "" // one reply, then hang (value receiver: per-call copy)
		return r, nil
	}
	select {
	case <-ctx.Done():
		return "", ctx.Err()
	case <-time.After(10 * time.Second):
		return "too late", nil
	}
}

// The "cancel" service aborts a turn stuck in the LLM call: the turn
// ends with cancelled + done, never an error.
func TestCancelStopsPendingLLMCall(t *testing.T) {
	t.Parallel()
	kctx := kernel.NewContext()
	kctx.Provide("llm", slowLLM{})
	kctx.Provide("codemode", &stubCode{})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	inputs, _ := kernel.Get[chan string](kctx, "inputs")
	cancel, err := kernel.Get[func()](kctx, "cancel")
	if err != nil {
		t.Fatalf("cancel service: %v", err)
	}
	var kinds []string
	done := make(chan struct{})
	kctx.On("loop/event", func(p any) {
		ev := p.(Event)
		kinds = append(kinds, ev.Kind)
		if ev.Kind == "done" {
			close(done)
		}
	})
	inputs <- "go"
	time.Sleep(50 * time.Millisecond) // let the turn reach Complete
	cancel()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatalf("turn did not end after cancel; events %v", kinds)
	}
	if len(kinds) != 2 || kinds[0] != "cancelled" || kinds[1] != "done" {
		t.Fatalf("events = %v, want [cancelled done]", kinds)
	}
	r, _ := kernel.Get[*runner](kctx, "runner")
	entries := r.hist.Entries()
	if last := entries[len(entries)-2]; last.Kind != "cancelled" {
		t.Errorf("history should record a cancelled entry, got %v", entries)
	}
}

// Cancelling mid-script interrupts the running code (a real codemode
// VM spinning in while(true)) instead of waiting for its timeout.
func TestCancelInterruptsRunningCode(t *testing.T) {
	t.Parallel()
	r := &runner{
		llm:  slowLLM{reply: "```js\nwhile (true) {}\n```"},
		code: codemode.New(30 * time.Second),
		hist: &memHistory{},
	}
	ctx, cancel := context.WithCancel(context.Background())
	var kinds []string
	errc := make(chan error, 1)
	go func() { errc <- r.Run(ctx, "spin", func(k, _ string) { kinds = append(kinds, k) }) }()
	time.Sleep(100 * time.Millisecond) // the VM is spinning
	cancel()
	select {
	case err := <-errc:
		if err != context.Canceled {
			t.Fatalf("Run err = %v, want context.Canceled", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("Run did not return after cancel: code was not interrupted")
	}
	if n := len(kinds); n < 2 || kinds[n-2] != "cancelled" || kinds[n-1] != "done" {
		t.Fatalf("events = %v, want ... cancelled done", kinds)
	}
}
