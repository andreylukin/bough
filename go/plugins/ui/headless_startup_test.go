package ui

import (
	"bytes"
	"strings"
	"testing"
	"testing/synctest"
)

// Not parallel: the headless router is process-wide.
func TestHeadlessCommandWaitsForStartup(t *testing.T) {
	for _, stopped := range []bool{false, true} {
		synctest.Test(t, func(t *testing.T) {
			ready := make(chan struct{})
			var out bytes.Buffer
			oldOut := hlOut
			hlOut = &out
			hlMu.Lock()
			oldReady, oldCmds, oldHist := hlReady, hlCmds, hlHist
			hlReady, hlCmds, hlHist = ready, reg(t, "model"), nil
			hlMu.Unlock()
			defer func() {
				hlOut = oldOut
				hlMu.Lock()
				hlReady, hlCmds, hlHist = oldReady, oldCmds, oldHist
				hlMu.Unlock()
				hlStopped.Store(false)
			}()
			done := make(chan string, 1)
			go func() { done <- hlLineIn("/model control-1") }()
			synctest.Wait()
			select {
			case route := <-done:
				t.Fatalf("command routed as %s before mounting finished: %s", route, out.String())
			default:
			}
			if stopped {
				StopInput()
			}
			close(ready)
			route := <-done
			if stopped {
				if route != "drop" || out.Len() != 0 {
					t.Fatalf("stopped startup command ran: %s %s", route, out.String())
				}
			} else if route != "command" || !strings.Contains(out.String(), "model ran control-1") {
				t.Fatalf("startup command was lost: %s %s", route, out.String())
			}
		})
	}
}
