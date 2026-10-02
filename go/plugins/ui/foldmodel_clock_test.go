package ui

import (
	"strings"
	"testing"
	"testing/synctest"
	"time"
)

func TestFoldModelRunningCardClock(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	now := time.Unix(0, 0)
	d.m.spawnNow = func() time.Time { return now }
	foldModelCheckClock(t, d, func() { now = now.Add(time.Second) })
}

// The production clock must have the same elapsed behavior when no
// clock is injected. Synctest advances time without a wall-clock wait.
func TestFoldModelRunningCardDefaultClock(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		foldModelCheckClock(t, defaultDrv(t), func() { time.Sleep(time.Second) })
	})
}

// A running card deliberately changes when its clock advances, including
// adding an elapsed chip at one second. The fold property's identity
// check must compare frames at the same time, not normalize clock text:
// normalization missed the first chip and could hide body text changes.
func foldModelCheckClock(t *testing.T, d *drv, advance func()) {
	t.Helper()
	d.feed(eventMsg{Kind: "sub:start", Text: "task", Data: map[string]any{"worker": 1}})
	d.feed(eventMsg{Kind: "sub:code", Text: "code", Data: map[string]any{"worker": 1}})
	d.event("thinking", "one\ntwo")
	d.m.toggleBlock(0)
	d.feed(keyTab())
	if d.m.focusID != d.m.blocks[1].id {
		t.Fatal("thinking block is not focused")
	}

	for i, elapsed := range []string{"", " · 1s", " · 2s"} {
		if i > 0 {
			advance()
		}
		d.m.refresh()
		want := "▾ ⠋ subagent 1 · running · 1 call" + elapsed
		if got := stripANSI(d.m.lines[0]); got != want {
			t.Fatalf("after %ds: running card header %q, want %q", i, got, want)
		}
		before := strings.Join(d.m.lines, "\n")
		collapsed := d.m.blocks[1].collapsed
		d.feed(keyEnter())
		if d.m.blocks[1].collapsed == collapsed {
			t.Fatalf("after %ds: first toggle did not change thinking block state", i)
		}
		d.feed(keyEnter())
		if d.m.blocks[1].collapsed != collapsed {
			t.Fatalf("after %ds: toggling twice changed thinking block state", i)
		}
		if after := strings.Join(d.m.lines, "\n"); after != before {
			t.Fatalf("after %ds: toggling twice changed the transcript at the same time:\n--- before\n%s\n--- after\n%s", i, stripANSI(before), stripANSI(after))
		}
	}

	d.feed(eventMsg{Kind: "sub:done", Data: map[string]any{"worker": 1}})
	if got, want := stripANSI(d.m.lines[0]), "▾ ✔ subagent 1 · done · 1 call · 2s"; got != want {
		t.Fatalf("finished card header %q, want %q", got, want)
	}
	before := strings.Join(d.m.lines, "\n")
	advance()
	d.m.refresh()
	if after := strings.Join(d.m.lines, "\n"); after != before {
		t.Fatalf("finished card's elapsed time changed:\n--- before\n%s\n--- after\n%s", stripANSI(before), stripANSI(after))
	}
}
