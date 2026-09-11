package ui

import (
	"fmt"
	"strings"
	"testing"
)

// longTranscript feeds n replayed-shaped turns into a fresh driver.
func longTranscript(t *testing.T, n int) *drv {
	d := newDrv(t, 100, 30, cfgWith(t, nil, nil, nil))
	for i := 1; i <= n; i++ {
		code := fmt.Sprintf("console.log(%q)", fmt.Sprintf("step %d", i))
		d.m.addEvent(Event{Kind: "user", Text: fmt.Sprintf("do step %d", i)})
		d.m.addEvent(Event{Kind: "code", Text: code})
		d.m.addEvent(Event{Kind: "result", Text: fmt.Sprintf("step %d", i)})
		d.m.addEvent(Event{Kind: "assistant", Text: fmt.Sprintf("step **%d** done", i)})
		d.m.addEvent(Event{Kind: "done"})
	}
	return d
}

// Every event refreshes the transcript. Re-rendering every block each
// time made a refresh cost O(turns) in lipgloss and glamour work, until
// a long session's screen fell seconds behind the loop. An unchanged
// block reuses its render: planting a marker in the cache shows up in
// the transcript, so refresh did not render that block again.
func TestRefreshReusesUnchangedBlocks(t *testing.T) {
	d := longTranscript(t, 20)
	if len(d.m.parts) == 0 {
		t.Fatal("refresh cached no block renders")
	}
	for id, e := range d.m.parts {
		e.part = fmt.Sprintf("CACHED-%d", id)
		d.m.parts[id] = e
	}
	d.m.refresh()
	got := d.m.vp.GetContent()
	if n := strings.Count(got, "CACHED-"); n != len(d.m.parts) {
		t.Errorf("refresh re-rendered unchanged blocks: %d of %d cached renders reused", n, len(d.m.parts))
	}
}

// The cached render matches a fresh one after the changes that alter
// a block: a toggle, a focus move, a text edit, a resize.
func TestRefreshCacheMatchesFreshRender(t *testing.T) {
	d := longTranscript(t, 5)
	fresh := func() string {
		d.m.parts = map[int]partEntry{}
		d.m.refresh()
		return d.m.vp.GetContent()
	}
	check := func(what string) {
		t.Helper()
		d.m.refresh()
		got := d.m.vp.GetContent()
		if want := fresh(); got != want {
			t.Errorf("%s: cached transcript differs from a fresh render\n got: %q\nwant: %q", what, got, want)
		}
	}
	check("steady")
	for i := range d.m.blocks {
		if d.m.blocks[i].collapsible() {
			d.m.blocks[i].collapsed = !d.m.blocks[i].collapsed
			d.m.focusID = d.m.blocks[i].id
			break
		}
	}
	check("toggle + focus")
	d.m.blocks[0].text += " edited"
	check("text edit")
	d.m.resize(60, 20)
	check("resize")
}
