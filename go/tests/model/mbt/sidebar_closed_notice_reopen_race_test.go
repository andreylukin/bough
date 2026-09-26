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

// specs/sidebar_closed_notice_reopen_race.fizz: the sidebar's
// closed/open state and whether a finish notice just force-reopened it.
// Close and Open are the person's own clicks, entirely client state with
// nothing for a real serve to observe, so the adapter tracks them itself
// exactly as it tracks Session#0.viewing in the worked example. Notice
// is the one action that must be real: it drives an actual turn to
// completion on a real serve (the notice a finish delivers), then
// records the unconditional force-reopen the product performs.
type sidebarAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string
	gate gate

	id   string   // this walk's session
	ids  []string // every session a walk created, checked after all walks
	turn int      // turn names are unique across walks: the queue is shared

	closed       bool
	justNotified bool

	// noticeIgnoresClosed is the deliberate wiring bug
	// TestSidebarClosedNoticeReopenRaceCatchesWrongAdapter injects: Notice
	// leaves a closed sidebar closed instead of always force-reopening it.
	noticeIgnoresClosed bool
}

func newSidebarAdapter(t *testing.T) *sidebarAdapter {
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &sidebarAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

// Init starts each walk on a fresh session in the same serve, and the
// sidebar open with no notice pending, as the spec's Init has it.
func (a *sidebarAdapter) Init() error {
	ctx, cancel := actionCtx()
	defer cancel()
	row, err := a.s.CreateSession(ctx, a.s.Dir(a.t, "work"), "")
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.closed, a.justNotified = false, false
	a.gate.reset()
	return nil
}

func (a *sidebarAdapter) Cleanup() error { return nil }

func (a *sidebarAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Sidebar", Index: 0}: a}, nil
}

// GetState is the Sidebar role's state: both fields are the client's own
// state (nothing a real serve stores), tracked by the adapter the way
// the example tracks Session#0.viewing.
func (a *sidebarAdapter) GetState() (map[string]any, error) {
	return map[string]any{"closed": a.closed, "justNotified": a.justNotified}, nil
}

func (a *sidebarAdapter) Close() error {
	if !a.gate.pass(!a.closed) {
		return nil
	}
	a.closed = true
	a.justNotified = false
	return nil
}

func (a *sidebarAdapter) Open() error {
	if !a.gate.pass(a.closed) {
		return nil
	}
	a.closed = false
	a.justNotified = false
	return nil
}

// Notice has no precondition: a finish notice arrives and force-reopens
// the sidebar whatever it was doing. It drives one real turn to
// completion so the notice is a genuine finish, not a simulated one.
func (a *sidebarAdapter) Notice() error {
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
	control.Release(a.t, a.dir, name)
	if _, err := waitRow(a.s, a.id, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	if a.noticeIgnoresClosed && a.closed {
		a.justNotified = true
		return nil
	}
	a.closed = false
	a.justNotified = true
	return nil
}

var sidebarClosedNoticeReopenRaceActions = map[string]map[string]fmbt.ActionFunc{"Sidebar": {
	"Close":  action((*sidebarAdapter).Close),
	"Open":   action((*sidebarAdapter).Open),
	"Notice": action((*sidebarAdapter).Notice),
}}

func sidebarClosedNoticeReopenRaceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 6, "max-parallel-runs": 0}
}

// sidebarClosedNoticeReopenRaceHistory reads the abstract trace off a
// transcript: Close and Open leave nothing in history (client-only, like
// View/Leave in the worked example), and a turn's close is Notice, the
// only action that ever touches the server.
func sidebarClosedNoticeReopenRaceHistory(entries []history.Entry) []tracecheck.Step {
	state := func(closed, justNotified bool) map[string]any {
		return map[string]any{"Sidebar#0.closed": closed, "Sidebar#0.justNotified": justNotified}
	}
	steps := []tracecheck.Step{{Action: "Init", State: state(false, false)}}
	for _, e := range entries {
		if e.Kind == "done" {
			steps = append(steps, tracecheck.Step{Action: "Sidebar#0.Notice", State: state(false, true)})
		}
	}
	return steps
}

func init() {
	historyProjections["sidebar_closed_notice_reopen_race"] = sidebarClosedNoticeReopenRaceHistory
}

// TestSidebarClosedNoticeReopenRacePaths walks every path the generator
// derives from the spec's graph (loadWalks/walkRole, shared with the
// hung_* flows: harness_test.go), each against its own fresh session on
// one serve. This is the deterministic run the default gate checks; the
// random fizzbee-mbt run below only runs in the nightly exhaustive job.
func TestSidebarClosedNoticeReopenRacePaths(t *testing.T) {
	t.Parallel()
	paths := loadWalks(t, "sidebar_closed_notice_reopen_race", envCover())
	a := newSidebarAdapter(t)
	for i, p := range paths {
		if err := walkRole(a, "Sidebar", sidebarClosedNoticeReopenRaceActions, &a.gate, p); err != nil {
			t.Errorf("path %d: %v", i, err)
		}
	}
	g, err := tracecheck.Load(testdataDir("sidebar_closed_notice_reopen_race"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sidebarClosedNoticeReopenRaceHistory)
	}
}

// The walks above prove nothing unless a wiring bug in the adapter fails
// them: this one lets Notice respect a closed sidebar instead of always
// force-reopening it, which the spec's NoticeLeavesOpen assertion (and
// the graph it produces) forbids. CoverTransitions walks every link, so
// a path that closes the sidebar right before a Notice is guaranteed.
func TestSidebarClosedNoticeReopenRacePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newSidebarAdapter(t)
	a.noticeIgnoresClosed = true
	for _, p := range loadWalks(t, "sidebar_closed_notice_reopen_race", tracecheck.CoverTransitions) {
		if err := walkRole(a, "Sidebar", sidebarClosedNoticeReopenRaceActions, &a.gate, p); err != nil {
			t.Logf("caught: %v", err)
			return
		}
	}
	t.Fatal("every path passed with a Notice that respects a closed sidebar; the walk is not checking state")
}

// TestSidebarClosedNoticeReopenRace is the random fizzbee-mbt run,
// exercised only in the nightly exhaustive job (runMBT skips otherwise).
func TestSidebarClosedNoticeReopenRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newSidebarAdapter(t)
	if err := runMBT(t, "sidebar_closed_notice_reopen_race", a, sidebarClosedNoticeReopenRaceActions, sidebarClosedNoticeReopenRaceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "sidebar_closed_notice_reopen_race"))
	if err != nil {
		t.Fatal(err)
	}
	for _, id := range a.ids {
		checkHistory(t, g, sessionHistory(t, a.s.Home, id), sidebarClosedNoticeReopenRaceHistory)
	}
}
