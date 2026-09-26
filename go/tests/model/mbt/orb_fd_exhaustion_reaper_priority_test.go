//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_fd_exhaustion_reaper_priority.fizz against the real reaper
// (internal/serve/reaper.go reapIdleOrbs) and a real ensureRunningLocked
// restart (internal/orb/restart.go, internal/orb/orb.go
// ensureRunningLocked): two orbs, each on its own serve so one walk's
// reap tick never sees the other's directory, since reapIdleOrbs scans
// every orb under its home in one pass.
//
// owner is real: state.json's PID names a real process this test
// spawns and holds while owner is True, so ownerAlive reads it exactly
// as it would a live child's; owner going False kills that process
// without writing state.json again, the way a real crash or a clean
// exit writes nothing either — a write here would shorten how idle
// reapIdleOrbs (idle = now since the last write) sees the orb by
// exactly as much. ctr is real too, through container.Fake:
// StartRestart brings the container up as ensureRunningLocked's own
// rt.Start does — the incident's stuck orbs were VMs left "running"
// under a "starting"/"building" file (reaper.go's own comment), not
// VMs that never came up. idle is real time via the adapter's own
// clock (like orb_idle_reaper_vs_resume_race's oirIdle), and
// restarting has no real counterpart on disk: it is the adapter's own
// tracked flag, the way the example's `viewing` is.

// ofrpFields is the Orb role's state.
type ofrpFields struct {
	file, ctr                                  string
	owner, restarting, idle, reapedLiveRestart bool
}

func (o ofrpFields) state() map[string]any {
	return map[string]any{
		"file": o.file, "ctr": o.ctr, "owner": o.owner,
		"restarting": o.restarting, "idle": o.idle, "reapedLiveRestart": o.reapedLiveRestart,
	}
}

const ofrpIdle = time.Hour // the reaper's limit; the adapter owns the clock

// ofrpHolderCmd is a real, long-lived process an owner-True write's PID
// names: ownerAlive reads it off pidAlive with no write of our own, so
// an owner going False costs no disk write and does not touch
// state.json's updatedAt — a real crash writes nothing either, and a
// write here would shorten how idle reapIdleOrbs (idle = now since the
// last write) sees the orb by exactly as much.
func ofrpHolderCmd() *exec.Cmd { return exec.Command("sleep", "3600") }

// ofrpOrb is one Orb role instance, its own serve: home, runtime and
// supervisor. Independent per instance so a reap tick started for one
// never walks the other's orb directory.
type ofrpOrb struct {
	t     *testing.T
	label string
	home  string
	rt    *container.Fake
	sup   *serve.Supervisor
	api   *serve.API

	n  int
	id string

	// The adapter's own tracked state: owner and idle are backed by a
	// real disk write (PID, and the wall-clock last-write time reapIdleOrbs
	// itself reads); restarting has no disk counterpart at all.
	owner, restarting, idle, reapedLiveRestart bool
	idleAt                                     time.Time
	wrote                                      time.Time
	holder                                     *exec.Cmd // the pid on disk when owner is True; killed, never rewritten, when it goes False

	gate   *gate // shared with the sibling orb: one walk, one require chain
	action string
	trace  *[]tracecheck.Step

	// forgetOwnerExit is the deliberate wiring bug
	// TestOrbFdExhaustionReaperPriorityPathsCatchesWrongAdapter injects:
	// the real owning process still dies, but the adapter's own owner
	// bookkeeping never notices, the way a real client that missed the
	// exit notification would keep believing the child alive.
	forgetOwnerExit bool
}

func newOfrpOrb(t *testing.T, label string, g *gate, trace *[]tracecheck.Step) *ofrpOrb {
	root, err := os.MkdirTemp("", "bofrp-"+label+"-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	o := &ofrpOrb{t: t, label: label, home: filepath.Join(root, "home"), rt: container.NewFake(), gate: g, trace: trace}
	hist := filepath.Join(o.home, ".bough", "history")
	if err := os.MkdirAll(hist, 0o755); err != nil {
		t.Fatal(err)
	}
	o.rt.AddImage("img")
	o.sup, err = serve.NewSupervisor(serve.Options{
		HistDir:  hist,
		MetaPath: filepath.Join(o.home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + o.home, "PATH=" + os.Getenv("PATH")},
		Runtime:  o.rt,
	})
	if err != nil {
		t.Fatal(err)
	}
	o.api = serve.NewAPI(o.sup)
	t.Cleanup(func() { o.sup.Close() })
	return o
}

func (o *ofrpOrb) name() string { return container.OrbName(o.id) }

// spawnHolder starts a fresh real process and returns its pid: the
// value an owner-True write puts in state.json's PID.
func (o *ofrpOrb) spawnHolder() (int, error) {
	o.holder = ofrpHolderCmd()
	if err := o.holder.Start(); err != nil {
		return 0, fmt.Errorf("spawn holder: %w", err)
	}
	return o.holder.Process.Pid, nil
}

// killHolder ends the process named by the last owner-True write,
// without writing anything: a real crash or a clean exit leaves
// state.json exactly as it was, and pidAlive is the only thing that
// changes.
func (o *ofrpOrb) killHolder() error {
	if o.holder == nil {
		return nil
	}
	h := o.holder
	o.holder = nil
	if err := h.Process.Kill(); err != nil {
		return fmt.Errorf("kill holder: %w", err)
	}
	h.Wait()
	return nil
}

// Init starts a fresh walk on this orb: running, its (fake) container
// up, owned, not idle, not restarting — Init in the spec.
func (o *ofrpOrb) Init() error {
	if err := o.killHolder(); err != nil {
		return err
	}
	o.n++
	o.id = fmt.Sprintf("ofrp%s%04d", o.label, o.n)
	o.wrote, o.idleAt = time.Time{}, time.Time{}
	o.owner, o.restarting, o.idle, o.reapedLiveRestart = true, false, false, false
	if err := os.MkdirAll(orb.Dir(o.home, o.id), 0o755); err != nil {
		return err
	}
	pid, err := o.spawnHolder()
	if err != nil {
		return err
	}
	st := orb.State{Session: o.id, Image: "img", Container: o.name(), Status: orb.StatusRunning, PID: pid}
	st.UpdatedAt = time.Now().UTC()
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	if err := writeFileAtomic(filepath.Join(orb.Dir(o.home, o.id), "state.json"), b); err != nil {
		return err
	}
	o.wrote = st.UpdatedAt
	if err := o.rt.Start(context.Background(), container.RunSpec{Name: o.name(), Image: "img"}); err != nil {
		return err
	}
	o.api.ForgetRunning()
	o.action = "Init"
	return nil
}

// cleanupDir kills any holder the walk left running and removes this
// walk's orb directory, so the next walk's reap tick never sees it:
// reapIdleOrbs walks every orb under home, and a walk left behind would
// be idle and reapable under the next walk's forced clock too.
func (o *ofrpOrb) cleanupDir() error {
	if err := o.killHolder(); err != nil {
		return err
	}
	return os.RemoveAll(orb.Dir(o.home, o.id))
}

func (o *ofrpOrb) GetRoleId(idx int) fmbt.RoleId { return fmbt.RoleId{RoleName: "Orb", Index: idx} }

// observe reads file and ctr off the ground truth and the rest off the
// adapter's own tracked client state.
func (o *ofrpOrb) observe() (ofrpFields, error) {
	var f ofrpFields
	st, err := orb.ReadState(o.home, o.id)
	if err != nil {
		return f, err
	}
	f.file = string(st.Status)
	cs, err := o.rt.Inspect(context.Background(), o.name())
	if err != nil {
		return f, err
	}
	f.ctr = string(cs)
	f.owner, f.restarting, f.idle, f.reapedLiveRestart = o.owner, o.restarting, o.idle, o.reapedLiveRestart
	return f, nil
}

func ofrpQualify(role string, st map[string]any) map[string]any {
	q := make(map[string]any, len(st))
	for k, v := range st {
		q[role+"."+k] = v
	}
	return q
}

// GetState is the role's state, after recording the step just taken
// into the shared trace (once per action, from whichever orb fired it).
func (o *ofrpOrb) GetState() (map[string]any, error) {
	f, err := o.observe()
	if err != nil {
		return nil, err
	}
	if o.action != "" {
		name := o.action
		if name != "Init" {
			name = "Orb#" + o.label + "." + name
		}
		*o.trace = append(*o.trace, tracecheck.Step{Action: name})
		o.action = ""
	}
	return f.state(), nil
}

// write applies edit to what is on disk now and moves updatedAt
// forward, as orb's own writeState does.
func (o *ofrpOrb) write(edit func(*orb.State)) error {
	st, err := orb.ReadState(o.home, o.id)
	if err != nil {
		return err
	}
	edit(&st)
	st.UpdatedAt = time.Now().UTC()
	if !st.UpdatedAt.After(o.wrote) {
		st.UpdatedAt = o.wrote.Add(time.Microsecond)
	}
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	if err := writeFileAtomic(filepath.Join(orb.Dir(o.home, o.id), "state.json"), b); err != nil {
		return err
	}
	o.wrote = st.UpdatedAt
	return nil
}

// --- the session's own writes ---

func (o *ofrpOrb) GoIdle() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(!f.idle && (f.file == "running" || f.file == "failed" || f.file == "starting")) {
		return nil
	}
	o.action = "GoIdle"
	st, err := orb.ReadState(o.home, o.id)
	if err != nil {
		return err
	}
	o.idleAt = st.UpdatedAt
	o.idle = true
	return nil
}

func (o *ofrpOrb) OwnerExits() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.owner && !f.restarting) {
		return nil
	}
	o.action = "OwnerExits"
	if !o.forgetOwnerExit {
		o.owner = false
	}
	if err := o.killHolder(); err != nil {
		return err
	}
	if f.ctr == "running" {
		if err := o.rt.Stop(context.Background(), o.name()); err != nil {
			return err
		}
	}
	return nil
}

func (o *ofrpOrb) OwnerCrashMidRestart() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.owner && f.restarting) {
		return nil
	}
	o.action = "OwnerCrashMidRestart"
	o.owner = false
	return o.killHolder()
}

// StartRestart is ensureRunningLocked's slow path: state.json goes to
// "starting" and the runtime brings the container up, exactly the
// order orb.go's ensureRunningLocked has them in (write, then
// rt.Start) — the incident's stuck orbs were VMs left running under
// "starting"/"building" (reaper.go's own comment on the branch this
// exercises), not ones that never came up.
func (o *ofrpOrb) StartRestart() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.ctr != "running" && !f.restarting) {
		return nil
	}
	o.action = "StartRestart"
	o.owner, o.restarting, o.idle = true, true, false
	pid, err := o.spawnHolder()
	if err != nil {
		return err
	}
	if err := o.write(func(s *orb.State) { s.PID, s.Status = pid, orb.StatusStarting }); err != nil {
		return err
	}
	return o.rt.Start(context.Background(), container.RunSpec{Name: o.name(), Image: "img"})
}

func (o *ofrpOrb) RestartOk() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.restarting && f.owner) {
		return nil
	}
	o.action = "RestartOk"
	o.restarting, o.idle = false, false
	if f.ctr != "running" {
		if err := o.rt.Start(context.Background(), container.RunSpec{Name: o.name(), Image: "img"}); err != nil {
			return err
		}
	}
	return o.write(func(s *orb.State) { s.Status = orb.StatusRunning })
}

func (o *ofrpOrb) RestartFails() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.restarting && f.owner) {
		return nil
	}
	o.action = "RestartFails"
	o.restarting, o.idle = false, false
	return o.write(func(s *orb.State) { s.Status, s.Error = orb.StatusFailed, "model: restart failed" })
}

// --- the reaper ---

func (o *ofrpOrb) tripwire(f ofrpFields) {
	if f.restarting && f.owner {
		o.reapedLiveRestart = true
	}
}

// ReapRunningOrFailed is reapIdleOrbs' running/failed branch: no
// owner/container guard at all.
func (o *ofrpOrb) ReapRunningOrFailed() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.idle && (f.file == "running" || f.file == "failed")) {
		return nil
	}
	o.action = "ReapRunningOrFailed"
	o.tripwire(f)
	if err := o.reap(); err != nil {
		return err
	}
	o.owner, o.idle = false, false
	return nil
}

// ReapOrphanedStart is reapIdleOrbs' starting/building branch: reaps
// only when the owner is dead and the container is up.
func (o *ofrpOrb) ReapOrphanedStart() error {
	f, err := o.observe()
	if err != nil {
		return err
	}
	if !o.gate.pass(f.idle && f.file == "starting" && !f.owner) {
		return nil
	}
	o.action = "ReapOrphanedStart"
	o.tripwire(f)
	if err := o.reap(); err != nil {
		return err
	}
	o.restarting = false
	return nil
}

// reap is one real reapIdleOrbs tick, timed so only this orb's own
// directory crosses the idle line: each orb has its own serve, so a
// tick started here never even lists the sibling's directory.
func (o *ofrpOrb) reap() error {
	ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
	defer cancel()
	stopped := o.api.ReapIdleOrbs(ctx, ofrpIdle, o.idleAt.Add(ofrpIdle))
	found := false
	for _, id := range stopped {
		found = found || id == o.id
	}
	if !found {
		return fmt.Errorf("reapIdleOrbs did not stop %s (idle for %s, file %q)", o.id, ofrpIdle, mustFile(o))
	}
	o.api.ForgetRunning()
	return nil
}

func mustFile(o *ofrpOrb) string {
	st, _ := orb.ReadState(o.home, o.id)
	return string(st.Status)
}

// --- the adapter (fmbt.Model) ---

type ofrpAdapter struct {
	t      *testing.T
	gate   gate
	trace  []tracecheck.Step
	traces [][]tracecheck.Step
	orb1   *ofrpOrb
	orb2   *ofrpOrb
}

func newOfrpAdapter(t *testing.T) *ofrpAdapter {
	a := &ofrpAdapter{t: t}
	a.orb1 = newOfrpOrb(t, "0", &a.gate, &a.trace)
	a.orb2 = newOfrpOrb(t, "1", &a.gate, &a.trace)
	return a
}

func (a *ofrpAdapter) Init() error {
	a.gate.reset()
	a.trace = nil
	if err := a.orb1.Init(); err != nil {
		return err
	}
	if err := a.orb2.Init(); err != nil {
		return err
	}
	// Init is recorded once, holding both roles' starting state.
	f1, err := a.orb1.observe()
	if err != nil {
		return err
	}
	f2, err := a.orb2.observe()
	if err != nil {
		return err
	}
	a.trace = append(a.trace, tracecheck.Step{Action: "Init", State: mergeState(f1.state(), f2.state())})
	a.orb1.action, a.orb2.action = "", ""
	return nil
}

func mergeState(a, b map[string]any) map[string]any {
	q := ofrpQualify("Orb#0", a)
	for k, v := range ofrpQualify("Orb#1", b) {
		q[k] = v
	}
	return q
}

func (a *ofrpAdapter) Cleanup() error {
	if len(a.trace) > 0 {
		a.traces = append(a.traces, a.trace)
		a.trace = nil
	}
	e1 := a.orb1.cleanupDir()
	e2 := a.orb2.cleanupDir()
	if e1 != nil {
		return e1
	}
	return e2
}

func (a *ofrpAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{
		{RoleName: "Orb", Index: 0}: a.orb1,
		{RoleName: "Orb", Index: 1}: a.orb2,
	}, nil
}

func (a *ofrpAdapter) GetState() (map[string]any, error) { return map[string]any{}, nil }

var ofrpActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"GoIdle":               action((*ofrpOrb).GoIdle),
	"OwnerExits":           action((*ofrpOrb).OwnerExits),
	"OwnerCrashMidRestart": action((*ofrpOrb).OwnerCrashMidRestart),
	"StartRestart":         action((*ofrpOrb).StartRestart),
	"RestartOk":            action((*ofrpOrb).RestartOk),
	"RestartFails":         action((*ofrpOrb).RestartFails),
	"ReapRunningOrFailed":  action((*ofrpOrb).ReapRunningOrFailed),
	"ReapOrphanedStart":    action((*ofrpOrb).ReapOrphanedStart),
}}

func ofrpOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 20, "max-parallel-runs": 0}
}

// ofrpDiff compares the spec's role fields with the adapter's, for
// either role's prefix.
func ofrpDiff(want map[string]any, o1, o2 ofrpFields) string {
	var diffs []string
	for k, w := range want {
		var f string
		var got map[string]any
		if s, ok := strings.CutPrefix(k, "Orb#0."); ok {
			f, got = s, o1.state()
		} else if s, ok := strings.CutPrefix(k, "Orb#1."); ok {
			f, got = s, o2.state()
		} else {
			continue // "orb1"/"orb2": the role references themselves
		}
		wj, _ := json.Marshal(w)
		gj, _ := json.Marshal(got[f])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", k, wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

// walkOfrpPaths drives both orbs down every walk the graph gives and
// returns the first step whose state is not the spec's.
func walkOfrpPaths(t *testing.T, a *ofrpAdapter, cover tracecheck.Cover) error {
	t.Helper()
	b, err := pathsJSONCover("orb_fd_exhaustion_reaper_priority", cover)
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
		t.Fatal("no walks over testdata/orb_fd_exhaustion_reaper_priority")
	}
	for pi, p := range doc.Paths {
		if err := a.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", pi, err)
		}
		err := func() error {
			for si, s := range p.Trace {
				if si > 0 {
					role, name, ok := strings.Cut(s.Action, ".")
					if !ok {
						return fmt.Errorf("step %d: unqualified action %s", si, s.Action)
					}
					f, ok := ofrpActions["Orb"][name]
					if !ok {
						return fmt.Errorf("step %d: no adapter action for %s", si, s.Action)
					}
					var target any = a.orb1
					if role == "Orb#1" {
						target = a.orb2
					}
					if _, err := f(target, nil); err != nil {
						return fmt.Errorf("step %d (%s): %w", si, s.Action, err)
					}
					if a.gate.off {
						return fmt.Errorf("step %d (%s): the adapter's view says it is not enabled", si, s.Action)
					}
				}
				f1, err := a.orb1.observe()
				if err != nil {
					return fmt.Errorf("step %d (%s): orb1: %w", si, s.Action, err)
				}
				f2, err := a.orb2.observe()
				if err != nil {
					return fmt.Errorf("step %d (%s): orb2: %w", si, s.Action, err)
				}
				a.orb1.action, a.orb2.action = "", ""
				if diff := ofrpDiff(s.State, f1, f2); diff != "" {
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

// Every settled state of the spec against a real reaper and two
// simulated, independently-served sessions (every transition under
// MODEL_COVER=transitions).
func TestOrbFdExhaustionReaperPriorityPaths(t *testing.T) {
	t.Parallel()
	a := newOfrpAdapter(t)
	if err := walkOfrpPaths(t, a, envCover()); err != nil {
		t.Fatal(err)
	}
}

// The runner's random walks, in the exhaustive run only (runMBT).
func TestOrbFdExhaustionReaperPriority(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOfrpAdapter(t)
	if err := runMBT(t, "orb_fd_exhaustion_reaper_priority", a, ofrpActions, ofrpOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The walk proves nothing unless a wiring bug that misses a live-restart
// reap fails it: here the adapter's tripwire never latches, however the
// real reaper behaves.
func TestOrbFdExhaustionReaperPriorityPathsCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOfrpAdapter(t)
	a.orb1.forgetOwnerExit, a.orb2.forgetOwnerExit = true, true
	err := walkOfrpPaths(t, a, tracecheck.CoverTransitions)
	if err == nil {
		t.Fatal("walks whose adapter never notices an owner exit passed; the walk is not checking state")
	}
	if !strings.Contains(err.Error(), "owner") {
		t.Fatalf("caught, but not on owner: %v", err)
	}
	t.Logf("caught: %v", err)
}
