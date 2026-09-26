//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"testing"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/unreal/hookbridge"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/hookbridge_session_attribution_race.fizz: two callers (a parent
// session and a subagent) sharing one hooks.Firer through their own
// hookbridge.Bridge. The spec's two Firer shapes are two real Go types
// here: harRaceFirer has only the old SetSession/Fire pair (the
// fallback plugins/hooks.Service used to be reached through before
// FireAs existed), harSafeFirer has FireAs/TakeFireRecordsFor (what
// plugins/hooks.Service is today). Bridge.fire picks the branch by type
// assertion alone, so swapping the adapter's Firer between the two
// types is what UseSafeFirer models.
//
// The race itself needs no fake I/O to park on (see
// orb_build_concurrency's file comment for the pattern this borrows):
// harRaceFirer.SetSession blocks in place until the adapter's *_Fire
// action releases it, so A_SetSession and A_Fire really are two
// separate halves of one goroutine's call into the real, unmodified
// hookbridge.Bridge.fire, with the other caller's SetSession free to
// land between them exactly as two real goroutines would.

// harRaceFirer is the fallback shape: SetSession names the shared,
// process-wide "current" session, and Fire (later, maybe after the
// other caller renamed it) records under whatever is current when it
// runs. armed/release model the goroutine boundary: SetSession blocks
// after naming current until the test's *_Fire action lets it through.
type harRaceFirer struct {
	mu      sync.Mutex
	current string
	armed   map[string]chan struct{} // id -> closed once SetSession has set current and is parked
	release map[string]chan struct{} // id -> closed to let SetSession return
	attr    map[string]string        // id -> what Fire actually read as current
}

func newHARRaceFirer() *harRaceFirer {
	return &harRaceFirer{armed: map[string]chan struct{}{}, release: map[string]chan struct{}{}, attr: map[string]string{}}
}

// arm prepares id's parking point before the goroutine that will call
// SetSession(id) starts, so the test can wait for it to actually park.
func (f *harRaceFirer) arm(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.armed[id] = make(chan struct{})
	f.release[id] = make(chan struct{})
}

func (f *harRaceFirer) SetSession(id string) {
	f.mu.Lock()
	f.current = id
	armed, release := f.armed[id], f.release[id]
	f.mu.Unlock()
	close(armed)
	<-release
}

// waitParked blocks until id's SetSession call has set current and is
// waiting to be released.
func (f *harRaceFirer) waitParked(id string) {
	f.mu.Lock()
	ch := f.armed[id]
	f.mu.Unlock()
	<-ch
}

// releaseAndWait lets id's parked SetSession return (so its Fire runs)
// and waits for the caller's goroutine (done) to finish recording.
func (f *harRaceFirer) releaseAndWait(id string, done chan struct{}) {
	f.mu.Lock()
	ch := f.release[id]
	f.mu.Unlock()
	close(ch)
	<-done
}

// Fire is what plugins/hooks.Service.Fire's fallback callers actually
// read: whatever SetSession most recently named, not who is asking.
// harWho pulls "who is asking" out of the context purely so the test
// can tell A's outcome from B's; the product itself has no such thing,
// which is exactly the bug this spec is about.
type harWhoKey struct{}

func (f *harRaceFirer) Fire(ctx context.Context, event string, _ map[string]any) (map[string]any, error) {
	// The context value is test instrumentation only, so the mismatch
	// can be pinned on A or B; plugins/hooks.Service's real Fire has no
	// such thing to read and answers off f.current alone, which is
	// exactly the bug this spec is about.
	who, _ := ctx.Value(harWhoKey{}).(string)
	f.mu.Lock()
	defer f.mu.Unlock()
	if who != "" {
		f.attr[who] = f.current
	}
	return nil, nil
}

func (f *harRaceFirer) TakeFireRecords() []map[string]any { return nil }

func (f *harRaceFirer) currentName() string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.current
}

// harSafeFirer is today's shape: the session travels with the call, so
// there is nothing shared to land on. attr[session] is trivially
// session; it exists so the adapter reads its answer the same way for
// both modes, off a real call into the type, not an assumption.
// misattribute is the wrong-adapter test's injected bug: a safe Firer
// that nonetheless credits the fire to the other of the two callers.
type harSafeFirer struct {
	mu           sync.Mutex
	attr         map[string]string
	misattribute bool
}

func newHARSafeFirer() *harSafeFirer { return &harSafeFirer{attr: map[string]string{}} }

func otherHARCaller(session string) string {
	if session == "A" {
		return "B"
	}
	return "A"
}

func (f *harSafeFirer) FireAs(_ context.Context, session, _ string, _ map[string]any) (map[string]any, error) {
	f.mu.Lock()
	if f.misattribute {
		f.attr[session] = otherHARCaller(session)
	} else {
		f.attr[session] = session
	}
	f.mu.Unlock()
	return nil, nil
}

func (f *harSafeFirer) TakeFireRecordsFor(session string) []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.attr, session)
	return nil
}

// Fire and TakeFireRecords only satisfy hookbridge.Firer's static
// shape; Bridge.fire always prefers FireAs/TakeFireRecordsFor when a
// Firer has them, so these never run.
func (f *harSafeFirer) Fire(context.Context, string, map[string]any) (map[string]any, error) {
	return nil, fmt.Errorf("harSafeFirer.Fire: unreachable, FireAs should have been used")
}
func (f *harSafeFirer) TakeFireRecords() []map[string]any { return nil }

func (f *harSafeFirer) attrOf(session string) string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.attr[session]
}

// harAdapter is both the fmbt.Model and the spec's one Flow role.
type harAdapter struct {
	t    *testing.T
	gate gate

	mode           string // "fallback" | "safe"
	aPhase, bPhase string // "idle" | "set" | "fired"
	aAttr, bAttr   string

	fb   *harRaceFirer
	safe *harSafeFirer
	a, b *hookbridge.Bridge

	aDone, bDone chan struct{}

	// bugSafe is TestHookbridgeSessionAttributionRaceCatchesWrongAdapter's
	// injected bug: a safe Firer that misattributes.
	bugSafe bool
}

func newHARAdapter(t *testing.T) *harAdapter {
	a := &harAdapter{t: t}
	get := func() (hookbridge.Firer, bool) {
		if a.mode == "safe" {
			return a.safe, true
		}
		return a.fb, true
	}
	a.a = hookbridge.New(get)
	a.a.Session = "A"
	a.b = hookbridge.New(get)
	a.b.Session = "B"
	return a
}

// Init resets to a fresh, all-idle Flow: fallback mode, one Firer of
// each shape (fresh, so a leftover parked goroutine from a walk that
// stopped early never bleeds into the next).
func (a *harAdapter) Init() error {
	if err := a.Cleanup(); err != nil {
		return err
	}
	a.mode, a.aPhase, a.bPhase, a.aAttr, a.bAttr = "fallback", "idle", "idle", "", ""
	a.fb, a.safe = newHARRaceFirer(), newHARSafeFirer()
	a.safe.misattribute = a.bugSafe
	a.aDone, a.bDone = nil, nil
	a.gate.reset()
	return nil
}

// Cleanup lets a walk that stopped mid-race finish its parked
// goroutine(s) rather than leaking them into the next walk.
func (a *harAdapter) Cleanup() error {
	if a.fb == nil {
		return nil
	}
	if a.aPhase == "set" && a.aDone != nil {
		a.fb.releaseAndWait("A", a.aDone)
		a.aDone = nil
	}
	if a.bPhase == "set" && a.bDone != nil {
		a.fb.releaseAndWait("B", a.bDone)
		a.bDone = nil
	}
	return nil
}

func (a *harAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Flow", Index: 0}: a}, nil
}

func (a *harAdapter) GetState() (map[string]any, error) {
	current := ""
	if a.mode == "fallback" {
		current = a.fb.currentName()
	}
	return map[string]any{
		"mode": a.mode, "a_phase": a.aPhase, "b_phase": a.bPhase,
		"current": current, "a_attr": a.aAttr, "b_attr": a.bAttr,
	}, nil
}

func (a *harAdapter) UseSafeFirer() error {
	if a.gate.pass(a.mode == "fallback" && a.aPhase == "idle" && a.bPhase == "idle") {
		a.mode = "safe"
	}
	return nil
}

// setSession is A_SetSession/B_SetSession: fires a real Bridge.Stop
// call in its own goroutine and waits until it is really parked inside
// harRaceFirer.SetSession, current already set.
func (a *harAdapter) setSession(id string, b *hookbridge.Bridge, phase *string, done *chan struct{}) error {
	if !a.gate.pass(a.mode == "fallback" && *phase == "idle") {
		return nil
	}
	a.fb.arm(id)
	*done = make(chan struct{})
	d := *done
	ctx := context.WithValue(context.Background(), harWhoKey{}, id)
	go func() {
		b.Stop(ctx, "x")
		close(d)
	}()
	a.fb.waitParked(id)
	*phase = "set"
	return nil
}

// fire is A_Fire/B_Fire: safe mode fires straight through (nothing
// shared to race on); fallback mode releases the parked SetSession, so
// the real Bridge.fire's Fire call runs now, against whatever
// current is at that instant.
func (a *harAdapter) fire(id string, b *hookbridge.Bridge, phase, attr *string, done *chan struct{}) error {
	safeReq := a.mode == "safe" && *phase == "idle"
	fallbackReq := a.mode == "fallback" && *phase == "set"
	if !a.gate.pass(safeReq || fallbackReq) {
		return nil
	}
	if a.mode == "safe" {
		b.Stop(context.Background(), "x")
		*attr = a.safe.attrOf(id)
	} else {
		a.fb.releaseAndWait(id, *done)
		*done = nil
		a.fb.mu.Lock()
		*attr = a.fb.attr[id]
		a.fb.mu.Unlock()
	}
	*phase = "fired"
	return nil
}

func (a *harAdapter) A_SetSession() error { return a.setSession("A", a.a, &a.aPhase, &a.aDone) }
func (a *harAdapter) B_SetSession() error { return a.setSession("B", a.b, &a.bPhase, &a.bDone) }
func (a *harAdapter) A_Fire() error       { return a.fire("A", a.a, &a.aPhase, &a.aAttr, &a.aDone) }
func (a *harAdapter) B_Fire() error       { return a.fire("B", a.b, &a.bPhase, &a.bAttr, &a.bDone) }

var harActions = map[string]map[string]fmbt.ActionFunc{"Flow": {
	"UseSafeFirer": action((*harAdapter).UseSafeFirer),
	"A_SetSession": action((*harAdapter).A_SetSession),
	"B_SetSession": action((*harAdapter).B_SetSession),
	"A_Fire":       action((*harAdapter).A_Fire),
	"B_Fire":       action((*harAdapter).B_Fire),
}, "": {
	// deadlock_detection is off, so fizz links every resting state
	// (both callers fired) to itself as a role-less "end".
	"end": func(m any, _ []fmbt.Arg) (any, error) {
		m.(*harAdapter).gate.pass(false)
		return nil, errDisabled
	},
}}

func harOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

func TestHookbridgeSessionAttributionRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHARAdapter(t)
	if err := runMBT(t, "hookbridge_session_attribution_race", a, harActions, harOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// Nothing here proves anything unless a safe Firer that actually
// misattributes fails it: bugSafe makes harSafeFirer.FireAs credit
// every fire to the other of the two callers, which
// SafeFirerNeverMisattributes forbids outright, on the very first
// A_Fire or B_Fire the walk takes in safe mode.
func TestHookbridgeSessionAttributionRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newHARAdapter(t)
	a.bugSafe = true
	err := runMBT(t, "hookbridge_session_attribution_race", a, harActions, harOptions())
	if err == nil {
		t.Fatal("a run whose safe Firer misattributes a fire passed; the runner is not checking state")
	}
	if strings.Contains(err.Error(), "failed to listen") {
		t.Fatalf("the runner did not start: %v", err)
	}
	t.Log(err)
}

// --- deterministic walk over the checked-in graph, no fizz tools -------

// walkHARPaths drives a fresh adapter down every walk in b, comparing
// the adapter's state with the spec's after every step, and returns one
// message per mismatch (stopping at the first when firstOnly).
func walkHARPaths(t *testing.T, cover tracecheck.Cover, firstOnly bool, buggy bool) []string {
	t.Helper()
	b, err := pathsJSONCover("hookbridge_session_attribution_race", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no paths over testdata/hookbridge_session_attribution_race")
	}
	var bad []string
	for i, p := range doc.Paths {
		a := newHARAdapter(t)
		a.bugSafe = buggy
	steps:
		for j, step := range p.Trace {
			var err error
			switch {
			case j == 0:
				err = a.Init()
			case step.Action == "end":
			default:
				name := strings.TrimPrefix(step.Action, "Flow#0.")
				f, ok := harActions["Flow"][name]
				if !ok {
					t.Fatalf("path %d: no adapter action for %s", i, step.Action)
				}
				_, err = f(a, nil)
			}
			if err == nil && a.gate.off {
				err = fmt.Errorf("the adapter found %s disabled", step.Action)
			}
			var got map[string]any
			if err == nil {
				got, err = a.GetState()
			}
			if err != nil {
				bad = append(bad, fmt.Sprintf("path %d step %d (%s): %v", i, j, step.Action, err))
				break
			}
			for k, want := range step.State {
				field, ok := strings.CutPrefix(k, "Flow#0.")
				if ok && got[field] != want {
					bad = append(bad, fmt.Sprintf("path %d step %d (%s): %s is %v, the spec says %v", i, j, step.Action, field, got[field], want))
					break steps
				}
			}
		}
		if err := a.Cleanup(); err != nil {
			t.Fatal(err)
		}
		if firstOnly && len(bad) > 0 {
			break
		}
	}
	return bad
}

// TestHookbridgeSessionAttributionRacePaths needs no fizz tools: the
// walks come from the checked-in graph.
func TestHookbridgeSessionAttributionRacePaths(t *testing.T) {
	t.Parallel()
	for _, b := range walkHARPaths(t, envCover(), false, false) {
		t.Error(b)
	}
}

// The safe-mode misattribution bug shows only where a Fire actually
// happens, so this covers every transition and stops at the first miss.
func TestHookbridgeSessionAttributionRacePathsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	bad := walkHARPaths(t, tracecheck.CoverTransitions, true, true)
	if len(bad) == 0 {
		t.Fatal("a path walk whose safe Firer misattributes A's fire passed; it is not checking state")
	}
	t.Log(bad[0])
}
