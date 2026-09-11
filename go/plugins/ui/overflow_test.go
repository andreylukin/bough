package ui

import (
	"strings"
	"testing"

	"charm.land/lipgloss/v2"
	xansi "github.com/charmbracelet/x/ansi"
)

// An open box is exactly pane width - 4, however long its unbroken
// runs: hard-wrapping at w-2 forgot the padding and pushed the box two
// cells wider than its neighbours.
func TestBoxWidthWithUnbrokenRun(t *testing.T) {
	d := newDrv(t, 100, 24, cfgWith(t, nil, nil, nil))
	out := d.m.box(strings.Repeat("-", 180), lipgloss.NewStyle(), lipgloss.NewStyle())
	for i, l := range strings.Split(out, "\n") {
		if w := xansi.StringWidth(l); w != 96 {
			t.Fatalf("box row %d is %d cells wide, want 96: %s", i, w, xansi.Strip(l))
		}
	}
}

// No rendered line may be wider than the pane: a wide line makes the
// viewport scrollable sideways and the whole transcript reads as if it
// had slid off the screen. Markdown code fences are the usual culprit
// (glamour leaves them unwrapped); a raw long line in any block is the
// other.
func TestNoLineWiderThanThePane(t *testing.T) {
	cfg := cfgWith(t, nil, nil, nil)
	cfg.collapse = "none" // boxes drawn open: a closed block is one truncated row
	d := newDrv(t, 100, 24, cfg)
	long := strings.Repeat("x", 300)
	d.feed(eventMsg(Event{Kind: "user", Text: "go"}))
	d.feed(eventMsg(Event{Kind: "assistant", Text: "Here:\n\n```go\nfunc f() { " + long + " }\n```\n\nand a URL https://example.com/" + long}))
	// One unbroken run that no word-wrap can split: the case lipgloss
	// used to let widen the box past the pane.
	d.feed(eventMsg(Event{Kind: "result", Text: "#!/usr/bin/env python3\n# ---- batch " + strings.Repeat("-", 85) + "\nprint(1)"}))
	d.feed(eventMsg(Event{Kind: "code", Text: "x = \"" + long + "\""}))
	d.feed(eventMsg(Event{Kind: "todo", Text: "[ ] " + long}))
	d.feed(eventMsg(Event{Kind: "done", Text: ""}))
	for i, l := range d.m.lines {
		if w := xansi.StringWidth(l); w > 100 {
			t.Fatalf("line %d is %d cells wide (pane 100): %.80s", i, w, xansi.Strip(l))
		}
	}
	if d.m.vp.XOffset() != 0 {
		t.Fatal("scrolled sideways")
	}
}


// The subagent overlay is a pane too: its header carries the whole
// task and its boxes hold whatever the child printed.
func TestOverlayLinesFitThePane(t *testing.T) {
	d := newDrv(t, 60, 24, cfgWith(t, nil, nil, nil))
	long := strings.Repeat("y", 200)
	d.feed(eventMsg(Event{Kind: "user", Text: "go"}))
	d.feed(eventMsg(Event{Kind: "sub:start", Text: "Explore " + long, Data: map[string]any{"worker": 1}}))
	d.feed(eventMsg(Event{Kind: "sub:result", Text: "# " + strings.Repeat("-", 150), Data: map[string]any{"worker": 1}}))
	for bi := range d.m.blocks {
		b := &d.m.blocks[bi]
		if b.kind != "spawn" || b.sub == nil {
			continue
		}
		for i, l := range strings.Split(d.m.fit(d.m.subTranscript(b, d.cfgp.Load())), "\n") {
			if w := xansi.StringWidth(l); w > 60 {
				t.Fatalf("overlay line %d is %d wide: %.80s", i, w, xansi.Strip(l))
			}
		}
		return
	}
	t.Fatal("no spawn block with a sub transcript")
}
