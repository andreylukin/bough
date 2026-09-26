//go:build !windows

package mbt

import (
	"encoding/json"
	"fmt"
	"reflect"
	"strings"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/runtime_strip_pane_narrow_toggle.fizz: the pane's own
// ResizeObserver (app.tsx ~4428-4432, threadRef.clientWidth<760) and
// RuntimeStrip's own matchMedia+collision ResizeObserver (app.tsx
// ~2799-2823) each react to the same underlying width (a side panel
// opening/closing, or the window narrowing) but catch up independently.
// Every field here is the page's own client state — nothing an API
// reports — so, like `viewing` in the worked example, the adapter is
// the state: there is no server call to make, and no serve to start.
type runtimeStripPaneNarrowToggleAdapter struct {
	panelOpen, pageNarrow, paneNarrow, stripNarrow bool
	gate                                           gate

	// stripIgnoresPanel is the deliberate wiring bug
	// TestRuntimeStripPaneNarrowToggleCatchesWrongAdapter injects: a
	// SyncStrip that catches up on the page-wide signal alone, the kind
	// of mistake a strip wired straight to matchMedia and not to the
	// pane's own width would make.
	stripIgnoresPanel bool
}

func (a *runtimeStripPaneNarrowToggleAdapter) Init() error {
	a.panelOpen, a.pageNarrow, a.paneNarrow, a.stripNarrow = false, false, false, false
	a.gate.reset()
	return nil
}

func (a *runtimeStripPaneNarrowToggleAdapter) Cleanup() error { return nil }

func (a *runtimeStripPaneNarrowToggleAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Layout", Index: 0}: a}, nil
}

func (a *runtimeStripPaneNarrowToggleAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"panelOpen":   a.panelOpen,
		"pageNarrow":  a.pageNarrow,
		"paneNarrow":  a.paneNarrow,
		"stripNarrow": a.stripNarrow,
	}, nil
}

func (a *runtimeStripPaneNarrowToggleAdapter) OpenPanel() error {
	if a.gate.pass(!a.panelOpen) {
		a.panelOpen = true
	}
	return nil
}

func (a *runtimeStripPaneNarrowToggleAdapter) ClosePanel() error {
	if a.gate.pass(a.panelOpen) {
		a.panelOpen = false
	}
	return nil
}

func (a *runtimeStripPaneNarrowToggleAdapter) ResizePageNarrow() error {
	if a.gate.pass(!a.pageNarrow) {
		a.pageNarrow = true
	}
	return nil
}

func (a *runtimeStripPaneNarrowToggleAdapter) ResizePageWide() error {
	if a.gate.pass(a.pageNarrow) {
		a.pageNarrow = false
	}
	return nil
}

// SyncPane is the pane's own ResizeObserver on the thread container.
func (a *runtimeStripPaneNarrowToggleAdapter) SyncPane() error {
	want := a.panelOpen || a.pageNarrow
	if a.gate.pass(a.paneNarrow != want) {
		a.paneNarrow = want
	}
	return nil
}

// SyncStrip is RuntimeStrip's own matchMedia+collision ResizeObserver,
// wired independently of SyncPane over the same underlying width.
func (a *runtimeStripPaneNarrowToggleAdapter) SyncStrip() error {
	want := a.panelOpen || a.pageNarrow
	if !a.gate.pass(a.stripNarrow != want) {
		return nil
	}
	if a.stripIgnoresPanel {
		a.stripNarrow = a.pageNarrow
		return nil
	}
	a.stripNarrow = want
	return nil
}

var runtimeStripPaneNarrowToggleActions = map[string]map[string]fmbt.ActionFunc{"Layout": {
	"OpenPanel":        action((*runtimeStripPaneNarrowToggleAdapter).OpenPanel),
	"ClosePanel":       action((*runtimeStripPaneNarrowToggleAdapter).ClosePanel),
	"ResizePageNarrow": action((*runtimeStripPaneNarrowToggleAdapter).ResizePageNarrow),
	"ResizePageWide":   action((*runtimeStripPaneNarrowToggleAdapter).ResizePageWide),
	"SyncPane":         action((*runtimeStripPaneNarrowToggleAdapter).SyncPane),
	"SyncStrip":        action((*runtimeStripPaneNarrowToggleAdapter).SyncStrip),
}}

// Nothing here costs a real turn, so a random walk is cheap; the example's
// defaults are more than enough headroom.
func runtimeStripPaneNarrowToggleOptions() map[string]any {
	return map[string]any{"max-seq-runs": 200, "max-actions": 10, "max-parallel-runs": 0}
}

func TestRuntimeStripPaneNarrowToggle(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := &runtimeStripPaneNarrowToggleAdapter{}
	if err := runMBT(t, "runtime_strip_pane_narrow_toggle", a, runtimeStripPaneNarrowToggleActions, runtimeStripPaneNarrowToggleOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The runner's random walks rarely reach every corner of a six-action,
// four-flag graph in one skip-gated pass, and TestRuntimeStripPaneNarrowToggle
// itself only runs under MODEL_COVER=transitions (runMBT skips otherwise).
// So the same adapter also walks every path in
// testdata/runtime_strip_pane_narrow_toggle/paths.json deterministically,
// which always runs and takes every transition under MODEL_COVER=transitions.
func TestRuntimeStripPaneNarrowTogglePaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := &runtimeStripPaneNarrowToggleAdapter{}
	if err := walkRuntimeStripPaneNarrowTogglePaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
}

func walkRuntimeStripPaneNarrowTogglePaths(t *testing.T, a *runtimeStripPaneNarrowToggleAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("runtime_strip_pane_narrow_toggle", cover)
	if err != nil {
		return err
	}
	var file struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		return err
	}
	for pi, p := range file.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("path %d init: %w", pi, err)
		}
		for si, step := range p.Trace {
			if si > 0 {
				name := strings.TrimPrefix(step.Action, "Layout#0.")
				f, ok := runtimeStripPaneNarrowToggleActions["Layout"][name]
				if !ok {
					return fmt.Errorf("path %d step %d: no adapter action for %q", pi, si, step.Action)
				}
				if _, err := f(a, nil); err != nil {
					return fmt.Errorf("path %d step %d (%s): %w", pi, si, name, err)
				}
				if a.gate.off {
					return fmt.Errorf("path %d step %d: %s is enabled in the model but not in the adapter's view", pi, si, name)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("path %d step %d: %w", pi, si, err)
			}
			for k, v := range step.State {
				field, ok := strings.CutPrefix(k, "Layout#0.")
				if !ok {
					continue
				}
				if !reflect.DeepEqual(got[field], v) {
					return fmt.Errorf("path %d step %d (%s): %s is %v, the model says %v", pi, si, step.Action, field, got[field], v)
				}
			}
		}
		if err := a.Cleanup(); err != nil {
			return err
		}
	}
	return nil
}

// The runs above prove nothing unless a wrongly-wired adapter fails them.
// This one syncs the strip's signal off the page-wide width alone,
// dropping the panel: any transition where the panel opens or closes
// while the page itself stays wide diverges from the pane's own signal.
func TestRuntimeStripPaneNarrowToggleCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := &runtimeStripPaneNarrowToggleAdapter{stripIgnoresPanel: true}
	err := walkRuntimeStripPaneNarrowTogglePaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("a strip that ignores the panel walked every path; the walk is not checking state")
	}
	t.Logf("caught: %v", err)
}
