//go:build !windows

package mbt

import (
	"fmt"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/sidebar_peek_tree_scroll.fizz. The peek card, focus, fold and
// scroll are the page's own state, tracked here the way the page holds
// them; what the server owns is whether the row is archived and whether
// it pins under Needs you (a failed turn: hasFailure), which the adapter
// reads off the row.
type sidebarPeekAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id       string
	peek     string
	folded   bool
	focus    string
	selected bool
	scrolled bool
	turn     int
	ids      []string

	// clearAsFail is the deliberate bug: PollClear ends its turn in an
	// error, so the row stays under Needs you.
	clearAsFail bool
}

func newSidebarPeekAdapter(t *testing.T) *sidebarPeekAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &sidebarPeekAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *sidebarPeekAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id, a.peek, a.folded, a.focus, a.selected, a.scrolled = row.ID, "none", false, "none", false, false
	a.gate.reset()
	a.ids = append(a.ids, row.ID)
	return nil
}

func (a *sidebarPeekAdapter) Cleanup() error { return nil }

func (a *sidebarPeekAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Tree", Index: 0}: a}, nil
}

func (a *sidebarPeekAdapter) row() (serve.Row, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	row, _, err := a.s.GetSession(ctx, a.id)
	return row, err
}

func (a *sidebarPeekAdapter) GetState() (map[string]any, error) {
	row, err := a.row()
	if err != nil {
		return nil, err
	}
	grp := "recent"
	if row.Status == serve.StatusError {
		grp = "needs"
	}
	return map[string]any{
		"peek": a.peek, "grp": grp, "folded": a.folded, "focus": a.focus,
		"archived": row.Archived, "selected": a.selected, "scrolled": a.scrolled,
	}, nil
}

func (a *sidebarPeekAdapter) state() (archived, needs bool, err error) {
	row, err := a.row()
	return row.Archived, row.Status == serve.StatusError, err
}

func (a *sidebarPeekAdapter) Hover() error {
	arch, _, err := a.state()
	if err != nil || !a.gate.pass(!arch && a.peek == "none") {
		return err
	}
	a.peek = "pending"
	return nil
}

func (a *sidebarPeekAdapter) Tick() error {
	if a.gate.pass(a.peek == "pending") {
		a.peek = "shown"
	}
	return nil
}

func (a *sidebarPeekAdapter) Unhover() error {
	if a.gate.pass(a.peek != "none") {
		a.peek = "none"
	}
	return nil
}

func (a *sidebarPeekAdapter) Scroll() error {
	a.scrolled, a.peek = true, "none"
	return nil
}

func (a *sidebarPeekAdapter) Select() error {
	arch, _, err := a.state()
	if err != nil || !a.gate.pass(!arch) {
		return err
	}
	a.selected, a.focus, a.peek = true, "row", "none"
	return nil
}

// runTurn drives one whole turn to its end: error when fail, else done.
func (a *sidebarPeekAdapter) runTurn(fail bool) error {
	a.turn++
	name := fmt.Sprintf("t%04d", a.turn)
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "finished " + name})
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.s.Prompt(ctx, a.id, "turn "+name); err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	want := serve.StatusDone
	if fail {
		want = serve.StatusError
		control.ReleaseWith(a.t, a.dir, name, control.Turn{Mode: "error", Error: "model says no"})
	} else {
		control.Release(a.t, a.dir, name)
	}
	_, err := waitRow(a.s, a.id, string(want), func(r serve.Row) bool { return r.Status == want })
	return err
}

func (a *sidebarPeekAdapter) PollNeedsYou() error {
	arch, needs, err := a.state()
	if err != nil || !a.gate.pass(!arch && !needs) {
		return err
	}
	return a.runTurn(true)
}

func (a *sidebarPeekAdapter) PollClear() error {
	arch, needs, err := a.state()
	if err != nil || !a.gate.pass(!arch && needs) {
		return err
	}
	return a.runTurn(a.clearAsFail)
}

func (a *sidebarPeekAdapter) ToggleFold() error {
	a.folded = !a.folded
	return nil
}

func (a *sidebarPeekAdapter) FocusOther() error {
	a.focus = "other"
	return nil
}

func (a *sidebarPeekAdapter) Archive() error {
	arch, _, err := a.state()
	if err != nil || !a.gate.pass(!arch) {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if _, err := a.s.Archive(ctx, a.id); err != nil {
		return err
	}
	a.peek, a.selected = "none", false
	if a.focus == "row" {
		a.focus = "none"
	}
	return nil
}

var sidebarPeekActions = map[string]map[string]fmbt.ActionFunc{"Tree": {
	"Hover":        action((*sidebarPeekAdapter).Hover),
	"Tick":         action((*sidebarPeekAdapter).Tick),
	"Unhover":      action((*sidebarPeekAdapter).Unhover),
	"Scroll":       action((*sidebarPeekAdapter).Scroll),
	"Select":       action((*sidebarPeekAdapter).Select),
	"PollNeedsYou": action((*sidebarPeekAdapter).PollNeedsYou),
	"PollClear":    action((*sidebarPeekAdapter).PollClear),
	"ToggleFold":   action((*sidebarPeekAdapter).ToggleFold),
	"FocusOther":   action((*sidebarPeekAdapter).FocusOther),
	"Archive":      action((*sidebarPeekAdapter).Archive),
}}

func sidebarPeekOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

// sidebarPeekHistory: a transcript holds only the server-side part of
// the model (needs-you via a failed turn, and its clearing), so the
// projection checks grp alone; page-only actions leave no entries.
func sidebarPeekHistory(entries []history.Entry) []tracecheck.Step {
	grp := func(s string) map[string]any { return map[string]any{"Tree#0.grp": s} }
	steps := []tracecheck.Step{{Action: "Init", State: grp("recent")}}
	failed, needs := false, false
	for _, e := range entries {
		switch e.Kind {
		case "input":
			failed = false
		case "error":
			failed = true
		case "done":
			if failed && !needs {
				needs = true
				steps = append(steps, tracecheck.Step{Action: "Tree#0.PollNeedsYou", State: grp("needs")})
			} else if !failed && needs {
				needs = false
				steps = append(steps, tracecheck.Step{Action: "Tree#0.PollClear", State: grp("recent")})
			}
		}
	}
	return steps
}

func init() { historyProjections["sidebar_peek_tree_scroll"] = sidebarPeekHistory }

func TestSidebarPeekTreeScroll(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSidebarPeekAdapter(t)
	if err := runMBT(t, "sidebar_peek_tree_scroll", a, sidebarPeekActions, sidebarPeekOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "sidebar_peek_tree_scroll"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sidebarPeekHistory)
	}
}

func TestSidebarPeekTreeScrollCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSidebarPeekAdapter(t)
	a.clearAsFail = true
	if err := runMBT(t, "sidebar_peek_tree_scroll", a, sidebarPeekActions, sidebarPeekOptions()); err == nil {
		t.Fatal("a run whose PollClear keeps the row failing passed; the runner is not checking state")
	}
}
