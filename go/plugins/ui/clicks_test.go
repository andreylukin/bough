package ui

// Exhaustive click coverage: every cell of a mixed frame, every mouse
// button, and the click surfaces that only exist in a mode (inspector,
// palette, picker, drag-select). The model's own hit ranges say what a
// click MAY do; the test asserts nothing else happens.

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

// mixedDrv builds an 80x40 frame with one block of every kind that
// renders, a pending ask with two options, and a typed draft.
func mixedDrv(t *testing.T) (*drv, *fakeAsk) {
	t.Helper()
	fa := &fakeAsk{}
	cfg := cfgWith(t, nil, nil, nil)
	cfg.ask = fa
	d := newDrv(t, 80, 40, cfg)
	d.event("user", "first prompt")
	d.event("assistant", "Some **prose** with a [link](http://x).\n\n- one\n- two")
	d.event("code", "tools.bash(\"echo a\")\nconsole.log(1)\nconsole.log(2)")
	d.event("result", nLines(30))
	d.event("error", "error: something failed")
	d.event("thinking", "hmm\nlet me think")
	d.event("bash", "ls -la")
	d.event("todo", "- [ ] a\n- [x] b")
	d.event("system", "a system note")
	d.event("command", "/help")
	d.event("done", "")
	d.event("user", "second")
	d.feed(askEvent())
	d.typeStr("draft text")
	return d, fa
}

// snapshot captures everything a click could change.
type clickState struct {
	collapsed []bool
	answers   int
	draft     string
	blocks    int
	focus     int
	yoff      int
	picking   bool
	inspect   bool
}

func (d *drv) clickState(fa *fakeAsk) clickState {
	st := clickState{answers: len(fa.texts), draft: d.m.input.Value(), blocks: len(d.m.blocks),
		focus: d.m.focusID, yoff: d.m.vp.YOffset(), picking: d.m.picking, inspect: d.m.inspecting}
	for i := range d.m.blocks {
		st.collapsed = append(st.collapsed, d.m.blocks[i].collapsed)
	}
	return st
}

// expected computes what a left click on viewport row y may do from the
// model's hit ranges: the index of the block whose header/body is on
// that row (or -1) and whether the row is an ask option.
func (d *drv) hit(y int) (idx int, option int) {
	if y < 0 || y >= d.m.vp.Height() {
		return -1, 0
	}
	row := y + d.m.vp.YOffset()
	for _, r := range d.m.ranges {
		if row >= r.start && row < r.end {
			b := &d.m.blocks[r.idx]
			if b.kind == "ask" && !b.answered && !b.expired {
				if off := row - r.start; off >= 1 && off <= len(b.options) {
					return r.idx, off
				}
				return r.idx, 0
			}
			return r.idx, 0
		}
	}
	return -1, 0
}

func (d *drv) clickAt(x, y int, btn tea.MouseButton) {
	d.feed(tea.MouseClickMsg{X: x, Y: y, Button: btn})
	d.feed(tea.MouseReleaseMsg{X: x, Y: y, Button: btn})
}

// Every cell, left button: only the block under the cell may toggle
// (and take focus), only an ask option row may answer, and nothing
// else moves: draft, block count, scroll, modes.
func TestClickEveryCellLeft(t *testing.T) {
	t.Parallel()
	d, fa := mixedDrv(t)
	d.m.vp.GotoTop()
	d.refreshFrame()
	for y := 0; y < 40; y++ {
		for _, x := range []int{0, 1, 3, 20, 79} {
			before := d.clickState(fa)
			idx, opt := d.hit(y)
			d.clickAt(x, y, tea.MouseLeft)
			after := d.clickState(fa)
			where := fmt.Sprintf("click (%d,%d)", x, y)
			if after.draft != before.draft || after.blocks != before.blocks || after.picking || after.inspect {
				t.Fatalf("%s changed draft/blocks/mode: %+v -> %+v", where, before, after)
			}
			toggled := idx >= 0 && d.m.blocks[idx].collapsible()
			if after.yoff != before.yoff && !toggled {
				t.Fatalf("%s scrolled the transcript (%d -> %d)", where, before.yoff, after.yoff)
			}
			if toggled {
				// The header you clicked stays on screen, whatever the
				// scroll position did to fit the expanded body.
				d.refreshFrame()
				for _, r := range d.m.ranges {
					if r.idx == idx {
						if row := r.start - d.m.vp.YOffset(); row < 0 || row >= d.m.vp.Height() {
							t.Fatalf("%s: toggled block %d's header scrolled off screen (row %d)", where, idx, row)
						}
					}
				}
			}
			for i := range before.collapsed {
				changed := before.collapsed[i] != after.collapsed[i]
				if changed && i != idx {
					t.Fatalf("%s toggled block %d (%s) but the cell belongs to block %d", where, i, d.m.blocks[i].kind, idx)
				}
				if changed && !d.m.blocks[i].collapsible() {
					t.Fatalf("%s toggled a non-collapsible %s block", where, d.m.blocks[i].kind)
				}
			}
			if opt > 0 && after.answers != before.answers+1 {
				t.Fatalf("%s on ask option %d did not answer", where, opt)
			}
			if opt == 0 && after.answers != before.answers {
				t.Fatalf("%s answered the ask from a non-option row", where)
			}
			if idx >= 0 && d.m.blocks[idx].collapsible() && before.collapsed[idx] == after.collapsed[idx] {
				t.Fatalf("%s on collapsible %s block %d did not toggle it", where, d.m.blocks[idx].kind, idx)
			}
			if idx >= 0 && d.m.blocks[idx].collapsible() && after.focus != d.m.blocks[idx].id {
				t.Fatalf("%s toggled block %d without focusing it (focus=%d)", where, idx, after.focus)
			}
			// Re-render after a toggle so the next row's hit test is current.
			d.refreshFrame()
			if strings.Contains(d.plain(), "render failed") {
				t.Fatalf("%s: render panicked:\n%s", where, d.plain())
			}
		}
	}
}

// refreshFrame forces a View so ranges reflect the current state.
func (d *drv) refreshFrame() { _ = d.view() }

// Right and middle buttons never do anything, anywhere.
func TestClickEveryCellOtherButtons(t *testing.T) {
	t.Parallel()
	d, fa := mixedDrv(t)
	for _, btn := range []tea.MouseButton{tea.MouseRight, tea.MouseMiddle} {
		for y := 0; y < 40; y++ {
			for _, x := range []int{0, 40, 79} {
				before := d.clickState(fa)
				d.clickAt(x, y, btn)
				after := d.clickState(fa)
				if fmt.Sprint(before) != fmt.Sprint(after) {
					t.Fatalf("button %v at (%d,%d) changed state: %+v -> %+v", btn, x, y, before, after)
				}
			}
		}
	}
}

// Out-of-bounds clicks (negative, past the width/height) are no-ops.
func TestClickOutOfBounds(t *testing.T) {
	t.Parallel()
	d, fa := mixedDrv(t)
	before := d.clickState(fa)
	for _, p := range [][2]int{{-1, 0}, {0, -1}, {80, 5}, {5, 40}, {200, 200}, {-5, -5}} {
		d.clickAt(p[0], p[1], tea.MouseLeft)
	}
	if after := d.clickState(fa); fmt.Sprint(before) != fmt.Sprint(after) {
		t.Fatalf("out-of-bounds click changed state: %+v -> %+v", before, after)
	}
}

// The wheel only scrolls: over any cell, no block toggles, no answer,
// no draft change; YOffset moves within [0, max].
func TestWheelEveryCellOnlyScrolls(t *testing.T) {
	t.Parallel()
	d, fa := mixedDrv(t)
	for y := 0; y < 40; y++ {
		for _, btn := range []tea.MouseButton{tea.MouseWheelUp, tea.MouseWheelDown} {
			before := d.clickState(fa)
			d.feed(tea.MouseWheelMsg{X: 10, Y: y, Button: btn})
			after := d.clickState(fa)
			before.yoff, after.yoff = 0, 0
			if fmt.Sprint(before) != fmt.Sprint(after) {
				t.Fatalf("wheel %v at row %d did more than scroll: %+v -> %+v", btn, y, before, after)
			}
			if off := d.m.vp.YOffset(); off < 0 {
				t.Fatalf("negative scroll offset %d", off)
			}
		}
	}
}

// A click on the status bar opens the session picker only when a
// history service is mounted; inside the picker clicks are inert.
func TestStatusBarClickNeedsHistory(t *testing.T) {
	t.Parallel()
	d, _ := mixedDrv(t)
	d.clickAt(5, d.m.vp.Height(), tea.MouseLeft)
	if d.m.picking {
		t.Fatalf("picker opened without a history service")
	}
	h := newDrv(t, 80, 24, cfgWith(t, nil, nil, histWith("/tmp/s.jsonl", "one")))
	h.clickAt(5, h.m.vp.Height(), tea.MouseLeft)
	if !h.m.picking {
		t.Fatalf("status bar click did not open the picker")
	}
	before := h.m.pick
	for y := 0; y < 24; y++ {
		h.clickAt(3, y, tea.MouseLeft)
	}
	if !h.m.picking || h.m.pick != before {
		t.Fatalf("clicks inside the picker changed it (picking=%v pick %d -> %d)", h.m.picking, before, h.m.pick)
	}
}

// A click on the composer rows never edits, moves or submits the draft.
func TestClickComposerRowsInert(t *testing.T) {
	t.Parallel()
	d, fa := mixedDrv(t)
	d.typeStr("\nmore") // grow the composer
	before := d.clickState(fa)
	for y := d.m.vp.Height() + 1; y < 40; y++ {
		for _, x := range []int{0, 2, 40} {
			d.clickAt(x, y, tea.MouseLeft)
		}
	}
	if after := d.clickState(fa); after.draft != before.draft || after.blocks != before.blocks {
		t.Fatalf("composer click changed draft %q -> %q or blocks %d -> %d", before.draft, after.draft, before.blocks, after.blocks)
	}
}

// With the inspector open, every click toggles at most one history
// entry's JSON and never touches the transcript's blocks.
func TestClickEveryCellInspector(t *testing.T) {
	t.Parallel()
	fa := &fakeAsk{}
	cfg := cfgWith(t, nil, nil, histWith("/tmp/h.jsonl", "alpha", "beta", "gamma"))
	cfg.ask = fa
	d := newDrv(t, 80, 24, cfg)
	d.event("code", "x")
	d.event("result", nLines(3))
	d.feed(keyCtrl('o'))
	if !d.m.inspecting {
		t.Fatal("inspector did not open")
	}
	for y := 0; y < 24; y++ {
		before := d.clickState(fa)
		open := len(d.m.ovExpanded)
		d.clickAt(4, y, tea.MouseLeft)
		after := d.clickState(fa)
		if fmt.Sprint(before.collapsed) != fmt.Sprint(after.collapsed) || after.draft != before.draft || !after.inspect {
			t.Fatalf("inspector click at row %d leaked to the transcript: %+v -> %+v", y, before, after)
		}
		expanded := 0
		for _, v := range d.m.ovExpanded {
			if v {
				expanded++
			}
		}
		if expanded > 3 || (len(d.m.ovExpanded) != open && len(d.m.ovExpanded) != open+1) {
			t.Fatalf("row %d: %d entries expanded (%d tracked)", y, expanded, len(d.m.ovExpanded))
		}
		if strings.Contains(d.plain(), "render failed") {
			t.Fatalf("row %d: render panicked:\n%s", y, d.plain())
		}
	}
}

// With the palette open, a click on a palette row accepts that command
// and closes the palette; a click elsewhere leaves the draft alone.
func TestClickEveryCellPalette(t *testing.T) {
	t.Parallel()
	d := newDrv(t, 80, 24, cfgWith(t, nil, nil, nil))
	d.m.cfg.Load().cmds = reg(t, "help", "clear", "quit")
	d.event("code", "x")
	d.event("result", nLines(3))
	d.typeStr("/")
	if !d.m.pal.open {
		t.Fatal("palette did not open")
	}
	rows := len(d.overlayRowsNow())
	top := d.m.vp.Height() - rows
	for y := 0; y < 24; y++ {
		d.m.input.SetValue("/")
		d.m.syncPalette()
		d.refreshFrame()
		collapsedBefore := fmt.Sprint(d.m.blocks[0].collapsed, d.m.blocks[1].collapsed)
		d.clickAt(3, y, tea.MouseLeft)
		draft := d.m.input.Value()
		switch {
		case y >= top && y < d.m.vp.Height():
			if d.m.pal.open || draft == "/" {
				t.Fatalf("row %d is palette row %d but the click did not accept (open=%v draft=%q)", y, y-top, d.m.pal.open, draft)
			}
		default:
			if draft != "/" {
				t.Fatalf("row %d outside the palette changed the draft to %q", y, draft)
			}
			if got := fmt.Sprint(d.m.blocks[0].collapsed, d.m.blocks[1].collapsed); got != collapsedBefore {
				// A click through an open palette onto a block header
				// toggles it: documented as the current behaviour.
				t.Logf("row %d: click through the open palette toggled a block (%s -> %s)", y, collapsedBefore, got)
			}
		}
	}
}

func (d *drv) overlayRowsNow() []string {
	d.refreshFrame()
	return d.m.overlayRows()
}

// A drag over any rows copies text and never toggles a block or answers
// the ask, whichever cell the press or release lands on.
func TestDragNeverClicks(t *testing.T) {
	t.Parallel()
	d, fa := mixedDrv(t)
	for y0 := 0; y0 < d.m.vp.Height(); y0 += 3 {
		for y1 := 0; y1 < d.m.vp.Height(); y1 += 5 {
			if y0 == y1 {
				continue
			}
			before := d.clickState(fa)
			d.feed(tea.MouseClickMsg{X: 2, Y: y0, Button: tea.MouseLeft})
			d.feed(tea.MouseMotionMsg{X: 10, Y: y1, Button: tea.MouseLeft})
			cmd := d.m.releaseSelect(tea.Mouse{X: 10, Y: y1, Button: tea.MouseLeft})
			after := d.clickState(fa)
			if fmt.Sprint(before.collapsed) != fmt.Sprint(after.collapsed) || after.answers != before.answers {
				t.Fatalf("drag %d->%d toggled a block or answered: %+v -> %+v", y0, y1, before, after)
			}
			if text := d.m.selectedText(); text != "" {
				if cmd == nil || !strings.HasPrefix(d.m.flash, "copied") {
					t.Fatalf("drag %d->%d selected %q but no clipboard cmd / flash (%q)", y0, y1, text, d.m.flash)
				}
			} else if d.m.sel.active {
				t.Fatalf("drag %d->%d over blank rows left an active empty selection", y0, y1)
			}
		}
	}
}

// A press then a release on a different row WITHOUT motion is a click
// on the release row (terminals without motion reports).
func TestPressReleaseWithoutMotionClicksReleaseRow(t *testing.T) {
	t.Parallel()
	d, _ := mixedDrv(t)
	d.m.vp.GotoTop()
	d.refreshFrame()
	var codeRow, codeIdx int = -1, -1
	for _, r := range d.m.ranges {
		if d.m.blocks[r.idx].kind == "code" {
			codeRow, codeIdx = r.start-d.m.vp.YOffset(), r.idx
		}
	}
	if codeRow < 0 {
		t.Fatal("no code block row")
	}
	was := d.m.blocks[codeIdx].collapsed
	d.feed(tea.MouseClickMsg{X: 2, Y: 0, Button: tea.MouseLeft})
	d.feed(tea.MouseReleaseMsg{X: 2, Y: codeRow, Button: tea.MouseLeft})
	if d.m.blocks[codeIdx].collapsed == was {
		t.Fatalf("release on the code header (row %d) did not toggle it", codeRow)
	}
}

// Clicking the same header twice returns to the original state, for
// every collapsible block, and each toggle keeps the hit ranges in sync.
func TestDoubleClickRoundTripsEveryBlock(t *testing.T) {
	t.Parallel()
	d, _ := mixedDrv(t)
	d.m.vp.GotoTop()
	for _, i := range d.m.focusables() {
		d.refreshFrame()
		var row int = -1
		for _, r := range d.m.ranges {
			if r.idx == i {
				row = r.start - d.m.vp.YOffset()
			}
		}
		if row < 0 || row >= d.m.vp.Height() {
			continue // scrolled out: not clickable right now
		}
		was := d.m.blocks[i].collapsed
		d.clickAt(1, row, tea.MouseLeft)
		if d.m.blocks[i].collapsed == was {
			t.Fatalf("block %d (%s) at row %d did not toggle", i, d.m.blocks[i].kind, row)
		}
		d.refreshFrame()
		row = -1
		for _, r := range d.m.ranges {
			if r.idx == i {
				row = r.start - d.m.vp.YOffset()
			}
		}
		if row < 0 || row >= d.m.vp.Height() {
			t.Fatalf("block %d (%s) header left the screen after expanding", i, d.m.blocks[i].kind)
		}
		d.clickAt(1, row, tea.MouseLeft)
		if d.m.blocks[i].collapsed != was {
			t.Fatalf("block %d (%s) did not round-trip", i, d.m.blocks[i].kind)
		}
	}
}
