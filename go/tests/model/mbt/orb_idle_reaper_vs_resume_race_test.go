//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_idle_reaper_vs_resume_race.fizz against the real reaper
// (internal/serve/reaper.go reapIdleOrbs -> Supervisor.stopOrb ->
// orb.StopContainer) and the real writes a session's own child makes to
// state.json (orb.Command -> ensureRunningLocked), on the same file,
// with no lock in common.
//
// serve runs in process (Supervisor + API), on container.Fake wrapped
// to hold the reaper's rt.Stop call: ReapMark starts a.api.ReapIdleOrbs
// in a goroutine and returns once orb.StopContainer's MarkStopped has
// written "stopped" and its rt.Stop is waiting, which is how the window
// between the snapshot and the runtime's answer becomes a state.
// ReapStopOk and ReapStopFailRestore resolve it as the runtime really
// would (the container really stops, or the call errs and StopContainer
// restores its snapshot).
//
// There is no live child process: nothing the reaper consults for a
// running or failed orb (the only file statuses GoIdle allows) reads
// the owner or the container, so the child's writes are played directly
// on state.json, exactly as ensureRunningLocked itself would leave it —
// the point of the flow is what two writers do to one file, not whether
// a session's process is really alive. `owner` and `idle` are tracked by
// the adapter the way the example's `viewing` is: real state the client
// side knows and serve cannot see.

// oirFields is the Orb role's state.
type oirFields struct {
	file, ctr, reapPrev                       string
	owner, idle, stopping, touched, clobbered bool
}

func (o oirFields) state() map[string]any {
	return map[string]any{
		"file": o.file, "ctr": o.ctr, "owner": o.owner, "idle": o.idle,
		"stopping": o.stopping, "reap_prev": o.reapPrev, "touched": o.touched, "clobbered": o.clobbered,
	}
}

const oirIdle = time.Hour // the reaper's limit; the adapter owns the clock

var errOirStopRefused = errors.New("model: the runtime refused the stop")

// oirRuntime is container.Fake with a hold on the next Stop of one
// named container: the reaper's rt.Stop call inside orb.StopContainer
// waits there for the walk to say how it ends. Named, not just "the
// next Stop of anything": reapIdleOrbs walks every orb directory under
// home, and a previous walk's left one (also idle, also running or
// failed under the same forced clock) would otherwise steal the hold
// before it ever reached this walk's own container.
type oirRuntime struct {
	*container.Fake
	mu      sync.Mutex
	armed   string
	ch      chan string
	holding bool
}

func newOirRuntime() *oirRuntime { return &oirRuntime{Fake: container.NewFake()} }

func (r *oirRuntime) arm(name string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.armed, r.ch = name, make(chan string, 1)
}

func (r *oirRuntime) Stop(ctx context.Context, name string) error {
	r.mu.Lock()
	if r.armed != name {
		r.mu.Unlock()
		return r.Fake.Stop(ctx, name)
	}
	r.armed = ""
	ch := r.ch
	r.holding = true
	r.mu.Unlock()
	d := <-ch
	r.mu.Lock()
	r.holding = false
	r.mu.Unlock()
	if d == "ok" {
		return r.Fake.Stop(ctx, name)
	}
	return errOirStopRefused
}

func (r *oirRuntime) isHolding() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.holding
}

func (r *oirRuntime) resolve(d string) {
	r.mu.Lock()
	ch := r.ch
	r.mu.Unlock()
	ch <- d
}

type oirAdapter struct {
	t    *testing.T
	home string
	hist string
	rt   *oirRuntime
	sup  *serve.Supervisor
	api  *serve.API

	n     int
	id    string
	wrote time.Time

	// The client-side state the adapter tracks (like the example's
	// `viewing`): nothing serve reads for a running or failed orb.
	owner, idle, stopping, touched, clobbered bool
	reapPrev                                  string
	idleAt                                    time.Time

	reapDone chan struct{} // closed when ReapMark's goroutine returns

	gate   gate
	action string
	trace  []tracecheck.Step
	traces [][]tracecheck.Step

	// ignoreTouched is the deliberate wiring bug the wrong-adapter test
	// injects: never notice a write that landed while the reaper's own
	// call was in flight.
	ignoreTouched bool
}

func newOirAdapter(t *testing.T) *oirAdapter {
	root, err := os.MkdirTemp("", "boir-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &oirAdapter{t: t, home: filepath.Join(root, "home"), rt: newOirRuntime()}
	a.hist = filepath.Join(a.home, ".bough", "history")
	if err := os.MkdirAll(a.hist, 0o755); err != nil {
		t.Fatal(err)
	}
	a.rt.AddImage("img")
	a.sup, err = serve.NewSupervisor(serve.Options{
		HistDir:  a.hist,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + a.home, "PATH=" + os.Getenv("PATH")},
		Runtime:  a.rt,
	})
	if err != nil {
		t.Fatal(err)
	}
	a.api = serve.NewAPI(a.sup)
	t.Cleanup(func() { a.sup.Close() })
	return a
}

func (a *oirAdapter) name() string { return container.OrbName(a.id) }

// Init starts each walk on a fresh orb: running, its container up,
// owned, not idle — Init in the spec.
func (a *oirAdapter) Init() error {
	a.n++
	a.id = fmt.Sprintf("oir%04d", a.n)
	a.wrote, a.idleAt = time.Time{}, time.Time{}
	a.owner, a.idle, a.stopping, a.touched, a.clobbered = true, false, false, false, false
	a.reapPrev, a.reapDone = "", nil
	a.gate.reset()
	if err := os.MkdirAll(orb.Dir(a.home, a.id), 0o755); err != nil {
		return err
	}
	st := orb.State{Session: a.id, Project: "p", Image: "img", Container: a.name(), Status: orb.StatusRunning}
	st.UpdatedAt = time.Now().UTC()
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	if err := writeFileAtomic(filepath.Join(orb.Dir(a.home, a.id), "state.json"), b); err != nil {
		return err
	}
	a.wrote = st.UpdatedAt
	if err := a.rt.Start(context.Background(), container.RunSpec{Name: a.name(), Image: "img"}); err != nil {
		return err
	}
	a.api.ForgetRunning()
	a.trace = nil
	a.action = "Init"
	return nil
}

// Cleanup lets a stop the walk left holding resolve, so the reaper's
// goroutine is never left waiting for the next walk's actions, and
// removes this walk's orb directory: reapIdleOrbs walks every orb under
// home, and a walk left behind would be idle and reapable under the
// next walk's forced clock too.
func (a *oirAdapter) Cleanup() error {
	if a.stopping {
		a.rt.resolve("ok")
		<-a.reapDone
		a.stopping = false
	}
	if len(a.trace) > 0 {
		a.traces = append(a.traces, a.trace)
		a.trace = nil
	}
	return os.RemoveAll(orb.Dir(a.home, a.id))
}

func (a *oirAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Orb", Index: 0}: a}, nil
}

// observe reads file and ctr off the ground truth and the rest off the
// adapter's own tracked client state.
func (a *oirAdapter) observe() (oirFields, error) {
	var o oirFields
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return o, err
	}
	o.file = string(st.Status)
	cs, err := a.rt.Inspect(context.Background(), a.name())
	if err != nil {
		return o, err
	}
	o.ctr = string(cs)
	o.owner, o.idle, o.stopping = a.owner, a.idle, a.stopping
	o.reapPrev, o.touched, o.clobbered = a.reapPrev, a.touched, a.clobbered
	return o, nil
}

func oirQualify(st map[string]any) map[string]any {
	q := make(map[string]any, len(st))
	for k, v := range st {
		q["Orb#0."+k] = v
	}
	return q
}

// GetState is the role's state, after recording the step just taken.
func (a *oirAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	if a.action != "" {
		name := a.action
		if name != "Init" {
			name = "Orb#0." + name
		}
		a.trace = append(a.trace, tracecheck.Step{Action: name, State: oirQualify(o.state())})
		a.action = ""
	}
	return o.state(), nil
}

// write is one state.json write by the owning child: it applies to
// what is on disk now (the reaper may have marked it stopped, or
// restored an older snapshot over it) and moves updatedAt forward, as
// orb's writeState does.
func (a *oirAdapter) write(edit func(*orb.State)) error {
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	edit(&st)
	st.UpdatedAt = time.Now().UTC()
	if !st.UpdatedAt.After(a.wrote) {
		st.UpdatedAt = a.wrote.Add(time.Microsecond)
	}
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	if err := writeFileAtomic(filepath.Join(orb.Dir(a.home, a.id), "state.json"), b); err != nil {
		return err
	}
	a.wrote = st.UpdatedAt
	return nil
}

// --- the session ---

func (a *oirAdapter) GoIdle() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(!o.idle && (o.file == "running" || o.file == "failed")) {
		return nil
	}
	a.action = "GoIdle"
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	a.idleAt = st.UpdatedAt
	a.idle = true
	return nil
}

func (a *oirAdapter) Activity() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.owner && o.idle && o.file == "running" && o.ctr == "running") {
		return nil
	}
	a.action = "Activity"
	a.idle = false
	return nil
}

// OwnerExits: the child exits; an owned, up container is stopped as
// part of that exit, straight through the runtime with no MarkStopped
// of its own, which can land while the reaper's own stop of the same
// container is in flight.
func (a *oirAdapter) OwnerExits() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.owner && o.file != "starting") {
		return nil
	}
	a.action = "OwnerExits"
	a.owner = false
	if o.ctr == "running" {
		if err := a.rt.Fake.Stop(context.Background(), a.name()); err != nil {
			return err
		}
	}
	return nil
}

// Command is ensureRunningLocked's slow path: it writes state.json
// "starting" whatever the reaper is doing to the same file right now.
func (a *oirAdapter) Command() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.ctr != "running" && o.file != "starting") {
		return nil
	}
	a.action = "Command"
	if o.stopping {
		a.touched = true
	}
	a.owner, a.idle = true, false
	return a.write(func(s *orb.State) { s.Status, s.Phase = orb.StatusStarting, orb.PhaseContainer })
}

func (a *oirAdapter) StartOk() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.file == "starting") {
		return nil
	}
	a.action = "StartOk"
	if o.stopping {
		a.touched = true
	}
	if err := a.rt.Fake.Start(context.Background(), container.RunSpec{Name: a.name(), Image: "img"}); err != nil {
		return err
	}
	return a.write(func(s *orb.State) { s.Status, s.Phase = orb.StatusRunning, "" })
}

func (a *oirAdapter) StartFails() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.file == "starting") {
		return nil
	}
	a.action = "StartFails"
	if o.stopping {
		a.touched = true
	}
	return a.write(func(s *orb.State) { s.Status, s.Error = orb.StatusFailed, "model: start failed" })
}

// --- the reaper ---

// ReapMark is reapIdleOrbs finding this orb idle, then
// orb.StopContainer's MarkStopped: it returns once the file says
// "stopped" and the real rt.Stop is waiting for the walk to answer.
func (a *oirAdapter) ReapMark() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.idle && (o.file == "running" || o.file == "failed") && !o.stopping) {
		return nil
	}
	a.action = "ReapMark"
	a.reapPrev = o.file
	a.touched = false
	a.rt.arm(a.name())
	done := make(chan struct{})
	a.reapDone = done
	go func() {
		defer close(done)
		a.api.ReapIdleOrbs(context.Background(), oirIdle, a.idleAt.Add(oirIdle))
	}()
	if err := waitUntil("the reaper's rt.Stop", a.rt.isHolding); err != nil {
		return err
	}
	a.stopping = true
	return nil
}

// ReapStopOk: rt.Stop succeeds, the container really goes down.
func (a *oirAdapter) ReapStopOk() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.stopping && o.ctr == "running") {
		return nil
	}
	a.action = "ReapStopOk"
	a.rt.resolve("ok")
	<-a.reapDone
	a.api.ForgetRunning()
	a.stopping, a.reapPrev = false, ""
	return nil
}

// ReapStopFailRestore: rt.Stop fails and StopContainer's Restore writes
// its snapshot back over whatever the file says now, unconditionally —
// clobbering a write the child made while the call was in flight.
func (a *oirAdapter) ReapStopFailRestore() error {
	o, err := a.observe()
	if err != nil {
		return err
	}
	if !a.gate.pass(o.stopping) {
		return nil
	}
	a.action = "ReapStopFailRestore"
	if o.touched && !a.ignoreTouched {
		a.clobbered = true
	}
	a.rt.resolve("err")
	<-a.reapDone
	a.api.ForgetRunning()
	a.stopping, a.reapPrev = false, ""
	return nil
}

var oirActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"GoIdle":              action((*oirAdapter).GoIdle),
	"Activity":            action((*oirAdapter).Activity),
	"OwnerExits":          action((*oirAdapter).OwnerExits),
	"Command":             action((*oirAdapter).Command),
	"StartOk":             action((*oirAdapter).StartOk),
	"StartFails":          action((*oirAdapter).StartFails),
	"ReapMark":            action((*oirAdapter).ReapMark),
	"ReapStopOk":          action((*oirAdapter).ReapStopOk),
	"ReapStopFailRestore": action((*oirAdapter).ReapStopFailRestore),
}}

// oirDiff compares the spec's role fields with the adapter's.
func oirDiff(want map[string]any, got map[string]any) string {
	var diffs []string
	for k, w := range want {
		f, ok := strings.CutPrefix(k, "Orb#0.")
		if !ok {
			continue // "orb": the role reference itself
		}
		wj, _ := json.Marshal(w)
		gj, _ := json.Marshal(got[f])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", f, wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

// walkOirPaths drives the adapter down every walk the graph gives and
// returns the first step whose state is not the spec's.
func walkOirPaths(t *testing.T, a *oirAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("orb_idle_reaper_vs_resume_race", cover)
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
		t.Fatal("no walks over testdata/orb_idle_reaper_vs_resume_race")
	}
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", pi, err)
		}
		err := func() error {
			for si, s := range p.Trace {
				if si > 0 {
					name := strings.TrimPrefix(s.Action, "Orb#0.")
					f, ok := oirActions["Orb"][name]
					if !ok {
						return fmt.Errorf("step %d: no adapter action for %s", si, s.Action)
					}
					if _, err := f(a, nil); err != nil {
						return fmt.Errorf("step %d (%s): %w", si, s.Action, err)
					}
					if a.gate.off {
						return fmt.Errorf("step %d (%s): the adapter's view says it is not enabled", si, s.Action)
					}
				}
				got, err := a.GetState()
				if err != nil {
					return fmt.Errorf("step %d (%s): %w", si, s.Action, err)
				}
				if diff := oirDiff(s.State, got); diff != "" {
					return fmt.Errorf("step %d (%s): %s", si, s.Action, diff)
				}
			}
			return nil
		}()
		if cerr := a.Cleanup(); err == nil && cerr != nil {
			err = fmt.Errorf("Cleanup: %w", cerr)
		}
		if err != nil {
			b, _ := json.Marshal(p.Trace)
			return fmt.Errorf("walk %d: %w\nwalk: %s", pi, err, b)
		}
	}
	return nil
}

// checkOirTraces replays every walk's own record on the graph: this
// flow keeps no session, so there is no transcript to check it against.
func checkOirTraces(t *testing.T, a *oirAdapter) {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("orb_idle_reaper_vs_resume_race")), "..", "testdata", "orb_idle_reaper_vs_resume_race"))
	if err != nil {
		t.Fatal(err)
	}
	steps := 0
	for i, tr := range a.traces {
		if v := g.Check(tr); v != nil {
			b, _ := json.Marshal(tr)
			t.Fatalf("walk %d is not a path in the model: %v\ntrace: %s", i, v, b)
		}
		steps += len(tr) - 1
	}
	if steps == 0 {
		t.Fatal("no walk took a step")
	}
	t.Logf("%d walks, %d steps replayed on the graph", len(a.traces), steps)
}

// Every settled state of the spec against the real reaper and a
// simulated child (every transition under MODEL_COVER=transitions).
func TestOrbIdleReaperVsResumeRacePaths(t *testing.T) {
	t.Parallel()
	a := newOirAdapter(t)
	if err := walkOirPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
	checkOirTraces(t, a)
}

// The runner's random walks, in the exhaustive run only (runMBT).
func TestOrbIdleReaperVsResumeRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOirAdapter(t)
	if err := runMBT(t, "orb_idle_reaper_vs_resume_race", a, oirActions, map[string]any{"max-seq-runs": 100, "max-actions": 20, "max-parallel-runs": 0}); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	checkOirTraces(t, a)
}

// The walk proves nothing unless a wiring bug that misses a clobbered
// write fails it: here the adapter never notices touched, so it reports
// clobbered=false where the file really was overwritten.
func TestOrbIdleReaperVsResumeRacePathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOirAdapter(t)
	a.ignoreTouched = true
	err := walkOirPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("walks whose adapter never notices a clobbered write passed; the walk is not checking state")
	}
	if !strings.Contains(err.Error(), "clobbered") {
		t.Fatalf("caught, but not on clobbered: %v", err)
	}
	t.Logf("caught: %v", err)
}
