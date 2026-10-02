package contextkit

import (
	"context"
	"errors"
	"testing"
)

// A write-path approval supplements the tool registration approval; it
// must not revive a call whose original context-tool row was removed.
func TestCommitChecksCompose(t *testing.T) {
	t.Parallel()
	stale := errors.New("original tool removed")
	var order []int
	ctx := WithCommitCheck(t.Context(), func() error { order = append(order, 1); return nil })
	ctx = WithCommitCheck(ctx, func() error { order = append(order, 2); return stale })
	ctx = WithCommitCheck(ctx, func() error { t.Fatal("ran after failed approval"); return nil })
	if err := CheckCommit(ctx); !errors.Is(err, stale) || len(order) != 2 || order[0] != 1 || order[1] != 2 {
		t.Fatalf("lost policy check: %v %v", order, err)
	}
	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if err := CheckCommit(cancelled); !errors.Is(err, context.Canceled) || len(order) != 2 {
		t.Fatalf("checks ran after cancellation: %v %v", order, err)
	}
}
