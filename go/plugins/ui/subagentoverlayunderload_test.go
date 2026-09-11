package ui

// Surface "subagent-overlay-under-load": a spawnAll of six subagents
// whose sub:* events arrive interleaved while the user has one child's
// transcript open in the overlay, scrolls it, resizes the pane, and
// esc's back. The replay harness never spawns (sub:* entries only
// render on resume), so the concurrent stream is fed here through the
// in-process driver, the same tea path the loop's events take.

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

const subagentOverlayUnderLoadN = 6

// subagentOverlayUnderLoadEv feeds one event for a worker.
func subagentOverlayUnderLoadEv(d *drv, kind, text string, worker int, extra map[string]any) {
	data := map[string]any{"worker": float64(worker)}
	for k, v := range extra {
		data[k] = v
	}
	d.feed(eventMsg(Event{Kind: kind, Text: text, Data: data}))
}

// subagentOverlayUnderLoadRound is one step of every child, interleaved:
// each worker runs call r and gets its output.
func subagentOverlayUnderLoadRound(d *drv, r int) {
	for w := 1; w <= subagentOverlayUnderLoadN; w++ {
		subagentOverlayUnderLoadEv(d, "sub:code", fmt.Sprintf("tools.bash(\"step-%d-%d\")", w, r), w, nil)
	}
	for w := subagentOverlayUnderLoadN; w >= 1; w-- {
		subagentOverlayUnderLoadEv(d, "sub:result", fmt.Sprintf("out-%d-%d\n%s", w, r, nLines(8)), w, nil)
	}
}

// subagentOverlayUnderLoadFocus tabs until worker's card is focused.
func subagentOverlayUnderLoadFocus(t *testing.T, d *drv, worker int) {
	t.Helper()
	for range 40 {
		d.feed(keyTab())
		if i := d.m.focusedSpawn(); i >= 0 && d.m.blocks[i].sub.worker == worker {
			return
		}
	}
	t.Fatalf("tab never focused subagent %d:\n%s", worker, d.plain())
}

// subagentOverlayUnderLoadCalls is each worker's call count from the model.
func subagentOverlayUnderLoadCalls(d *drv) map[int]int {
	out := map[int]int{}
	for i := range d.m.blocks {
		if b := &d.m.blocks[i]; b.kind == "spawn" && b.sub != nil {
			out[b.sub.worker] = b.sub.calls
		}
	}
	return out
}

func TestSubagentOverlayUnderLoad(t *testing.T) {
	d := newDrv(t, 80, 16, cfgWith(t, nil, nil, nil))
	d.feed(windowSize(80, 16))
	d.typeStr("fan out")
	d.feed(keyEnter())
	d.event("assistant", "Fanning out.\n"+nLines(30)+"\n```js\ntools.spawnAll([...])\n```")
	d.event("code", "tools.spawnAll([...])")
	for w := 1; w <= subagentOverlayUnderLoadN; w++ {
		subagentOverlayUnderLoadEv(d, "sub:start", fmt.Sprintf("task number %d: audit package p%d", w, w), w, nil)
	}
	subagentOverlayUnderLoadRound(d, 1)

	const dive = 3
	subagentOverlayUnderLoadFocus(t, d, dive)
	d.feed(keyCtrl('o'))
	if !d.m.inspecting || d.m.diving == 0 {
		t.Fatalf("ctrl+o did not open subagent %d's transcript:\n%s", dive, d.plain())
	}
	if !strings.Contains(d.plain(), fmt.Sprintf("subagent %d", dive)) {
		t.Fatalf("overlay does not name subagent %d:\n%s", dive, d.plain())
	}

	t.Run("cards update while overlay open", func(t *testing.T) {
		before := subagentOverlayUnderLoadCalls(d)
		subagentOverlayUnderLoadRound(d, 2)
		after := subagentOverlayUnderLoadCalls(d)
		for w := 1; w <= subagentOverlayUnderLoadN; w++ {
			if after[w] != before[w]+1 {
				t.Errorf("subagent %d calls %d -> %d under the overlay, want +1", w, before[w], after[w])
			}
		}
		// The dived child's overlay is live: its newest call shows once
		// scrolled to the end.
		d.m.overlay.GotoBottom()
		p := d.plain()
		if !strings.Contains(p, fmt.Sprintf("out-%d-2", dive)) {
			t.Errorf("overlay did not pick up subagent %d's new output:\n%s", dive, p)
		}
		if strings.Contains(p, "out-1-2") || strings.Contains(p, "out-6-2") {
			t.Errorf("overlay leaked a sibling's transcript:\n%s", p)
		}
	})

	t.Run("scroll and resize keep the overlay", func(t *testing.T) {
		d.press(keyPgUp())
		d.press(keyPgUp())
		up := d.m.overlay.YOffset()
		subagentOverlayUnderLoadRound(d, 3)
		if got := d.m.overlay.YOffset(); got != up {
			t.Errorf("a sibling event moved the scrolled overlay: offset %d -> %d", up, got)
		}
		d.press(keyPgDown())
		for _, sz := range [][2]int{{50, 10}, {120, 30}, {80, 16}} {
			d.feed(windowSize(sz[0], sz[1]))
			if !d.m.inspecting || d.m.diving == 0 {
				t.Fatalf("resize to %dx%d closed the dive", sz[0], sz[1])
			}
			for i, l := range strings.Split(d.plain(), "\n") {
				if w := len([]rune(l)); w > sz[0] {
					t.Errorf("%dx%d: row %d is %d wide:\n%s", sz[0], sz[1], i, w, d.plain())
				}
			}
			if !strings.Contains(d.plain(), fmt.Sprintf("subagent %d", dive)) && d.m.overlay.YOffset() == 0 {
				t.Errorf("%dx%d: overlay lost its subagent header:\n%s", sz[0], sz[1], d.plain())
			}
		}
	})

	// Every child finishes while the overlay is still open.
	for w := 1; w <= subagentOverlayUnderLoadN; w++ {
		subagentOverlayUnderLoadEv(d, "sub:assistant", fmt.Sprintf("Status: ok\nFindings: report-%d", w), w, nil)
		subagentOverlayUnderLoadEv(d, "sub:done", "", w, map[string]any{"status": "ok", "steps": float64(3)})
	}
	d.m.overlay.GotoBottom()
	if p := d.plain(); !strings.Contains(p, "✔ done") {
		t.Errorf("dived overlay does not show the child finished:\n%s", p)
	}

	t.Run("esc returns to spawner at bottom", func(t *testing.T) {
		d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
		if d.m.inspecting || d.m.diving != 0 {
			t.Fatalf("esc did not close the dive (inspecting=%v diving=%d)", d.m.inspecting, d.m.diving)
		}
		if !d.m.vp.AtBottom() {
			t.Errorf("back from the overlay but the transcript is not at the bottom (offset %d):\n%s", d.m.vp.YOffset(), d.plain())
		}
	})

	t.Run("no card stuck spinning and done count", func(t *testing.T) {
		d.event("result", "[6 subagents finished]")
		d.event("assistant", "All six finished.")
		d.event("done", "")
		if d.m.hasRunningSpawn() {
			t.Errorf("a card is still running after every sub:done")
		}
		// Unfold nothing: collapsed heads are one row each; make them
		// all fit so the count reads off the screen.
		d.feed(windowSize(120, 60))
		d.m.vp.GotoBottom()
		p := d.plain()
		if spinnerFrameIn(p) {
			t.Errorf("a spinner frame is still on screen:\n%s", p)
		}
		done := 0
		for _, l := range strings.Split(p, "\n") {
			if strings.Contains(l, "subagent ") && strings.Contains(l, "✔") && strings.Contains(l, "done") {
				done++
			}
			if strings.Contains(l, "subagent ") && strings.Contains(l, "running") {
				t.Errorf("card still says running: %q", l)
			}
		}
		if done != subagentOverlayUnderLoadN {
			t.Errorf("%d cards read done, want %d:\n%s", done, subagentOverlayUnderLoadN, p)
		}
		for w := 1; w <= subagentOverlayUnderLoadN; w++ {
			if c := subagentOverlayUnderLoadCalls(d)[w]; c != 3 {
				t.Errorf("subagent %d has %d calls, want 3", w, c)
			}
		}
	})
}

// TestSubagentOverlayUnderLoadEscNoResize: the esc-back half without the
// resize bug in the way. Six children stream and finish under a scrolled
// dive; esc lands on the spawner's transcript pinned at the bottom.
func TestSubagentOverlayUnderLoadEscNoResize(t *testing.T) {
	d := newDrv(t, 80, 16, cfgWith(t, nil, nil, nil))
	d.feed(windowSize(80, 16))
	d.event("assistant", "Fanning out.\n"+nLines(30)+"\n```js\ntools.spawnAll([...])\n```")
	d.event("code", "tools.spawnAll([...])")
	for w := 1; w <= subagentOverlayUnderLoadN; w++ {
		subagentOverlayUnderLoadEv(d, "sub:start", fmt.Sprintf("task number %d", w), w, nil)
	}
	subagentOverlayUnderLoadRound(d, 1)
	subagentOverlayUnderLoadFocus(t, d, 2)
	d.feed(keyCtrl('o'))
	d.press(keyPgUp())
	for r := 2; r <= 3; r++ {
		subagentOverlayUnderLoadRound(d, r)
	}
	for w := 1; w <= subagentOverlayUnderLoadN; w++ {
		subagentOverlayUnderLoadEv(d, "sub:done", "", w, map[string]any{"status": "ok", "steps": float64(3)})
	}
	d.press(tea.KeyPressMsg{Code: tea.KeyEscape})
	if d.m.inspecting || d.m.diving != 0 {
		t.Fatalf("esc did not close the dive")
	}
	if !d.m.vp.AtBottom() {
		t.Fatalf("esc returned off the bottom (offset %d):\n%s", d.m.vp.YOffset(), d.plain())
	}
	if p := d.plain(); !strings.Contains(p, "subagent 6") {
		t.Errorf("the spawner's newest card is not on screen after esc:\n%s", p)
	}
	if d.m.hasRunningSpawn() {
		t.Errorf("a card is still running after every sub:done")
	}
}
