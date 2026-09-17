package loop

import (
	"context"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// partialStreamer streams some text, then hangs until cancelled and
// returns nothing, like a provider cut off mid-answer.
type partialStreamer struct{}

func (partialStreamer) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	return "", nil
}

func (partialStreamer) Stream(ctx context.Context, system string, messages []Message, onDelta func(string)) (string, error) {
	onDelta("Here is the first ")
	onDelta("half of the answer")
	<-ctx.Done()
	return "", ctx.Err()
}

// Esc mid-stream keeps what was streamed: an assistant entry with the
// partial text lands before cancelled, and the projection still
// alternates roles.
func TestCancelKeepsPartialStreamedText(t *testing.T) {
	t.Parallel()
	kctx := kernel.NewContext()
	kctx.Provide("llm", partialStreamer{})
	kctx.Provide("codemode", &stubCode{})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	t.Cleanup(kctx.Unmount)
	inputs, _ := kernel.Get[chan string](kctx, "inputs")
	cancel, _ := kernel.Get[func()](kctx, "cancel")
	done := make(chan struct{})
	kctx.On("loop/event", func(p any) {
		if p.(Event).Kind == "done" {
			close(done)
		}
	})
	inputs <- "go"
	time.Sleep(50 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatal("turn did not end after cancel")
	}
	r, _ := kernel.Get[*runner](kctx, "runner")
	e := r.hist.Entries()
	n := len(e)
	if n < 3 || e[n-3].Kind != "assistant" || e[n-2].Kind != "cancelled" {
		t.Fatalf("want assistant, cancelled, done at the end; got %v", e)
	}
	if got, _ := e[n-3].Data["text"].(string); got != "Here is the first half of the answer" {
		t.Errorf("partial text = %q", got)
	}
	if p, _ := e[n-3].Data["partial"].(bool); !p {
		t.Errorf("partial flag missing: %v", e[n-3].Data)
	}
	msgs := DefaultProject(e)
	for i := 1; i < len(msgs); i++ {
		if msgs[i].Role == msgs[i-1].Role {
			t.Fatalf("roles do not alternate: %+v", msgs)
		}
	}
}
