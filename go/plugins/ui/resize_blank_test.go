package ui

import (
	"strings"
	"testing"
)

// Growing then shrinking the terminal must not blank the transcript
// (the internal/vtreal tmux resize sequence, pinned in-process).
func TestResizeGrowThenShrinkKeepsTranscript(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 120, 40, cfgWith(t, nil, nil, nil))
	d.event("user", "resize me")
	d.event("assistant", "echo: resize me")
	d.event("done", "")
	for _, sz := range [][2]int{{40, 12}, {200, 50}, {60, 8}, {100, 30}} {
		d.feed(windowSize(sz[0], sz[1]))
		if p := d.plain(); !strings.Contains(p, "resize me") {
			t.Fatalf("transcript blank after resize to %dx%d (yoff=%d atBottom=%v):\n%s", sz[0], sz[1], d.m.vp.YOffset(), d.m.vp.AtBottom(), p)
		}
	}
}
