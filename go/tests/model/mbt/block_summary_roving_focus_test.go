//go:build !windows

package mbt

import (
	"slices"
	"strconv"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/block_summary_roving_focus.fizz: which block summary is the
// transcript's one tab stop. Nothing on the server sees it (the stop,
// focus and the open <details> are the page's own), so the adapter
// keeps them by the page's rules as the spec states them, against a
// serve that only supplies the session the page would show. The
// browser flow is where the page itself is checked.
type rovingAdapter struct {
	t    *testing.T
	s    *servetest.Server
	gate gate

	top     int
	nested  string
	cur, at string
	focused bool

	// stepTwo is the deliberate bug TestBlockSummaryRovingFocusCatchesWrongAdapter
	// injects: Down skips a summary.
	stepTwo bool
}

func newRovingAdapter(t *testing.T) *rovingAdapter {
	return &rovingAdapter{t: t, s: servetest.Start(t, servetest.Options{Config: controlConfig})}
}

func (a *rovingAdapter) Init() error {
	a.gate.reset()
	a.top, a.nested, a.cur, a.at, a.focused = 1, "closed", "b0", "", false
	ctx, cancel := actionCtx()
	defer cancel()
	_, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	return err
}

func (a *rovingAdapter) Cleanup() error { return nil }

func (a *rovingAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Blocks", Index: 0}: a}, nil
}

func (a *rovingAdapter) GetState() (map[string]any, error) {
	return map[string]any{"top": a.top, "nested": a.nested, "cur": a.cur, "focused": a.focused, "at": a.at}, nil
}

func (a *rovingAdapter) vis() []string {
	v := []string{"b0"}
	if a.nested == "open" {
		v = append(v, "n")
	}
	for i := 1; i < a.top; i++ {
		v = append(v, "b"+strconv.Itoa(i))
	}
	return v
}

func (a *rovingAdapter) FocusIn() error {
	if a.gate.pass(!a.focused) {
		a.focused, a.at = true, a.cur
	}
	return nil
}

func (a *rovingAdapter) Blur() error {
	if a.gate.pass(a.focused) {
		a.focused, a.at = false, ""
	}
	return nil
}

func (a *rovingAdapter) move(d int) {
	v := a.vis()
	if a.stepTwo {
		d *= 2
	}
	if i := slices.Index(v, a.at) + d; i >= 0 && i < len(v) {
		a.cur, a.at = v[i], v[i]
	}
}

func (a *rovingAdapter) Down() error {
	if a.gate.pass(a.focused) {
		a.move(1)
	}
	return nil
}

func (a *rovingAdapter) Up() error {
	if a.gate.pass(a.focused) {
		a.move(-1)
	}
	return nil
}

func (a *rovingAdapter) Stream() error {
	if a.gate.pass(a.top < 3) {
		a.top++
	}
	return nil
}

func (a *rovingAdapter) OpenNested() error {
	if a.gate.pass(a.nested == "closed") {
		a.nested = "open"
	}
	return nil
}

func (a *rovingAdapter) CloseNested() error {
	if !a.gate.pass(a.nested == "open") {
		return nil
	}
	a.nested = "closed"
	if a.cur == "n" {
		a.cur = "b0"
	}
	if a.at == "n" {
		a.at = "b0"
	}
	return nil
}

var rovingActions = map[string]map[string]fmbt.ActionFunc{"Blocks": {
	"FocusIn":     action((*rovingAdapter).FocusIn),
	"Blur":        action((*rovingAdapter).Blur),
	"Down":        action((*rovingAdapter).Down),
	"Up":          action((*rovingAdapter).Up),
	"Stream":      action((*rovingAdapter).Stream),
	"OpenNested":  action((*rovingAdapter).OpenNested),
	"CloseNested": action((*rovingAdapter).CloseNested),
}}

func rovingOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// Nothing here reaches a transcript: history holds no block-focus state.
func rovingHistory(entries []history.Entry) []tracecheck.Step {
	return []tracecheck.Step{{Action: "Init", State: map[string]any{"Blocks#0.top": 1}}}
}

func init() { historyProjections["block_summary_roving_focus"] = rovingHistory }

func TestBlockSummaryRovingFocus(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRovingAdapter(t)
	if err := runMBT(t, "block_summary_roving_focus", a, rovingActions, rovingOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

func TestBlockSummaryRovingFocusCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newRovingAdapter(t)
	a.stepTwo = true
	if err := runMBT(t, "block_summary_roving_focus", a, rovingActions, rovingOptions()); err == nil {
		t.Fatal("a run whose Down skips a summary passed; the runner is not checking state")
	}
}
