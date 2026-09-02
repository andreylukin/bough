package ui

import (
	"bufio"
	"fmt"
	"os"
	"sync"
	"sync/atomic"
	"time"
)

// Headless mode reads lines from stdin into inputs and prints loop
// events as "[kind] text" on stdout. On stdin EOF it waits for
// in-flight runs to finish (a "done" event per sent line, with an idle
// timeout), then interrupts the process so the launcher unmounts and
// exits 0.
//
// Stdin can only be read once per process, but hot reload can remount
// the ui row (with a fresh inputs channel), so the stdin pump is a
// process-wide singleton and the target channel is swapped per mount.
var (
	hlOnce    sync.Once
	hlMu      sync.Mutex
	hlInputs  chan<- string // current mount's inputs; nil while unmounted
	hlPending atomic.Int64
	hlTick    = make(chan struct{}, 1)
)

// runHeadless wires this mount's inputs and broadcaster into the pump,
// starts the pump on first call, and returns a disposer that detaches
// inputs so a reload never sends into a closed channel. The printer
// goroutine for a disposed mount leaks quietly (its broadcaster stops
// publishing); one idle goroutine per reload is accepted.
func runHeadless(inputs chan<- string, b *broadcaster) func() {
	events, _ := b.subscribe()
	go func() {
		for ev := range events {
			fmt.Printf("[%s] %s\n", ev.Kind, ev.Text)
			if ev.Kind == "done" {
				hlPending.Add(-1)
			}
			select {
			case hlTick <- struct{}{}:
			default:
			}
		}
	}()

	hlMu.Lock()
	hlInputs = inputs
	hlMu.Unlock()
	hlOnce.Do(func() { go headlessPump() })

	return func() {
		hlMu.Lock()
		hlInputs = nil
		hlMu.Unlock()
	}
}

func headlessPump() {
	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for sc.Scan() {
		line := sc.Text()
		hlPending.Add(1)
		for {
			hlMu.Lock()
			ch := hlInputs
			if ch != nil {
				// Send under the lock: the disposer (which runs before the
				// loop row closes the channel) blocks until we finish.
				ch <- line
				hlMu.Unlock()
				break
			}
			hlMu.Unlock()
			// Mid-reload: wait for the remounted ui row to reattach.
			time.Sleep(50 * time.Millisecond)
		}
	}

	// EOF: drain until every sent line saw its "done", or events go idle.
	for hlPending.Load() > 0 {
		select {
		case <-hlTick:
		case <-time.After(60 * time.Second):
			fmt.Fprintln(os.Stderr, "ui: headless: timed out waiting for loop to finish")
			hlPending.Store(0)
		}
	}
	interruptSelf()
}
