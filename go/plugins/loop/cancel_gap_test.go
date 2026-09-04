package loop

// A cancel that arrives before its turn has installed a context must
// not be dropped. The ui marks itself running as it submits and the
// input only reaches run afterwards, so ctrl+c pressed the instant you
// see your own typo lands in that gap. On a fast machine the gap is
// microseconds; on a loaded CI runner it was wide enough to lose the
// cancel every time.

import (
	"context"
	"testing"
)

func TestCancelBeforeTheTurnStartsIsHonoured(t *testing.T) {
	var tn turns
	tn.Cancel() // nothing in flight yet

	// run installs the turn's context; the pending cancel applies to it.
	tctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tn.mu.Lock()
	tn.cancel = cancel
	pending := tn.pending
	tn.pending = false
	tn.mu.Unlock()
	if !pending {
		t.Fatal("a cancel with no turn in flight must be remembered, not dropped")
	}
	cancel() // what run does when it sees the pending flag
	if tctx.Err() == nil {
		t.Fatal("the turn should start already cancelled")
	}
}

// The flag is consumed, so it cannot cancel a second turn.
func TestPendingCancelAppliesOnlyOnce(t *testing.T) {
	var tn turns
	tn.Cancel()

	tn.mu.Lock()
	first := tn.pending
	tn.pending = false
	tn.mu.Unlock()
	if !first {
		t.Fatal("the first turn should see the pending cancel")
	}

	tn.mu.Lock()
	second := tn.pending
	tn.mu.Unlock()
	if second {
		t.Fatal("a consumed cancel must not carry into the next turn")
	}
}

// A cancel with a turn in flight still cancels that turn directly.
func TestCancelDuringATurnStillCancels(t *testing.T) {
	var tn turns
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tn.mu.Lock()
	tn.cancel = cancel
	tn.mu.Unlock()

	tn.Cancel()
	if ctx.Err() == nil {
		t.Fatal("the turn in flight should be cancelled")
	}
	tn.mu.Lock()
	defer tn.mu.Unlock()
	if tn.pending {
		t.Error("a cancel that reached a live turn must not also latch")
	}
}
