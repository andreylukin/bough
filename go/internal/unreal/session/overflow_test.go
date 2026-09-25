//go:build !windows

package session

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/andreylukin/bough/internal/agentllm"
	"github.com/andreylukin/bough/internal/unreal/fake"
)

// A job that ends while the model's overflow is sticky wakes a request
// the Gate answers without the provider. The wake still opens its turn,
// so the error it records is that turn's, closed by its done, and never
// an error with no turn around it (which the page read as a failed
// session with no turn to retry).
func TestStickyOverflowWakeOpensItsTurn(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		r := newRigWith(t, []rigOpt{settle(200 * time.Millisecond)},
			fake.Step{Want: "serve", Output: []ullmItem{fake.Call("h1", "hold", `{"text":"npm run dev"}`)}},
			fake.Step{Want: "big", Err: fmt.Errorf("prompt too long: %w", agentllm.ErrContextOverflow)},
		)
		r.rt.Submit("serve it")
		r.waitDone(1)
		r.rt.Submit("big")
		r.waitDone(2)
		close(r.kit.release)
		r.waitFor("the wake's error", func() bool { return r.count("error") >= 2 })
		r.waitDone(3)
		open := false
		for _, e := range r.entries() {
			switch e.Kind {
			case "input":
				open = true
			case "done":
				open = false
			case "error":
				if !open {
					t.Fatalf("error entry %d outside a turn\n%s", e.Seq, r.dump())
				}
			}
		}
		if wake := r.last("input"); wake.Data["wake"] != true {
			t.Fatalf("the last input is not the wake: %v\n%s", wake.Data, r.dump())
		}
		if n := len(r.fake.Requests()); n != 2 {
			t.Fatalf("%d provider requests, want 2 (the wake's is the Gate's)", n)
		}
	})
}

// The overflow outlives the process: a session reopened after one (a
// serve restart, a Stop's respawn) answers the overflowed model's next
// request itself instead of paying for another 400.
func TestOverflowStickyAcrossReopen(t *testing.T) {
	t.Parallel()
	r := newRig(t, fake.Step{Want: "big", Err: fmt.Errorf("prompt too long: %w", agentllm.ErrContextOverflow)})
	r.rt.Submit("big")
	r.waitDone(1)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := r.rt.Close(ctx); err != nil {
		t.Fatal(err)
	}
	r.open()
	r.rt.Submit("again")
	r.waitDone(2)
	if n := len(r.fake.Requests()); n != 1 {
		t.Fatalf("%d provider requests, want 1 (the overflow is sticky across a reopen)\n%s", n, r.dump())
	}
	if !strings.Contains(r.last("error").Data["text"].(string), "no longer fits") || r.count("error") != 2 {
		t.Fatalf("history\n%s", r.dump())
	}
}
