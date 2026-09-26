//go:build !windows

package mbt

import (
	"context"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/call_row_hover_popover_race.fizz: useThinPop's show/hide timers
// for two adjacent transcript call rows (app.tsx, ~2041-2119). Every
// field is the page's own client state (which row is hovered, which
// row's show/hide timer is pending, whose popover is on screen) — no
// API reports it and no server action changes it, so the adapter is a
// straight reimplementation of enter/leave/ShowFire/HideFire, run
// against a real serve only so the walk has an actual session and call
// row to be hovering over, the way a person would be.
type popoverAdapter struct {
	t    *testing.T
	s    *servetest.Server
	sid  string
	gate gate

	hovered, pendingShow, pendingHide, visible any // "none" | 0 | 1

	// leaveLeavesShowPending is the deliberate bug
	// TestCallRowHoverPopoverRaceCatchesWrongAdapter injects: mouseleave
	// (app.tsx hideSoon) is supposed to clearTimeout the row's own show
	// timer along with everything else on its one timer ref, so leaving
	// a row always cancels its pending show. This drops that cancel, the
	// mistake that would let a show timer for a row nobody is hovering
	// anymore go on to fire.
	leaveLeavesShowPending bool
}

func newPopoverAdapter(t *testing.T) *popoverAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &popoverAdapter{t: t, s: s}
	ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.sid = row.ID
	return a
}

// Init is the popover closed, nothing hovered, nothing pending: the same
// starting state as the spec's Init, in the same serve every walk shares.
func (a *popoverAdapter) Init() error {
	a.hovered, a.pendingShow, a.pendingHide, a.visible = "none", "none", "none", "none"
	a.gate.reset()
	return nil
}

// Cleanup: nothing on the server needs undoing between walks — the
// session outlives them all, and hover state is only ever the adapter's.
func (a *popoverAdapter) Cleanup() error { return nil }

func (a *popoverAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Popover", Index: 0}: a}, nil
}

func (a *popoverAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"hovered":     a.hovered,
		"pendingShow": a.pendingShow,
		"pendingHide": a.pendingHide,
		"visible":     a.visible,
	}, nil
}

// enter mirrors the spec's atomic func enter(row): the row's mouseenter
// cancels a hide timer already pending for it, and arms a show timer
// unless its popover is already the one on screen.
func (a *popoverAdapter) enter(row int) {
	a.hovered = row
	if a.pendingHide == row {
		a.pendingHide = "none"
	}
	if a.visible != row {
		a.pendingShow = row
	}
}

// leave mirrors the spec's atomic func leave(row): mouseleave drops a
// still-pending show timer for the row (it never gets to fire for real)
// and, if the row's popover is up, arms its hide timer.
func (a *popoverAdapter) leave(row int) {
	a.hovered = "none"
	if a.pendingShow == row && !a.leaveLeavesShowPending {
		a.pendingShow = "none"
	}
	if a.visible == row {
		a.pendingHide = row
	}
}

func (a *popoverAdapter) Enter0() error {
	if !a.gate.pass(a.hovered == "none") {
		return nil
	}
	a.enter(0)
	return nil
}

func (a *popoverAdapter) Leave0() error {
	if !a.gate.pass(a.hovered == 0) {
		return nil
	}
	a.leave(0)
	return nil
}

func (a *popoverAdapter) Enter1() error {
	if !a.gate.pass(a.hovered == "none") {
		return nil
	}
	a.enter(1)
	return nil
}

func (a *popoverAdapter) Leave1() error {
	if !a.gate.pass(a.hovered == 1) {
		return nil
	}
	a.leave(1)
	return nil
}

// ShowFire: the pointer is still on the row the show timer was armed
// for, so its popover is the one that appears.
func (a *popoverAdapter) ShowFire() error {
	if !a.gate.pass(a.pendingShow != "none" && a.hovered == a.pendingShow) {
		return nil
	}
	a.visible = a.pendingShow
	a.pendingShow = "none"
	return nil
}

// ShowFireStale: the pointer moved off the row (to the other row, or off
// both) before the timer fired. Enter and Leave keep pendingShow in
// lockstep with hovered (Leave always cancels its own row's pending
// show, the way hideSoon's clearTimeout does), so this precondition is
// unreachable from the actions above — checked by hand: it never
// appears in the state graph (specs/call_row_hover_popover_race.fizz's
// generated graph has zero ShowFireStale links). It stays in the spec
// as the documented case a person never sees, matching the product.
func (a *popoverAdapter) ShowFireStale() error {
	if !a.gate.pass(a.pendingShow != "none" && a.hovered != a.pendingShow) {
		return nil
	}
	a.pendingShow = "none"
	return nil
}

func (a *popoverAdapter) HideFire() error {
	if !a.gate.pass(a.pendingHide != "none") {
		return nil
	}
	if a.visible == a.pendingHide {
		a.visible = "none"
	}
	a.pendingHide = "none"
	return nil
}

var popoverActions = map[string]map[string]fmbt.ActionFunc{"Popover": {
	"Enter0":        action((*popoverAdapter).Enter0),
	"Leave0":        action((*popoverAdapter).Leave0),
	"Enter1":        action((*popoverAdapter).Enter1),
	"Leave1":        action((*popoverAdapter).Leave1),
	"ShowFire":      action((*popoverAdapter).ShowFire),
	"ShowFireStale": action((*popoverAdapter).ShowFireStale),
	"HideFire":      action((*popoverAdapter).HideFire),
}}

// No step touches the server, so a walk is cheap: it is the timer
// interleavings that matter, not the count of them.
func popoverOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 8, "max-parallel-runs": 0}
}

// callRowHoverPopoverRaceHistory: hovering and its timers never write a
// history entry (nothing here is a server action), so the only step a
// real transcript could ever hold is the walk's Init.
func callRowHoverPopoverRaceHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{
		"Popover#0.hovered": "none", "Popover#0.pendingShow": "none",
		"Popover#0.pendingHide": "none", "Popover#0.visible": "none",
	}}}
}

func init() {
	historyProjections["call_row_hover_popover_race"] = callRowHoverPopoverRaceHistory
}

func TestCallRowHoverPopoverRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPopoverAdapter(t)
	if err := runMBT(t, "call_row_hover_popover_race", a, popoverActions, popoverOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a broken adapter fails it. Leaving
// a row here does not cancel its own pending show timer, so Enter0 then
// Leave0 reports pendingShow still 0 where the model has it cleared.
func TestCallRowHoverPopoverRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newPopoverAdapter(t)
	a.leaveLeavesShowPending = true
	if err := runMBT(t, "call_row_hover_popover_race", a, popoverActions, popoverOptions()); err == nil {
		t.Fatal("a run whose Leave leaves the show timer pending passed; the runner is not checking state")
	}
}
