//go:build !windows

package e2e

import (
	"testing"

	"github.com/andreylukin/bough/plugins/orb"
)

// An orb restart asked for by the agent's own bash call must wait for
// the turn to end, and the restarter knows a turn is open only from the
// loop's live event kinds (orb.TurnEvent). This pins both engines'
// streams to it: from the tool call's start to its end the restarter
// reads the turn as open, the turn closes with "done", and nothing after
// "done" opens it again. An engine renaming its call event fails here
// instead of letting a swap kill the call that asked for it.
func TestTurnEventsHoldOrbRestart(t *testing.T) {
	t.Parallel()
	for _, engine := range []string{"engine-unreal", "loop"} {
		t.Run(engine, func(t *testing.T) {
			t.Parallel()
			b := launchHeadless(t, launchOpts{args: []string{"--json"}, sets: []string{"loop.plugin=" + engine}})
			b.send("say CODE! please")
			b.closeStdin()
			if code := b.waitExit(); code != 0 {
				t.Fatalf("exit %d:\n%s", code, b.out.String())
			}
			out := b.out.String()
			open, opened, callStart, callEnd, dones := false, false, false, false, 0
			for _, ev := range events(out) {
				kind, _ := ev["kind"].(string)
				if kind == "call" {
					// The state before the call's end event is what the
					// restarter reads while the call runs.
					_, ended := ev["exit"]
					if ended && !open {
						t.Fatalf("%s: the call ran while the restarter read the turn as closed:\n%s", engine, out)
					}
					callStart = callStart || !ended
					callEnd = callEnd || ended
				}
				busy, done := orb.TurnEvent(kind)
				switch {
				case busy:
					if dones > 0 {
						t.Fatalf("%s: %q after done reopens the turn:\n%s", engine, kind, out)
					}
					open, opened = true, true
				case done:
					open = false
					dones++
				}
			}
			if !opened || !callStart || !callEnd || dones != 1 || open {
				t.Fatalf("%s: opened %v, call start %v end %v, done %d, open at exit %v:\n%s", engine, opened, callStart, callEnd, dones, open, out)
			}
		})
	}
}
