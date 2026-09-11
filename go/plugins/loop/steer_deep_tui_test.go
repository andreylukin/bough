package loop_test

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/uitest"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/llm"
)

// steerDeepRecLLM wraps a Script and records every call's messages.
type steerDeepRecLLM struct {
	*uitest.Script
	mu   sync.Mutex
	seen [][]llm.Message
}

func (r *steerDeepRecLLM) Complete(ctx context.Context, sys string, msgs []llm.Message) (string, error) {
	r.mu.Lock()
	r.seen = append(r.seen, append([]llm.Message(nil), msgs...))
	r.mu.Unlock()
	return r.Script.Complete(ctx, sys, msgs)
}

// Three steers typed while a real bash block runs through the real
// codemode: all land at the block's boundary, the model's next call
// carries each exactly once in the order typed, and every row settles.
func TestSteerDeepThreeDuringSlowBashEndToEnd(t *testing.T) {
	t.Parallel()
	gate := filepath.Join(t.TempDir(), "gate")
	stub := &steerDeepRecLLM{Script: &uitest.Script{Replies: []string{
		uitest.Bash("while [ ! -e " + gate + " ]; do sleep 0.05; done; echo ONE_RAN"),
		"steered ok",
	}}}
	d := uitest.Mount(t, func(c *kernel.Context) { c.Provide("llm", stub) },
		"codemode", "tools-basic", "loop")
	d.Say("go")
	d.WaitFor("Ran: while")
	steers := []string{"alpha steer", "bravo steer", "charlie steer"}
	for _, s := range steers {
		d.Say(s)
	}
	d.WaitUntil(func(f string) bool { return strings.Count(f, "(steer · pending)") == 3 }, "three pending steers")
	if err := os.WriteFile(gate, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	d.WaitFor("steered ok")
	d.WaitUntil(func(f string) bool { return !strings.Contains(f, "pending") }, "steers to land")
	f := d.Frame()
	stub.mu.Lock()
	defer stub.mu.Unlock()
	if len(stub.seen) != 2 {
		t.Fatalf("model calls = %d, want 2", len(stub.seen))
	}
	msgs, last := stub.seen[1], -1
	for _, s := range steers {
		n, at := 0, -1
		for i, m := range msgs {
			if m.Role == "user" && strings.HasPrefix(m.Content, s) {
				n, at = n+1, i
			}
		}
		if n != 1 || at <= last {
			t.Fatalf("steer %q: %d copies at %d (prev %d): %+v", s, n, at, last, msgs)
		}
		last = at
		if c := strings.Count(f, "❯ "+s+" (steer)"); c != 1 {
			t.Fatalf("row for %q shown %d times:\n%s", s, c, f)
		}
	}
}
