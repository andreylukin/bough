package ui

// Model-based folding property: random loop events, fold toggles
// (click, keys, collapse_all / expand_all), scrolling and resizes
// against the real model, with a reference model of each block's
// collapsed state checked after every step, plus render invariants:
// a closed block is one "▸ … (N lines)" row, an open one shows all of
// its text inside the pane, a closed fold is one "▸ N steps" row.
//
//	go test ./plugins/ui -run FoldModel -rapid.checks=300
//	BOUGH_FOLD_MODEL_LONG=1 go test ./plugins/ui -run FoldModel -rapid.checks=5000   (120-step sequences)
//	BOUGH_KNOWN_FOLD_MODEL=1 also runs the cases pinned on known bugs

import (
	"fmt"
	"os"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"pgregory.net/rapid"
)

func foldModelKnown(t *testing.T, bug string) {
	t.Helper()
	if os.Getenv("BOUGH_KNOWN_FOLD_MODEL") == "" {
		t.Skip("known bug (BOUGH_KNOWN_FOLD_MODEL=1 to run): " + bug)
	}
}

// foldModelText: plain words and newlines, so the rendered body can be
// compared with the stored text (no markdown, no sanitizing), plus an
// occasional unbroken run that must hard-wrap.
func foldModelText() *rapid.Generator[string] {
	return rapid.Custom(func(t *rapid.T) string {
		n := rapid.IntRange(1, 12).Draw(t, "lines")
		rows := make([]string, n)
		for i := range rows {
			if rapid.IntRange(0, 9).Draw(t, "long") == 0 {
				rows[i] = rapid.StringMatching(`[a-f0-9]{60,250}`).Draw(t, "run")
				continue
			}
			rows[i] = strings.Join(rapid.SliceOfN(rapid.StringMatching(`[a-z0-9]{1,9}`), 1, 8).Draw(t, "words"), " ")
		}
		return strings.Join(rows, "\n")
	})
}

var foldModelKinds = []string{
	"user", "assistant-delta", "assistant", "thinking-delta", "thinking",
	"code", "code", "result", "result", "error", "system", "job", "todo",
	"sub:start", "sub:code", "sub:done", "done", "cancelled", "steer", "run",
}

type foldModelState struct {
	w, h      int
	collapsed map[int]bool // reference: block id -> collapsed
}

// foldModelSync adopts blocks the model has not seen (new events) and
// forgets removed ones; then every known block must agree.
func foldModelSync(rt *rapid.T, d *drv, ref *foldModelState, step string, adopt bool) {
	seen := map[int]bool{}
	for _, b := range d.m.blocks {
		seen[b.id] = true
		want, ok := ref.collapsed[b.id]
		if !ok || adopt && b.kind == "todo" { // todo events rewrite the last block in place
			ref.collapsed[b.id] = b.collapsed
			continue
		}
		if want != b.collapsed {
			rt.Fatalf("after %s: block %d (%s) collapsed=%v, reference says %v\n%s", step, b.id, b.kind, b.collapsed, want, d.plain())
		}
	}
	for id := range ref.collapsed {
		if !seen[id] {
			delete(ref.collapsed, id)
		}
	}
}

// foldModelBody strips box borders and all whitespace from rows.
func foldModelBody(rows []string) string {
	var sb strings.Builder
	for _, r := range rows {
		r = strings.TrimSpace(r)
		if strings.HasPrefix(r, "╭") || strings.HasPrefix(r, "╰") {
			continue
		}
		r = strings.TrimPrefix(r, "│")
		r = strings.TrimSuffix(r, "│")
		sb.WriteString(r)
	}
	return strings.Join(strings.Fields(sb.String()), "")
}

// foldModelCheck: render invariants of the current frame.
func foldModelCheck(rt *rapid.T, d *drv, ref *foldModelState, step string) {
	m := &d.m
	lines := make([]string, len(m.lines))
	for i, l := range m.lines {
		lines[i] = ansi.Strip(l)
		if w := ansi.StringWidth(l); w > m.width {
			rt.Fatalf("after %s: transcript row %d is %d wide in %d: %q", step, i, w, m.width, lines[i])
		}
	}
	closed := map[int]foldRun{}
	hidden := map[int]bool{}
	for _, r := range m.runs() {
		if r.to > len(m.blocks) || r.from >= r.to {
			rt.Fatalf("after %s: fold run [%d,%d) outside %d blocks", step, r.from, r.to, len(m.blocks))
		}
		if !r.open {
			closed[r.from] = r
			for i := r.from + 1; i < r.to; i++ {
				hidden[i] = true
			}
		}
	}
	drawn := map[int]int{}
	for _, rg := range m.ranges {
		if rg.end > len(lines) || rg.start >= rg.end {
			rt.Fatalf("after %s: range %+v outside %d lines", step, rg, len(lines))
		}
		rows := lines[rg.start:rg.end]
		if rg.fold {
			if !strings.HasPrefix(rows[0], "▾ ") {
				rt.Fatalf("after %s: open fold header %q lacks ▾", step, rows[0])
			}
			continue
		}
		drawn[rg.idx]++
		b := m.blocks[rg.idx]
		if hidden[rg.idx] {
			rt.Fatalf("after %s: block %d is inside a closed fold but drawn", step, rg.idx)
		}
		if _, ok := closed[rg.idx]; ok {
			if len(rows) != 1 || !strings.HasPrefix(rows[0], "▸ ") || !strings.Contains(rows[0], "step") {
				rt.Fatalf("after %s: closed fold at %d is not one ▸ steps row: %q", step, rg.idx, rows)
			}
			continue
		}
		switch b.kind {
		case "code", "result", "thinking":
		default:
			continue
		}
		n := strings.Count(b.text, "\n") + 1
		unit := "lines"
		if n == 1 {
			unit = "line"
		}
		count := fmt.Sprintf("(%d %s)", n, unit)
		// The header may be truncated at narrow widths; check what fits.
		if !strings.Contains(rows[0], count) && ansi.StringWidth(rows[0]) < m.width-1 {
			rt.Fatalf("after %s: %s header %q lacks %q", step, b.kind, rows[0], count)
		}
		if b.collapsed {
			if len(rows) != 1 || !strings.HasPrefix(rows[0], "▸ ") {
				rt.Fatalf("after %s: collapsed %s %d renders %d rows: %q", step, b.kind, b.id, len(rows), rows)
			}
			continue
		}
		if !strings.HasPrefix(rows[0], "▾ ") {
			rt.Fatalf("after %s: expanded %s header %q lacks ▾", step, b.kind, rows[0])
		}
		if got, want := foldModelBody(rows[1:]), strings.Join(strings.Fields(sanitizeText(b.text)), ""); got != want {
			rt.Fatalf("after %s: expanded %s %d body differs (width %d)\n got %q\nwant %q", step, b.kind, b.id, m.width, got, want)
		}
	}
	for i := range m.blocks {
		if drawn[i] > 1 {
			rt.Fatalf("after %s: block %d drawn %d times", step, i, drawn[i])
		}
		if drawn[i] == 0 && !hidden[i] {
			rt.Fatalf("after %s: block %d (%s) not drawn and not folded", step, i, m.blocks[i].kind)
		}
	}
}

func foldModelEvent(rt *rapid.T, d *drv) string {
	kind := rapid.SampledFrom(foldModelKinds).Draw(rt, "ev")
	text := foldModelText().Draw(rt, "text")
	switch kind {
	case "run": // a turn starts (submit's side effect on the model)
		d.m.running = true
		d.m.refresh()
	case "code":
		d.event(kind, `tools.bash("`+strings.ReplaceAll(text, "\n", " ")+`")`+"\n"+text)
	case "sub:start", "sub:code", "sub:done":
		d.feed(eventMsg{Kind: kind, Text: text, Data: map[string]any{"worker": 1}})
	case "done", "cancelled":
		d.event(kind, "")
	default:
		d.event(kind, text)
	}
	return "event " + kind
}

func foldModelRun(rt *rapid.T, t *testing.T, maxSteps int) {
	ref := &foldModelState{w: rapid.IntRange(20, 200).Draw(rt, "w0"), h: rapid.IntRange(6, 50).Draw(rt, "h0"), collapsed: map[int]bool{}}
	d := newDrv(t, ref.w, ref.h, cfgWith(t, nil, nil, nil))
	cfg := d.cfgp.Load()
	n := rapid.IntRange(1, maxSteps).Draw(rt, "n")
	for range n {
		var step string
		adopt := false
		switch rapid.IntRange(0, 11).Draw(rt, "op") {
		case 0, 1, 2, 3:
			focus := d.m.focusID
			step = foldModelEvent(rt, d)
			adopt = true
			// Focus is identity: a new block elsewhere never moves it.
			if focus >= 0 && d.m.focusID != focus {
				alive := false
				for _, b := range d.m.blocks {
					alive = alive || b.id == focus
				}
				if alive {
					rt.Fatalf("after %s: focus moved %d -> %d", step, focus, d.m.focusID)
				}
			}
		case 4: // keyboard walk
			if rapid.Bool().Draw(rt, "fwd") {
				d.feed(keyTab())
				step = "tab"
			} else {
				d.m.runAction("block_prev", "", cfg)
				step = "block_prev"
			}
		case 5, 6: // toggle the focused block (enter on an empty composer)
			step = "toggle"
			f := d.m.focusables()
			if len(f) == 0 || d.m.focusID < 0 || d.m.input.Value() != "" {
				continue
			}
			cur := -1
			for _, s := range f {
				if d.m.blocks[s.idx].id == d.m.focusID && s.fold == d.m.focusFold {
					cur = s.idx
				}
			}
			if cur < 0 {
				continue
			}
			wasBottom := d.m.vp.AtBottom()
			wasFold := d.m.focusFold // enter on an open fold's header refolds it, by design
			before := strings.Join(d.m.lines, "\n")
			beforeCol := map[int]bool{}
			for _, b := range d.m.blocks {
				beforeCol[b.id] = b.collapsed
			}
			d.feed(keyEnter())
			for _, b := range d.m.blocks {
				ref.collapsed[b.id] = b.collapsed // enter may fold/unfold a run
			}
			if !wasBottom && d.m.vp.AtBottom() && d.m.vp.TotalLineCount() > d.m.vp.Height() {
				// Contract (toggleBlock/setFold): the offset is kept, then
				// the toggled header is scrolled into view. Landing at the
				// bottom is fine only if that is where the header is.
				hdr, row := -1, cur
				for _, r := range d.m.runs() {
					if !r.open && cur > r.from && cur < r.to {
						row = r.from // folded away into a run: its row stands in
					}
				}
				for _, r := range d.m.ranges {
					if r.idx == row && (r.fold == d.m.focusFold || row != cur) {
						hdr = r.start
					}
				}
				// Known bug (TestFoldModelCollapseJoinsFold): a block folded
				// away has no range, toggleBlock's EnsureVisible finds
				// nothing, and the shrink clamps the view to the bottom.
				if hdr < d.m.vp.YOffset() {
					rt.Fatalf("toggle while scrolled up jumped to bottom, header row %d above offset %d", hdr, d.m.vp.YOffset())
				}
			}
			if rapid.Bool().Draw(rt, "twice") {
				// Toggling twice is the identity. From a closed fold the
				// first enter opens it (focus on its header) and the
				// second closes it again.
				joined := false
				for _, r := range d.m.runs() {
					joined = joined || !r.open && cur >= r.from && cur < r.to
				}
				joined = joined && !wasFold && !beforeCol[d.m.blocks[cur].id] && d.m.blocks[cur].collapsed
				d.feed(keyEnter())
				if joined {
					rt.Fatalf("collapsing block %d folded it into a run", d.m.blocks[cur].id)
				}
				refolded := false
				for _, b := range d.m.blocks {
					if b.collapsed != beforeCol[b.id] {
						// Contract (refold): folding an open run back closes
						// the rows opened inside it, and opening it again
						// leaves them closed.
						if wasFold && b.collapsed {
							refolded = true
						} else {
							rt.Fatalf("toggle twice changed block %d collapsed %v -> %v", b.id, beforeCol[b.id], b.collapsed)
						}
					}
					ref.collapsed[b.id] = b.collapsed
				}
				if after := strings.Join(d.m.lines, "\n"); !refolded && stripANSI(after) != stripANSI(before) {
					rt.Fatalf("toggle twice changed the transcript:\n--- before\n%s\n--- after\n%s", stripANSI(before), stripANSI(after))
				}
				step = "toggle twice"
			}
		case 7: // click a row of the transcript
			if len(d.m.ranges) == 0 {
				continue
			}
			rg := rapid.SampledFrom(d.m.ranges).Draw(rt, "range")
			y := rg.start - d.m.vp.YOffset()
			if y < 0 || y >= d.m.vp.Height() {
				continue
			}
			d.feed(tea.MouseClickMsg{X: 1, Y: y, Button: tea.MouseLeft})
			d.feed(tea.MouseReleaseMsg{X: 1, Y: y, Button: tea.MouseLeft})
			for _, b := range d.m.blocks {
				ref.collapsed[b.id] = b.collapsed
			}
			step = fmt.Sprintf("click row %d (block %d fold=%v)", rg.start, rg.idx, rg.fold)
		case 8: // collapse_all / expand_all
			all := rapid.Bool().Draw(rt, "all")
			for _, b := range d.m.blocks {
				c := all
				if !b.collapsible() || (!all && !d.m.mayExpand(&b, false)) {
					c = b.collapsed
				}
				ref.collapsed[b.id] = c
			}
			if all {
				d.m.runAction("collapse_all", "", cfg)
				step = "collapse_all"
			} else {
				d.m.runAction("expand_all", "", cfg)
				step = "expand_all"
			}
		case 9: // scroll
			switch rapid.IntRange(0, 3).Draw(rt, "scroll") {
			case 0:
				d.feed(keyPgUp())
			case 1:
				d.feed(keyPgDown())
			case 2:
				d.feed(tea.MouseWheelMsg{X: 1, Y: 1, Button: tea.MouseWheelUp})
			default:
				d.feed(tea.MouseWheelMsg{X: 1, Y: 1, Button: tea.MouseWheelDown})
			}
			step = "scroll"
		default:
			ref.w, ref.h = rapid.IntRange(20, 200).Draw(rt, "w"), rapid.IntRange(6, 50).Draw(rt, "h")
			d.feed(windowSize(ref.w, ref.h))
			step = fmt.Sprintf("resize %dx%d", ref.w, ref.h)
		}
		foldModelSync(rt, d, ref, step, adopt)
		foldModelCheck(rt, d, ref, step)
	}
}

func TestFoldModelProperty(t *testing.T) {
	t.Parallel()
	steps := 40
	if os.Getenv("BOUGH_FOLD_MODEL_LONG") != "" {
		steps = 120
	}
	rapid.Check(t, func(rt *rapid.T) { foldModelRun(rt, t, steps) })
}

// collapse_all then expand_all: every collapsible block within the
// preview cap is open, whatever it was before, and the frame shows
// each body in full (foldModelCheck).
func TestFoldModelCollapseExpandAll(t *testing.T) {
	t.Parallel()
	rapid.Check(t, func(rt *rapid.T) {
		ref := &foldModelState{w: rapid.IntRange(20, 200).Draw(rt, "w"), h: 30, collapsed: map[int]bool{}}
		d := newDrv(t, ref.w, ref.h, cfgWith(t, nil, nil, nil))
		for range rapid.IntRange(1, 8).Draw(rt, "k") {
			kind := rapid.SampledFrom([]string{"code", "result", "thinking", "system"}).Draw(rt, "kind")
			d.event(kind, foldModelText().Draw(rt, "text"))
		}
		d.event("done", "")
		cfg := d.cfgp.Load()
		d.m.runAction("collapse_all", "", cfg)
		d.m.runAction("expand_all", "", cfg)
		for _, b := range d.m.blocks {
			if b.collapsible() && b.collapsed {
				rt.Fatalf("block %d (%s) still collapsed after expand_all", b.id, b.kind)
			}
			ref.collapsed[b.id] = b.collapsed
		}
		foldModelCheck(rt, d, ref, "expand_all")
	})
}

// Known bug: collapsing an expanded code block beside another closed
// step makes a foldable run, so the block vanishes into a fresh
// "▸ 2 steps" row; the next enter (focus still on it) opens the fold
// instead of expanding the block. Toggling twice is not the identity.
// fold.go foldable/runs + model.go toggleBlock (foldAt short-circuit).
func TestFoldModelCollapseJoinsFold(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 1)
	d.m.toggleBlock(0) // expand the first step
	d.event("code", `tools.bash("echo z")`)
	d.press(keyEnter()) // collapse it again (focus on it)
	d.press(keyEnter()) // ...and expand it back
	if d.m.blocks[0].collapsed {
		t.Fatalf("enter twice on an expanded block should leave it expanded:\n%s", d.plain())
	}
}

// Known bug: fold.go setFold stores unfolded[id] = r.to (an index)
// and runs() reuses it, so a step landing after the fold opened is
// drawn under "▾ 2 steps" without being counted, and folding the
// header back then swallows it (refold+unfold is not the identity).
func TestFoldModelOpenFoldStaleExtent(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 2)
	d.m.unfold(0)
	d.event("code", `tools.bash("echo c")`)
	d.event("result", "ok")
	if p := d.plain(); !strings.Contains(p, "▾ 3 steps") {
		t.Fatalf("the open fold's header should count all three closed steps under it:\n%s", p)
	}
}

// Folding an open run back gives one row again even when a row inside
// it was closed by hand while it was open (keepRow must not split it).
func TestFoldModelRefoldAfterHandClose(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 3)
	d.m.unfold(0)
	d.m.toggleBlock(2) // open a row inside the fold
	d.m.toggleBlock(2) // ...and close it by hand
	d.m.refold(0)
	if r := d.m.foldRuns(); len(r) != 1 || r[0].from != 0 || r[0].to != len(d.m.blocks) {
		t.Fatalf("refold should give one closed run over every step, got %+v:\n%s", r, d.plain())
	}
}

// A reply dropped from inside an open fold (a superseding note) must
// not shift the fold's end onto the block after it.
func TestFoldModelOpenFoldSurvivesRemoval(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 1)
	d.event("assistant", "checking")
	steps(d, 1)
	d.m.unfold(0)
	d.feed(eventMsg{Kind: "system", Text: "asked again", Data: map[string]any{"supersedes": true}})
	rs := d.m.runs()
	if len(rs) == 0 || rs[0].to != len(d.m.blocks)-1 {
		t.Fatalf("open fold should end before the system note: runs %+v, %d blocks", rs, len(d.m.blocks))
	}
}
