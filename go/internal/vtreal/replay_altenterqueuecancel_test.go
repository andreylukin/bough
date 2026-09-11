package vtreal

// alt+enter queues follow-ups mid-turn (plugins/ui/model.go submit:
// the line goes straight to the loop's inputs channel); esc cancels
// the turn in flight (stop.go escPress → loop cancel). Two queued
// lines must then either both run, in order, or both come back to the
// composer — never one alone, never swapped. The history file's
// "input" entries are the record of what ran.

import (
	"fmt"
	"strings"
	"testing"
	"time"

	uv "github.com/charmbracelet/ultraviolet"
)

func altEnterQueueCancelEnded(a *app) int {
	c := 0
	for _, e := range followUpNewest(a) {
		if e.Kind == "done" || e.Kind == "cancelled" {
			c++
		}
	}
	return c
}

func TestAltEnterQueueCancelBothOrNeither(t *testing.T) {
	t.Parallel()
	tape := followUpTape(t)
	cfg := strings.Replace(replayConfig(tape),
		fmt.Sprintf("config: {file: %q}\n", tape),
		fmt.Sprintf("config: {file: %q, delay_ms: 400}\n", tape), 1)
	if !strings.Contains(cfg, "delay_ms: 400") {
		t.Fatalf("could not add delay_ms to the replay row:\n%s", cfg)
	}
	a := startCfg(t, 100, 30, cfg)
	a.typeText("alpha")
	a.key(uv.KeyEnter, 0)
	a.waitFor("one two") // alpha is streaming
	a.typeText("beta")
	a.key(uv.KeyEnter, uv.ModAlt)
	a.waitFor("beta (queued)")
	a.typeText("gamma")
	a.key(uv.KeyEnter, uv.ModAlt)
	a.waitFor("gamma (queued)")
	a.key(uv.KeyEscape, 0)

	// Settle: either three turns end (both ran) or alpha ends and the
	// composer holds both queued lines (both restored).
	restored := func() bool {
		c := followUpComposer(a)
		return strings.Contains(c, "beta") && strings.Contains(c, "gamma")
	}
	deadline := time.Now().Add(30 * time.Second)
	for time.Now().Before(deadline) {
		if n := altEnterQueueCancelEnded(a); n >= 3 || (n >= 1 && restored()) {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	time.Sleep(1500 * time.Millisecond) // let a stray extra turn show up
	s := a.settled()
	in := fmt.Sprint(followUpKinds(a, "input"))
	switch in {
	case "[alpha beta gamma]":
		a.waitFor("REPLY-GAMMA")
		if s := a.settled(); strings.Contains(s, "(queued)") {
			t.Errorf("a follow-up still shows queued after running:\n%s", s)
		}
	case "[alpha]":
		if !restored() {
			t.Errorf("queued lines neither ran nor came back to the composer:\n%s", s)
		}
		if c := followUpComposer(a); strings.Index(c, "beta") > strings.Index(c, "gamma") {
			t.Errorf("restored follow-ups reordered (%q):\n%s", c, s)
		}
	default:
		t.Errorf("inputs = %s, want [alpha beta gamma] or [alpha]:\n%s", in, s)
	}
	a.check("after esc with two queued")
}
