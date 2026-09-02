package loop

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
)

// stubLLM replies with a js block on the first call, plain text after.
type stubLLM struct{ calls int }

func (s *stubLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	s.calls++
	if s.calls == 1 {
		return "Running it.\n```js\nconsole.log('CODE!')\n```", nil
	}
	return "All done.", nil
}

type stubCode struct{ ran []string }

func (s *stubCode) RegisterTool(name string, fn any) {}
func (s *stubCode) Run(code string) (string, error) {
	s.ran = append(s.ran, code)
	return "CODE!", nil
}

func TestLoopCodeResultDoneSequence(t *testing.T) {
	kctx := kernel.NewContext()
	kctx.Provide("llm", &stubLLM{})
	code := &stubCode{}
	kctx.Provide("codemode", code)

	var mu sync.Mutex
	var kinds []string
	done := make(chan struct{})
	kctx.On("loop/event", func(p any) {
		ev, ok := p.(Event)
		if !ok {
			t.Errorf("payload is %T, want Event", p)
			return
		}
		mu.Lock()
		kinds = append(kinds, ev.Kind)
		mu.Unlock()
		if ev.Kind == "done" {
			close(done)
		}
	})

	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	inputs, err := kernel.Get[chan string](kctx, "inputs")
	if err != nil {
		t.Fatalf("inputs: %v", err)
	}
	inputs <- "do the thing"

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for done event")
	}

	mu.Lock()
	defer mu.Unlock()
	want := []string{"assistant", "code", "result", "assistant", "done"}
	if len(kinds) != len(want) {
		t.Fatalf("kinds = %v, want %v", kinds, want)
	}
	for i := range want {
		if kinds[i] != want[i] {
			t.Fatalf("kinds = %v, want %v", kinds, want)
		}
	}
	if len(code.ran) != 1 || code.ran[0] != "console.log('CODE!')\n" {
		t.Fatalf("codemode ran %q", code.ran)
	}
	kctx.Unmount()
}
