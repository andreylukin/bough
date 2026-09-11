package ui

import "testing"

func TestCancelMidThinkingSettlesLiveThinking(t *testing.T) {
	d := newDrv(t, 80, 20, cfgWith(t, nil, nil, nil))
	d.event("user", "q")
	d.event("thinking-delta", "hmm")
	d.event("cancelled", "")
	d.event("done", "")
	for _, b := range d.m.blocks {
		if b.live {
			t.Fatalf("live block survived a cancelled turn: %+v\n%s", b, d.plain())
		}
	}
}
