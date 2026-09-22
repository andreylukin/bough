package codemode

import (
	"context"
	"encoding/json"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/kernel"
)

func mountRunJS(t *testing.T) (*CodeMode, agenttools.Tool) {
	t.Helper()
	ctx := kernel.NewContext()
	reg := agenttools.NewRegistry()
	ctx.Provide("agent-tools", reg)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(ctx.Unmount)
	cm, _ := kernel.Get[*CodeMode](ctx, "codemode")
	tl, ok := reg.Lookup(RunJSName)
	if !ok {
		t.Fatal("run_js not registered")
	}
	return cm, tl
}

func runJS(t *testing.T, ctx context.Context, tl agenttools.Tool, code string) agenttools.Result {
	t.Helper()
	args, _ := json.Marshal(map[string]string{"code": code})
	r, err := tl.Call(ctx, agenttools.Call{Args: args})
	if err != nil {
		t.Fatal(err)
	}
	return r
}

// run_js is a block in the session's VM: tools.* are there, output and
// the last value come back, a throw is the call's error with what was
// printed before it, and globals persist to the next call.
func TestNativeRunJS(t *testing.T) {
	t.Parallel()
	cm, tl := mountRunJS(t)
	cm.RegisterTool("double", func(n int) int { return 2 * n })
	if r := runJS(t, context.Background(), tl, "console.log('hi'); kept = tools.double(21); kept"); r.Error != "" || r.Text != "hi\n42" {
		t.Fatalf("run = %+v", r)
	}
	if r := runJS(t, context.Background(), tl, "kept + 1"); r.Text != "43" {
		t.Fatalf("globals = %+v", r)
	}
	if r := runJS(t, context.Background(), tl, "console.log('before'); throw new Error('boom')"); r.Text != "before\n" || !strings.Contains(r.Error, "boom") {
		t.Fatalf("throw = %+v", r)
	}
	if d := tl.Detail(json.RawMessage(`{"code":"\n  const x = 1\nx"}`)); d != "const x = 1" {
		t.Fatalf("detail = %q", d)
	}
}

// Cancelling the call interrupts the block, as Esc interrupts the loop's.
func TestNativeRunJSCancel(t *testing.T) {
	t.Parallel()
	_, tl := mountRunJS(t)
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(100*time.Millisecond, cancel)
	start := time.Now()
	r := runJS(t, ctx, tl, "while (true) {}")
	if !strings.Contains(r.Error, "cancelled") || time.Since(start) > 5*time.Second {
		t.Fatalf("cancel = %+v after %s", r, time.Since(start))
	}
	if r := runJS(t, context.Background(), tl, "1 + 1"); r.Text != "2" {
		t.Fatalf("after a cancel = %+v", r)
	}
}

// Parallel calls queue on the VM: each block's output is its own.
func TestNativeRunJSParallelCallsQueue(t *testing.T) {
	t.Parallel()
	_, tl := mountRunJS(t)
	var wg sync.WaitGroup
	out := make([]agenttools.Result, 8)
	for i := range out {
		wg.Go(func() {
			out[i] = runJS(t, context.Background(), tl, "for (let k = 0; k < 3; k++) console.log('block', "+string(rune('0'+i))+"); 'done'")
		})
	}
	wg.Wait()
	for i, r := range out {
		want := strings.Repeat("block "+string(rune('0'+i))+"\n", 3) + "done"
		if r.Error != "" || r.Text != want {
			t.Fatalf("call %d = %+v", i, r)
		}
	}
}
