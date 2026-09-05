package ui

// Step folding: a finished run of collapsed code/result rows renders as
// one summary line.
//
// bough is code-mode, so a turn is a chain of code/result pairs — nine
// steps is nine "▸ Ran: …" rows plus nine "▸ result" rows, and the
// answer that matters scrolls off the top. Claude Code collapses the
// same machinery into one line ("Read 1 file, ran 2 shell commands"),
// which is the shape borrowed here: the run becomes
//
//	▸ 4 steps · read 2 files, ran 3 commands, edited 1 file
//
// Nothing about the blocks changes — a fold is a render-time view, so
// unfolding puts the same rows back, each collapsing as it always did,
// under a header row ("▾ 4 steps · …") that folds them up again.

import (
	"fmt"
	"strings"
)

// foldMin is the fewest steps (code blocks) worth folding. One step
// is its own story: folding it hides work without saving a screen.
const foldMin = 2

// foldRun is a run of block indices [from, to) drawn as one row when
// closed, or as a header row above its blocks when open. lead is the
// run's first block: it owns the row's identity, so focus, hit-testing
// and the unfolded flag all key off its id.
type foldRun struct {
	from, to, lead int
	open           bool
}

// foldable reports whether a block may be swallowed into a fold: the
// machinery of a step, currently closed, saying nothing on its own.
//
// A real turn is not bare code/result pairs. Each step arrives as a
// thinking block, a line of narration ("Let me check the loader."),
// the code, and its result — so a fold that only ate code and result
// never found three rows in a row, and never drew. Thinking is closed
// machinery by definition. Narration folds when it is one line that
// leads into a step: the reply that ends the turn is not followed by
// code, so it always stays out.
//
// Folding must not read the focus — focusables is derived from the
// folds, and a fold that dissolved when its own lead took focus could
// never be unfolded by keyboard.
func (m *model) foldable(i int) bool {
	b := &m.blocks[i]
	switch b.kind {
	case "code", "result", "thinking":
		return b.collapsed
	case "assistant":
		return !b.live && !strings.Contains(b.text, "\n") && m.leadsIntoStep(i)
	}
	return false
}

// leadsIntoStep reports whether the next block after i that is not
// thinking is a code block: block i is the preamble to a step.
func (m *model) leadsIntoStep(i int) bool {
	for j := i + 1; j < len(m.blocks); j++ {
		if m.blocks[j].kind == "thinking" {
			continue
		}
		return m.blocks[j].kind == "code"
	}
	return false
}

// foldRuns finds the closed folds in the transcript, in order.
func (m *model) foldRuns() []foldRun {
	var out []foldRun
	for _, r := range m.runs() {
		if !r.open {
			out = append(out, r)
		}
	}
	return out
}

// runs finds every foldable run, open or closed, in order. A run ends
// at anything that is not foldable, and the tail of a running turn is
// left alone: while the agent works, the steps arriving are the only
// sign it is working. An open run keeps the extent it had when it was
// opened: expanding a row inside it must not dissolve the header that
// folds it back.
func (m *model) runs() []foldRun {
	last := len(m.blocks)
	if m.running {
		last = 0
		for i := len(m.blocks) - 1; i >= 0; i-- {
			if !m.foldable(i) {
				last = i + 1
				break
			}
		}
	}
	var out []foldRun
	for i := 0; i < last; {
		if to, open := m.unfolded[m.blocks[i].id]; open {
			out = append(out, foldRun{from: i, to: to, lead: i, open: true})
			i = to
			continue
		}
		if !m.foldable(i) {
			i++
			continue
		}
		j := i
		for j < last && m.foldable(j) {
			j++
		}
		if m.stepsIn(i, j) >= foldMin {
			out = append(out, foldRun{from: i, to: j, lead: i})
		}
		i = j
	}
	return out
}

// stepsIn counts the code blocks in [from, to).
func (m *model) stepsIn(from, to int) int {
	n := 0
	for i := from; i < to; i++ {
		if m.blocks[i].kind == "code" {
			n++
		}
	}
	return n
}

// foldAt returns the run (open or closed) starting at block i, if one
// does.
func (m *model) foldAt(i int) (foldRun, bool) {
	for _, r := range m.runs() {
		if r.from == i {
			return r, true
		}
	}
	return foldRun{}, false
}

// renderFold draws a run's row — the closed fold, or the header above
// an open one: the step count and what the steps did, summed over
// every code block in the run.
func (m *model) renderFold(r foldRun, th theme) string {
	n := map[string]int{}
	steps := 0
	for i := r.from; i < r.to; i++ {
		if m.blocks[i].kind != "code" {
			continue
		}
		steps++
		for call, c := range countCalls(m.blocks[i].text) {
			n[call] += c
		}
	}
	if steps == 0 { // results with no code above them (a resumed transcript)
		steps = r.to - r.from
	}
	unit := "steps"
	if steps == 1 {
		unit = "step"
	}
	glyph := "▸"
	if r.open {
		glyph = "▾"
	}
	head := fmt.Sprintf("%s %d %s", glyph, steps, unit)
	if s := summarize(n); s != "" {
		head += " · " + s
	}
	if rs := []rune(head); len(rs) > m.width-1 && m.width > 2 {
		head = string(rs[:m.width-2]) + "…"
	}
	st := th["dim"]
	if m.blocks[r.lead].id == m.focusID && (r.open == m.focusFold) {
		st = th["focus"]
	}
	return st.Render(head)
}

// unfold opens the fold led by block i: its rows come back, each
// still closed, under a header row that holds the focus so enter
// again folds them up. The lead block below is its own stop.
func (m *model) unfold(i int) {
	if m.unfolded == nil {
		m.unfolded = map[int]int{}
	}
	m.setFold(i, true)
}

// refold closes the open fold led by block i, leaving focus on the
// closed row. Rows opened while the fold was open close with it:
// an expanded block would otherwise break the run, and "fold it
// back" means back to one line.
func (m *model) refold(i int) {
	if r, ok := m.foldAt(i); ok {
		for j := r.from; j < r.to; j++ {
			if m.blocks[j].collapsible() {
				m.blocks[j].collapsed = true
			}
		}
	}
	m.setFold(i, false)
}

func (m *model) setFold(i int, open bool) {
	id := m.blocks[i].id
	if r, ok := m.foldAt(i); ok && open {
		m.unfolded[id] = r.to
	} else {
		delete(m.unfolded, id)
	}
	m.focusID = id
	m.focusFold = open
	m.refresh()
	for _, r := range m.ranges {
		if r.idx == i {
			m.vp.EnsureVisible(r.start, 0, 0)
			break
		}
	}
}
