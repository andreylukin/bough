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

// specs/select_component_reposition_race.fizz: select.tsx's dropdown at
// phone width, raced against the resize listener (a keyboard opening
// under its search field) and app.tsx's pane switch (which unmounts
// Settings, and with it the Select, out from under an open picker). Every
// field is the page's own client state — which overlay is open, the
// popover's last-computed fit, where focus sits — no API reports it and
// no server action changes it, so the adapter is a straight
// reimplementation of app.tsx's route/overlay state and select.tsx's
// place()/hide(), run against a real serve only so the walk has an
// actual session behind the settings dialog, the way a person would.
type selectRepositionAdapter struct {
	t    *testing.T
	s    *servetest.Server
	sid  string
	gate gate

	route, settings, picker, keyboard bool
	fit, focus                        string

	// hideNoRefocus is the deliberate bug
	// TestSelectComponentRepositionRaceCatchesWrongAdapter injects:
	// ClosePicker (select.tsx's hide(true)) is supposed to move focus back
	// onto Settings' still-mounted button. This drops that, the mistake
	// that would leave focus dangling on a field that no longer exists.
	hideNoRefocus bool
}

func newSelectRepositionAdapter(t *testing.T) *selectRepositionAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	a := &selectRepositionAdapter{t: t, s: s}
	ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
	defer cancel()
	row, err := s.CreateSession(ctx, s.Dir(t, "work"), "")
	if err != nil {
		t.Fatal(err)
	}
	a.sid = row.ID
	return a
}

// Init: the thread pane, nothing open, nothing to fit, focus on the page
// — the same starting state as the spec's Init, in the same serve every
// walk shares.
func (a *selectRepositionAdapter) Init() error {
	a.route = true // true == "thread", false == "list"
	a.settings, a.picker, a.keyboard = false, false, false
	a.fit, a.focus = "", "page"
	a.gate.reset()
	return nil
}

// Cleanup: nothing on the server needs undoing between walks — the
// session outlives them all, and this state is only ever the adapter's.
func (a *selectRepositionAdapter) Cleanup() error { return nil }

func (a *selectRepositionAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "App", Index: 0}: a}, nil
}

func (a *selectRepositionAdapter) GetState() (map[string]any, error) {
	route := "list"
	if a.route {
		route = "thread"
	}
	return map[string]any{
		"route":    route,
		"settings": a.settings,
		"picker":   a.picker,
		"keyboard": a.keyboard,
		"fit":      a.fit,
		"focus":    a.focus,
	}, nil
}

// The sliders button: Settings mounts, nothing else moves yet.
func (a *selectRepositionAdapter) OpenSettings() error {
	if !a.gate.pass(a.route && !a.settings) {
		return nil
	}
	a.settings = true
	a.focus = "settings"
	return nil
}

// Escape, outside tap, or the sliders button again.
func (a *selectRepositionAdapter) CloseSettings() error {
	if !a.gate.pass(a.settings && !a.picker) {
		return nil
	}
	a.settings = false
	a.focus = "page"
	return nil
}

// The model/effort Select inside Settings: place() runs once at open
// against the viewport as it is right now.
func (a *selectRepositionAdapter) OpenPicker() error {
	if !a.gate.pass(a.settings && !a.picker) {
		return nil
	}
	a.picker = true
	a.focus = "picker"
	if a.keyboard {
		a.fit = "short"
	} else {
		a.fit = "full"
	}
	return nil
}

// Escape or a pick: hide(true) closes and refocuses the button, which is
// still mounted (Settings survives).
func (a *selectRepositionAdapter) ClosePicker() error {
	if !a.gate.pass(a.picker) {
		return nil
	}
	a.picker = false
	a.keyboard = false
	a.fit = ""
	if !a.hideNoRefocus {
		a.focus = "settings"
	}
	return nil
}

// The keyboard opens or shuts while the picker is open (its search
// field's autofocus, or the OS IME toggling): the resize listener fires
// and place() recomputes the fit against the new visible().
func (a *selectRepositionAdapter) Resize() error {
	if !a.gate.pass(a.picker) {
		return nil
	}
	a.keyboard = !a.keyboard
	if a.keyboard {
		a.fit = "short"
	} else {
		a.fit = "full"
	}
	return nil
}

// The phone breakpoint's Back: a route change away from "thread" unmounts
// Settings and, with it, any Select inside — even mid-open, even
// mid-keyboard. Nothing refocuses a button that no longer exists.
func (a *selectRepositionAdapter) PaneSwitch() error {
	if !a.gate.pass(a.route) {
		return nil
	}
	a.route = false
	a.settings, a.picker, a.keyboard = false, false, false
	a.fit, a.focus = "", "page"
	return nil
}

// Back to the thread pane: Settings and the picker stay shut until opened
// again (OpenSettings requires route == "thread").
func (a *selectRepositionAdapter) ReturnToThread() error {
	if !a.gate.pass(!a.route) {
		return nil
	}
	a.route = true
	return nil
}

var selectRepositionActions = map[string]map[string]fmbt.ActionFunc{"App": {
	"OpenSettings":   action((*selectRepositionAdapter).OpenSettings),
	"CloseSettings":  action((*selectRepositionAdapter).CloseSettings),
	"OpenPicker":     action((*selectRepositionAdapter).OpenPicker),
	"ClosePicker":    action((*selectRepositionAdapter).ClosePicker),
	"Resize":         action((*selectRepositionAdapter).Resize),
	"PaneSwitch":     action((*selectRepositionAdapter).PaneSwitch),
	"ReturnToThread": action((*selectRepositionAdapter).ReturnToThread),
}}

// No step touches the server, so a walk is cheap. Only one or two of the
// seven actions are ever enabled at a time (OpenPicker needs Settings
// open first, ClosePicker needs the picker open), so a uniform random
// pick survives to the next step on maybe 2/7 odds and the three-deep
// chain OpenSettings->OpenPicker->ClosePicker that the wrong-adapter test
// below needs is rare per walk; max-seq-runs is high enough that it is
// not, in practice, missed.
func selectRepositionOptions() map[string]any {
	return map[string]any{"max-seq-runs": 4000, "max-actions": 8, "max-parallel-runs": 0}
}

// selectComponentRepositionRaceHistory: opening the picker and racing it
// against a resize or a pane switch never writes a history entry (nothing
// here is a server action), so the only step a real transcript could ever
// hold is the walk's Init.
func selectComponentRepositionRaceHistory([]history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{
		"App#0.route": "thread", "App#0.settings": false, "App#0.picker": false,
		"App#0.keyboard": false, "App#0.fit": "", "App#0.focus": "page",
	}}}
}

func init() {
	historyProjections["select_component_reposition_race"] = selectComponentRepositionRaceHistory
}

func TestSelectComponentRepositionRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSelectRepositionAdapter(t)
	if err := runMBT(t, "select_component_reposition_race", a, selectRepositionActions, selectRepositionOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a broken adapter fails it. Closing
// the picker here does not refocus Settings' button, so OpenPicker then
// ClosePicker reports focus stuck on "picker" where the model has it back
// on "settings".
func TestSelectComponentRepositionRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSelectRepositionAdapter(t)
	a.hideNoRefocus = true
	if err := runMBT(t, "select_component_reposition_race", a, selectRepositionActions, selectRepositionOptions()); err == nil {
		t.Fatal("a run whose ClosePicker leaves focus on the picker passed; the runner is not checking state")
	}
}
