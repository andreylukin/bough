package ui

import (
	"regexp"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

// reverseRe matches an SGR sequence carrying the reverse-video attribute.
var reverseRe = regexp.MustCompile(`\x1b\[[0-9;]*\b7[;m]`)

// Press-drag-release over the transcript highlights the swept text,
// copies its plain form (the release returns the clipboard command),
// and the flash names what was copied; the next press clears it.
func TestDragSelectsHighlightsAndCopies(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("assistant", "alpha beta gamma\ndelta epsilon")
	d.event("done", "")
	row := frameRow(d, "alpha beta gamma")
	d.feed(tea.MouseClickMsg{X: 8, Y: row, Button: tea.MouseLeft})
	d.feed(tea.MouseMotionMsg{X: 6, Y: row + 1, Button: tea.MouseLeft})
	if !d.m.sel.active {
		t.Fatal("dragging should start a selection")
	}
	if got := d.m.selectedText(); got != "beta gamma\n  delta" {
		t.Fatalf("selectedText = %q", got)
	}
	// The composer cursor is reverse video too, so look at the row itself.
	hl := d.m.highlight(d.m.lines, d.m.cfg.Load())
	r := row + d.m.vp.YOffset()
	if !reverseRe.MatchString(hl[r]) || reverseRe.MatchString(d.m.lines[r]) {
		t.Errorf("selected span should render in reverse video:\n%q", hl[r])
	}
	next, cmd := d.m.Update(tea.MouseReleaseMsg{X: 6, Y: row + 1, Button: tea.MouseLeft})
	d.m = next.(model)
	if cmd == nil {
		t.Fatal("release should return the clipboard command")
	}
	if !strings.HasPrefix(d.m.flash, "copied 2 lines") {
		t.Errorf("flash = %q", d.m.flash)
	}
	if !d.m.sel.active {
		t.Error("the highlight should stay until the next press")
	}
	d.feed(tea.MouseClickMsg{X: 0, Y: row, Button: tea.MouseLeft})
	if d.m.sel.active || reverseRe.MatchString(d.m.highlight(d.m.lines, d.m.cfg.Load())[r]) {
		t.Error("a new press should clear the highlight")
	}
}
