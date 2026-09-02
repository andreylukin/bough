package loop

import (
	"context"
	"strings"
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

	// The turn is recorded in history: input first, then the event
	// mirror (a run's output lands as a "result" entry either way).
	r, err := kernel.Get[*runner](kctx, "runner")
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	var entryKinds []string
	for _, e := range r.hist.Entries() {
		entryKinds = append(entryKinds, e.Kind)
	}
	wantEntries := []string{"input", "assistant", "code", "result", "assistant", "done"}
	if len(entryKinds) != len(wantEntries) {
		t.Fatalf("entry kinds = %v, want %v", entryKinds, wantEntries)
	}
	for i := range wantEntries {
		if entryKinds[i] != wantEntries[i] {
			t.Fatalf("entry kinds = %v, want %v", entryKinds, wantEntries)
		}
	}
	entries := r.hist.Entries()
	if entries[0].Data["text"] != "do the thing" {
		t.Fatalf("input entry = %+v", entries[0])
	}
	if entries[3].Data["code"] != "console.log('CODE!')\n" {
		t.Fatalf("result entry carries no code: %+v", entries[3])
	}
	kctx.Unmount()
}

// sysLLM records the system prompt it was called with.
type sysLLM struct {
	mu     sync.Mutex
	system string
}

func (s *sysLLM) Complete(ctx context.Context, system string, messages []Message) (string, error) {
	s.mu.Lock()
	s.system = system
	s.mu.Unlock()
	return "ok", nil
}

func (s *sysLLM) seen() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.system
}

// runTurn mounts the loop over llm/code (plus extra services), sends
// one input and waits for its done event.
func runTurn(t *testing.T, llm LLM, extra map[string]any) {
	t.Helper()
	kctx := kernel.NewContext()
	kctx.Provide("llm", llm)
	kctx.Provide("codemode", &stubCode{})
	for k, v := range extra {
		kctx.Provide(k, v)
	}
	done := make(chan struct{})
	kctx.On("loop/event", func(p any) {
		if ev, ok := p.(Event); ok && ev.Kind == "done" {
			select {
			case <-done:
			default:
				close(done)
			}
		}
	})
	if err := (&plugin{}).Apply(kctx, nil); err != nil {
		t.Fatalf("Apply: %v", err)
	}
	inputs, err := kernel.Get[chan string](kctx, "inputs")
	if err != nil {
		t.Fatalf("inputs: %v", err)
	}
	inputs <- "hi"
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for done event")
	}
	kctx.Unmount()
}

func TestSystemPromptDocumentsAskWhenMounted(t *testing.T) {
	llm := &sysLLM{}
	runTurn(t, llm, map[string]any{"ask-answers": struct{}{}})
	sys := llm.seen()
	for _, want := range []string{"tools.ask(", "separate argument", "clickable choices"} {
		if !strings.Contains(sys, want) {
			t.Errorf("system prompt should contain %q:\n%s", want, sys)
		}
	}
}

func TestSystemPromptOmitsAskWhenAbsent(t *testing.T) {
	llm := &sysLLM{}
	runTurn(t, llm, nil)
	if strings.Contains(llm.seen(), "tools.ask(") {
		t.Errorf("no ask plugin mounted, prompt must not document tools.ask:\n%s", llm.seen())
	}
}
