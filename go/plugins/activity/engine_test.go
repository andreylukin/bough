package activity

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/llm"
	"github.com/andreylukin/bough/plugins/loop"
)

type recLLM struct {
	mu   sync.Mutex
	seen []string
}

func (r *recLLM) Complete(_ context.Context, _ string, msgs []llm.Message) (string, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.seen = append(r.seen, msgs[0].Content)
	return "running the tests", nil
}

// On the engine a call start is the step, so it gets a label; a loop
// block's per-call start (numeric id) is inside a program that already
// has one.
func TestEngineCallStartIsLabelled(t *testing.T) {
	t.Parallel()
	r := &recLLM{}
	var mu sync.Mutex
	var got []string
	a := &Activity{llm: r, ctx: context.Background(), emit: func(s string) { mu.Lock(); got = append(got, s); mu.Unlock() }}
	a.on(loop.Event{Kind: "call", Text: "ls", Data: map[string]any{"id": 1, "tool": "bash", "phase": "start"}})
	a.on(loop.Event{Kind: "call", Text: "go test ./...", Data: map[string]any{"id": "c1", "tool": "bash", "ms": 3}})
	a.on(loop.Event{Kind: "call", Text: "go test ./...", Data: map[string]any{"id": "c1", "tool": "bash", "phase": "start"}})
	deadline := time.Now().Add(5 * time.Second)
	for {
		mu.Lock()
		n := len(got)
		mu.Unlock()
		if n == 2 || time.Now().After(deadline) {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.seen) != 1 || r.seen[0] != `tools.bash("go test ./...")` {
		t.Fatalf("labelled %q, want only the engine call's start", r.seen)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(got) != 2 || got[1] != "running the tests" {
		t.Fatalf("emitted %q", got)
	}
}
