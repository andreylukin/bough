package ui

import (
	"fmt"
	"testing"
	"time"
)

// A subscriber that reads slower than the emitter publishes still gets
// every event, in order, and publish never waits on it.
func TestBroadcasterSlowSubscriberLosesNothing(t *testing.T) {
	b := &broadcaster{subs: map[int]*subscriber{}}
	ch, unsub := b.subscribe()
	defer unsub()
	const n = 5000
	start := time.Now()
	for i := range n {
		b.publish(Event{Kind: "delta", Text: fmt.Sprint(i)})
	}
	if d := time.Since(start); d > time.Second {
		t.Fatalf("publishing %d events to an unread subscriber took %v; publish blocked", n, d)
	}
	for i := range n {
		select {
		case ev := <-ch:
			if ev.Text != fmt.Sprint(i) {
				t.Fatalf("event %d = %q, want %d", i, ev.Text, i)
			}
			if i%1000 == 0 {
				time.Sleep(time.Millisecond) // a slow reader
			}
		case <-time.After(5 * time.Second):
			t.Fatalf("event %d of %d never arrived", i, n)
		}
	}
}
