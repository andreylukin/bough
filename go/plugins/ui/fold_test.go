package ui

// Step folding (fold.go): a finished run of closed code/result rows
// draws as one summary line, and opening it puts the rows back.

import (
	"strings"
	"testing"
)

// steps adds n code/result pairs, each running one command.
func steps(d *drv, n int) {
	for i := range n {
		d.event("code", `tools.bash("echo `+string(rune('a'+i))+`")`)
		d.event("result", "ok")
	}
}

func TestStepsFoldIntoOneRow(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 3)
	p := d.plain()
	if !strings.Contains(p, "▸ 3 steps · ran 3 commands") {
		t.Fatalf("three steps should fold into one summary row:\n%s", p)
	}
	if strings.Contains(p, "Ran: echo a") || strings.Contains(p, "▸ result") {
		t.Errorf("the folded rows must not also be drawn:\n%s", p)
	}
}

// The summary counts what the steps did, not just how many there were.
func TestFoldSummaryCountsByVerb(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("code", `tools.view("a.go"); tools.view("b.go")`)
	d.event("result", "ok")
	d.event("code", `tools.patch("a.go", x, y)`)
	d.event("result", "ok")
	d.event("code", `tools.bash("go test ./...")`)
	d.event("result", "ok")
	if p := d.plain(); !strings.Contains(p, "▸ 3 steps · read 2 files, edited 1 file, ran 1 command") {
		t.Fatalf("summary should tally every call in the run:\n%s", p)
	}
}

// One step is its own story: folding it would hide work and save
// nothing.
func TestSingleStepDoesNotFold(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 1)
	if p := d.plain(); !strings.Contains(p, "Ran: echo a") || strings.Contains(p, "steps") {
		t.Fatalf("a single step stays as its own rows:\n%s", p)
	}
}

func TestFoldOpensToItsRows(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 3)
	d.press(keyTab()) // the fold row is the only focusable
	d.press(keyEnter())
	p := d.plain()
	if strings.Contains(p, "3 steps") {
		t.Fatalf("an opened fold is replaced by its rows:\n%s", p)
	}
	for _, want := range []string{"Ran: echo a", "Ran: echo b", "Ran: echo c"} {
		if !strings.Contains(p, want) {
			t.Errorf("unfolded run missing %q:\n%s", want, p)
		}
	}
	// The rows themselves are still closed — unfolding is not expanding.
	for i := range d.m.blocks {
		if !d.m.blocks[i].collapsed {
			t.Errorf("block %d should still be collapsed after unfolding", i)
		}
	}
	// Opening is one-way: enter again expands the lead like any other
	// block, rather than folding the run back up.
	d.press(keyEnter())
	if d.m.blocks[0].collapsed {
		t.Errorf("enter on the lead should expand it, not re-fold:\n%s", d.plain())
	}
}

// The block cursor walks what is on screen: a folded run offers one
// stop, not one per hidden row.
func TestFoldedRunIsOneFocusStop(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 3)
	if f := d.m.focusables(); len(f) != 1 || f[0] != 0 {
		t.Fatalf("a folded run is one focus stop at its lead, got %v", f)
	}
}

// An expanded block is not machinery any more; it ends the run.
func TestExpandedBlockBreaksTheRun(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	steps(d, 4)
	d.m.blocks[3].collapsed = false // the second step's result, opened
	d.m.refresh()
	p := d.plain()
	if !strings.Contains(p, "▸ 2 steps") {
		t.Fatalf("the run should break around the opened block:\n%s", p)
	}
	if strings.Contains(p, "4 steps") {
		t.Errorf("the opened block must not be folded away:\n%s", p)
	}
}

// While the turn runs, the steps arriving are the only sign it is
// working: the tail stays as rows and folds when the turn ends.
func TestRunningTurnTailIsNotFolded(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.running = true
	for i := range 3 {
		m.addEvent(Event{Kind: "code", Text: `tools.bash("echo ` + string(rune('a'+i)) + `")`})
		m.addEvent(Event{Kind: "result", Text: "ok"})
	}
	if runs := m.foldRuns(); len(runs) != 0 {
		t.Fatalf("a running turn's steps stay visible, got %v", runs)
	}
	m.running = false
	m.refresh()
	if runs := m.foldRuns(); len(runs) != 1 {
		t.Fatalf("the finished run folds, got %v", runs)
	}
}

// A real turn interleaves thinking and a line of narration with every
// step (the shape from a recorded session). Those fold too; the reply
// that ends the turn does not.
func TestNarratedStepsFold(t *testing.T) {
	t.Parallel()
	d := defaultDrv(t)
	d.event("thinking", "which file\nholds it")
	d.event("assistant", "Let me look at the loader.")
	d.event("code", `tools.view("loader.go")`)
	d.event("result", "ok")
	d.event("assistant", "Now the test.")
	d.event("code", `tools.bash("go test ./...")`)
	d.event("result", "ok")
	d.event("thinking", "done")
	d.event("assistant", "The loader merges by id.")
	d.event("done", "")
	p := d.plain()
	if !strings.Contains(p, "▸ 2 steps · read 1 file, ran 1 command") {
		t.Fatalf("narrated steps should fold into one row:\n%s", p)
	}
	if strings.Contains(p, "Let me look") || strings.Contains(p, "Now the test") {
		t.Errorf("one-line narration folds with its step:\n%s", p)
	}
	if !strings.Contains(p, "The loader merges by id.") {
		t.Errorf("the reply that ends the turn stays visible:\n%s", p)
	}
	if strings.Count(p, "▸") != 1 { // the trailing thinking row folds too
		t.Errorf("want exactly the fold row:\n%s", p)
	}
}
