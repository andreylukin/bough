package stepgate

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

// Off (the nil Gate, what every process gets without the variable) the
// hook must cost nothing and change nothing: a production binary runs
// every one of these calls.
func TestNilGateIsANoOp(t *testing.T) {
	t.Parallel()
	var g *Gate
	g.Hold("x")()
	if !g.HoldOr("x", nil) {
		t.Fatal("HoldOr on the nil gate held")
	}
	g.Note("k", "v")
	g.Watch("w", nil, func() { t.Error("Watch on the nil gate ran its func") })
	in := make(chan string, 1)
	if Relay(g, "r", in, nil) != (<-chan string)(in) {
		t.Fatal("Relay on the nil gate did not hand back its input")
	}
}

// A relayed value stays unread until its hold is let through, as the
// ask's answer does while the test fires the timeout.
func TestRelayHoldsUntilLetThrough(t *testing.T) {
	t.Parallel()
	g := &Gate{dir: t.TempDir()}
	in := make(chan string, 1)
	in <- "late"
	stop := make(chan struct{})
	defer close(stop)
	out := Relay(g, "recv", in, stop)
	select {
	case v := <-out:
		t.Fatalf("relayed %q before the hold was let through", v)
	case <-time.After(50 * time.Millisecond):
	}
	if len(in) != 1 {
		t.Fatal("the value left its channel while held")
	}
	if err := os.WriteFile(filepath.Join(g.dir, "recv.go"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	select {
	case v := <-out:
		if v != "late" {
			t.Fatalf("relayed %q, want late", v)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("not relayed after the hold was let through")
	}
}
