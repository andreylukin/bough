package ui

import (
	"bufio"
	"fmt"
	"os"
	"sync/atomic"
	"time"
)

// runHeadless reads lines from stdin into inputs and prints loop events
// as "[kind] text" on stdout. On stdin EOF it waits for in-flight runs to
// finish (a "done" event per sent line, with an idle timeout), then
// interrupts the process so the launcher unmounts and exits 0.
func runHeadless(inputs chan<- string, b *broadcaster) {
	events, _ := b.subscribe()
	var pending atomic.Int64
	tick := make(chan struct{}, 1)
	go func() {
		for ev := range events {
			fmt.Printf("[%s] %s\n", ev.Kind, ev.Text)
			if ev.Kind == "done" {
				pending.Add(-1)
			}
			select {
			case tick <- struct{}{}:
			default:
			}
		}
	}()

	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for sc.Scan() {
		pending.Add(1)
		inputs <- sc.Text()
	}

	// EOF: drain until every sent line saw its "done", or events go idle.
	for pending.Load() > 0 {
		select {
		case <-tick:
		case <-time.After(60 * time.Second):
			fmt.Fprintln(os.Stderr, "ui: headless: timed out waiting for loop to finish")
			pending.Store(0)
		}
	}
	interruptSelf()
}
