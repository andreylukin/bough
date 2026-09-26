package orb

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_status_stale_during_restart.fizz: what h.State() (the field
// /orb status, GET .../orb and the "orb" prompt section all read) shows
// while restart.go's swap runs on its own goroutine (RequestRestart ->
// [BuildFail | BuildDone -> SwapBegin -> [SwapFail | SwapDone]]).
//
// The recipe (go/tests/model/README.md) puts a flow's Go MBT test under
// tests/model/mbt, driving a real serve. This flow's subject — h.State,
// h.gate, the restarter — is unexported inside this package, reached in
// every other flow through a service key on a live kernel row; a restart
// swaps that key's value (h.Orb()) on its own goroutine well after the
// swap starts, which is exactly the staleness this spec is about, so a
// caller outside plugins/orb cannot observe the bug at all: it would see
// only the fully-swapped-in orb, or the fully-frozen one, never the
// steps between. So the test lives beside handle.go and restart.go
// instead, driving them the way plugins/orb's own restart_test.go does
// (a handle + restarter over a container.Fake with holds), which is the
// real code the fizz spec's comment names. The exhaustive fmbt random-run
// harness (tests/model/mbt/harness_test.go) is private to that package
// and skips outside MODEL_COVER=transitions in every other flow too; the
// deterministic path walk below, over the same checked-in graph, is what
// a normal `go test` actually runs for any flow and gives full transition
// coverage on its own.

// ossaRuntime is container.Fake with two calls held until the walk
// answers them: the Commit of a changed setup.sh (BuildFail/BuildDone,
// the spec's "build") and the Start of the container the swap creates
// or reuses (SwapBegin, the spec's "container" -> "resume.sh" edge).
// Nothing else holds: the session's own first start (Init) runs on an
// unarmed runtime, straight through.
type ossaRuntime struct {
	*container.Fake
	mu     sync.Mutex
	prefix string // "bough-orb/<slug>:" once a build is armed
	ctr    string // the container name once a start hold is armed
	commit chan error
	start  chan error
}

func (r *ossaRuntime) armBuild(slug string) {
	r.mu.Lock()
	r.prefix = "bough-orb/" + slug + ":"
	r.mu.Unlock()
}

func (r *ossaRuntime) armStart(name string) {
	r.mu.Lock()
	r.ctr = name
	r.mu.Unlock()
}

func (r *ossaRuntime) Commit(ctx context.Context, spec container.CommitSpec, log io.Writer) error {
	r.mu.Lock()
	if r.prefix == "" || !strings.HasPrefix(spec.Tag, r.prefix) {
		r.mu.Unlock()
		return r.Fake.Commit(ctx, spec, log)
	}
	r.prefix = ""
	ch := make(chan error)
	r.commit = ch
	r.mu.Unlock()
	if err := <-ch; err != nil {
		fmt.Fprintf(log, "fake: build of %s: %v\n", spec.Tag, err)
		return err
	}
	return r.Fake.Commit(ctx, spec, log)
}

func (r *ossaRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	r.mu.Lock()
	if r.ctr == "" || spec.Name != r.ctr {
		r.mu.Unlock()
		return r.Fake.Start(ctx, spec)
	}
	r.ctr = ""
	ch := make(chan error)
	r.start = ch
	r.mu.Unlock()
	if err := <-ch; err != nil {
		return err
	}
	return r.Fake.Start(ctx, spec)
}

// held is which of the two calls above is parked, "" for neither.
func (r *ossaRuntime) held() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	switch {
	case r.commit != nil:
		return "build"
	case r.start != nil:
		return "start"
	}
	return ""
}

func (r *ossaRuntime) release(which string, err error) bool {
	r.mu.Lock()
	var ch chan error
	switch which {
	case "build":
		ch, r.commit = r.commit, nil
	case "start":
		ch, r.start = r.start, nil
	}
	r.mu.Unlock()
	if ch == nil {
		return false
	}
	ch <- err
	return true
}

// ossaResume is the project's resume.sh: it waits for the walk to write
// its exit code into <ctl>/<session>.resume, and gives up after a
// minute so a run the test abandoned mid-swap cannot hang forever. The
// path is baked in at project-creation time (one adapter, one ctl dir),
// not read from the environment: os.Setenv is process-global and this
// package's other tests run in parallel.
func ossaResume(ctl string) string {
	return fmt.Sprintf(`#!/bin/sh
d="%s/$BOUGH_SESSION"
: > "$d.waiting"
i=0
while [ ! -f "$d.resume" ]; do
  i=$((i+1))
  if [ $i -gt 300 ]; then rm -f "$d.waiting"; exit 3; fi
  sleep 0.01
done
code=$(cat "$d.resume")
rm -f "$d.resume" "$d.waiting"
exit "$code"
`, ctl)
}

// ossaAdapter plays /orb status against one handle+restarter per
// session, all sharing one container.Fake with holds. It is the model's
// Orb role: RequestRestart edits setup.sh (so the successor's build is
// real, not a no-op EnsureImage hit) and asks h.Restart; the rest just
// answers the held calls in order.
type ossaAdapter struct {
	t         *testing.T
	home, ctl string
	rt        *ossaRuntime

	n             int
	slug, session string
	h             *handle

	gateOff bool // the walk's first disabled action turned this on

	// swapFailAsOK is the wrong-adapter test's bug: it tells SwapFail to
	// let resume.sh succeed, so the real state never fails.
	swapFailAsOK bool
}

func newOssaAdapter(t *testing.T) *ossaAdapter {
	root := t.TempDir()
	a := &ossaAdapter{t: t, home: filepath.Join(root, "home"), ctl: filepath.Join(root, "ctl")}
	for _, d := range []string{a.home, a.ctl} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	a.rt = &ossaRuntime{Fake: container.NewFake()}
	a.rt.AddImage(projectdef.BaseTag())
	// A walk that fails partway can leave a hold parked: let it go so
	// h.close (t.Cleanup, registered per session in Init) does not wait
	// forever on a goroutine nothing will ever release.
	t.Cleanup(func() {
		a.rt.release("build", errors.New("test over"))
		a.rt.release("start", errors.New("test over"))
	})
	return a
}

func (a *ossaAdapter) pass(enabled bool) bool {
	if !enabled {
		a.gateOff = true
	}
	return !a.gateOff
}

// Init opens a fresh session's orb on the shared runtime: no restart in
// flight, resume.sh released at once so the open does not wait on
// anything.
func (a *ossaAdapter) Init() error {
	a.n++
	a.slug = fmt.Sprintf("ossa%04d", a.n)
	a.session = a.slug
	a.gateOff = false
	if _, err := projectdef.CreateEmpty(a.home, a.slug, ""); err != nil {
		return err
	}
	if err := projectdef.WriteFile(a.home, a.slug, projectdef.FileYAML, "repos: []\n"); err != nil {
		return err
	}
	if err := projectdef.WriteFile(a.home, a.slug, projectdef.FileSetup, "# v0\n"); err != nil {
		return err
	}
	if err := projectdef.WriteFile(a.home, a.slug, projectdef.FileResume, ossaResume(a.ctl)); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(a.ctl, a.session+".resume"), []byte("0"), 0o644); err != nil {
		return err
	}
	p, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return err
	}
	scratch := a.t.TempDir()
	ctx, cancel := context.WithCancel(context.Background())
	o, err := iorb.Open(ctx, a.rt, a.home, a.session, p, scratch)
	if err != nil {
		cancel()
		return err
	}
	a.h = newHandle(a.home, a.session, a.slug, a.rt, scratch, cancel)
	a.h.settle(o, nil)
	kctx := kernel.NewContext()
	a.h.rs = newRestarter(a.h, kctx)
	a.h.rs.poll, a.h.rs.settle = 5*time.Millisecond, 5*time.Millisecond
	a.h.rs.prep = func(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratch string) (*iorb.Orb, error) {
		return iorb.Prepare(ctx, rt, home, session, p, scratch)
	}
	a.h.rs.setNotify(func(string) {})
	a.h.rs.start()
	a.t.Cleanup(a.h.close)
	return nil
}

// ossaState is what GetState derives from h.State(): the spec's own
// vocabulary, read off the real fields h.State() (h.Orb().State() once
// no swap is in flight, state.json's while one is) reports.
func ossaState(st iorb.State) (status, phase string, restarting bool) {
	phase = "ready"
	switch {
	case st.Restart != "":
		phase = "build"
	case st.Phase == iorb.PhaseContainer:
		phase = "container"
	case st.Phase == iorb.PhaseResume:
		phase = "resume.sh"
	}
	switch {
	// Restart != "" wins even over a Status a previous, failed cycle
	// left behind: SetRestart merges only that one field, so a restart
	// requested on a failed orb reads {Status: failed, Restart:
	// "building"} until the new cycle's own write replaces Status.
	case st.Restart != "":
		status = "building"
	case st.Status == iorb.StatusFailed:
		status = "failed"
	case phase != "ready":
		status = "building"
	default:
		status = "running"
	}
	return status, phase, status == "building"
}

func (a *ossaAdapter) GetState() (map[string]any, error) {
	status, phase, restarting := ossaState(a.h.State())
	return map[string]any{
		"Orb#0.status": status, "Orb#0.phase": phase, "Orb#0.restarting": restarting,
	}, nil
}

func (a *ossaAdapter) waiting() string { return filepath.Join(a.ctl, a.session+".waiting") }
func (a *ossaAdapter) resuming() bool {
	_, err := os.Stat(a.waiting())
	return err == nil
}

// settleUntil polls GetState (through h.State(), the real staleness
// point) until ok, or fails the step: a product regression here is a
// timeout, not a wrong value, because h.State() simply stops moving.
func (a *ossaAdapter) settleUntil(what string, ok func(status, phase string, restarting bool) bool) error {
	deadline := time.Now().Add(5 * time.Second)
	for {
		status, phase, restarting := ossaState(a.h.State())
		if ok(status, phase, restarting) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s: stuck at status=%s phase=%s restarting=%v", what, status, phase, restarting)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// settleAny polls a runtime-level condition (which hold is parked),
// not h.State(): used only where h.State() itself would not move until
// the condition is already true (RequestRestart's SetRestart lands
// before the successor's build ever reaches Commit).
func (a *ossaAdapter) settleAny(what string, ok func() bool) error {
	deadline := time.Now().Add(5 * time.Second)
	for !ok() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for %s", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
	return nil
}

func (a *ossaAdapter) RequestRestart() error {
	status, _, _ := ossaState(a.h.State())
	if !a.pass(status == "running" || status == "failed") {
		return nil
	}
	// A restart of the unchanged definition would find its own image
	// already built and skip Commit entirely (EnsureImage's early
	// return): edit setup.sh so the successor's build is real.
	b, err := projectdef.ReadFile(a.home, a.slug, projectdef.FileSetup)
	if err != nil {
		return err
	}
	if err := projectdef.WriteFile(a.home, a.slug, projectdef.FileSetup, string(b)+"echo v\n"); err != nil {
		return err
	}
	a.rt.armBuild(a.slug)
	if out := a.h.Restart(false, "test"); !strings.Contains(out, "scheduled") {
		return fmt.Errorf("restart = %q", out)
	}
	return a.settleAny("the build to start", func() bool { return a.rt.held() == "build" })
}

func (a *ossaAdapter) BuildFail() error {
	if !a.pass(a.rt.held() == "build") {
		return nil
	}
	a.rt.release("build", errors.New("fake: setup.sh exited 1"))
	return a.settleUntil("the failed build to clear", func(_, phase string, _ bool) bool { return phase == "ready" })
}

func (a *ossaAdapter) BuildDone() error {
	if !a.pass(a.rt.held() == "build") {
		return nil
	}
	a.rt.armStart(container.OrbName(a.session))
	a.rt.release("build", nil)
	return a.settleUntil("the swap to reach the container", func(_, phase string, _ bool) bool { return phase == "container" })
}

func (a *ossaAdapter) SwapBegin() error {
	if !a.pass(a.rt.held() == "start") {
		return nil
	}
	a.rt.release("start", nil)
	deadline := time.Now().Add(5 * time.Second)
	for !a.resuming() {
		if time.Now().After(deadline) {
			return fmt.Errorf("resume.sh did not start")
		}
		time.Sleep(5 * time.Millisecond)
	}
	return a.settleUntil("the swap to reach resume.sh", func(_, phase string, _ bool) bool { return phase == "resume.sh" })
}

func (a *ossaAdapter) endResume(code string) error {
	if err := os.WriteFile(filepath.Join(a.ctl, a.session+".resume"), []byte(code), 0o644); err != nil {
		return err
	}
	return nil
}

func (a *ossaAdapter) SwapFail() error {
	if !a.pass(a.resuming()) {
		return nil
	}
	code := "3"
	if a.swapFailAsOK {
		code = "0"
	}
	if err := a.endResume(code); err != nil {
		return err
	}
	return a.settleUntil("the swap to fail", func(status, _ string, _ bool) bool { return status == "failed" })
}

func (a *ossaAdapter) SwapDone() error {
	if !a.pass(a.resuming()) {
		return nil
	}
	if err := a.endResume("0"); err != nil {
		return err
	}
	return a.settleUntil("the swap to finish", func(status, phase string, _ bool) bool { return status == "running" && phase == "ready" })
}

var ossaActions = map[string]func(*ossaAdapter) error{
	"RequestRestart": (*ossaAdapter).RequestRestart,
	"BuildFail":      (*ossaAdapter).BuildFail,
	"BuildDone":      (*ossaAdapter).BuildDone,
	"SwapBegin":      (*ossaAdapter).SwapBegin,
	"SwapFail":       (*ossaAdapter).SwapFail,
	"SwapDone":       (*ossaAdapter).SwapDone,
}

// ossaTestdata is go/tests/model/testdata/orb_status_stale_during_restart,
// the graph scripts/model-test.sh gen writes from the checked-in spec.
func ossaTestdata() string {
	_, file, _, _ := runtime.Caller(0)
	return filepath.Join(filepath.Dir(file), "..", "..", "tests", "model", "testdata", "orb_status_stale_during_restart")
}

func ossaGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(ossaTestdata())
	if err != nil {
		t.Fatalf("%v (run: scripts/model-test.sh gen orb_status_stale_during_restart)", err)
	}
	return g
}

// stateDiff compares the spec's fields with the adapter's, through JSON
// so bool/string compare exactly.
func ossaStateDiff(want, got map[string]any) string {
	var diffs []string
	for k, w := range want {
		if !strings.HasPrefix(k, "Orb#0.") {
			continue // "orb": the role reference itself
		}
		wj, _ := json.Marshal(w)
		gj, _ := json.Marshal(got[k])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, adapter %s", k, wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

// walkOssaPaths drives the adapter down every generated path (every
// transition of the graph, CoverStates' default walks already reach
// every state) and returns the first step whose state disagrees.
func walkOssaPaths(t *testing.T, a *ossaAdapter) error {
	t.Helper()
	g := ossaGraph(t)
	// A walk that returns early (a mismatch, a disabled step) can leave a
	// hold parked: release it before the next Init, or that session's
	// h.close (its own t.Cleanup) would wait forever on a goroutine
	// nothing will ever answer.
	drain := func() {
		a.rt.release("build", errors.New("walk over"))
		a.rt.release("start", errors.New("walk over"))
	}
	defer drain()
	for wi, w := range g.Walks(tracecheck.CoverTransitions, 0) {
		drain()
		if err := a.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", wi, err)
		}
		for si, s := range w.Trace {
			if si > 0 {
				name := strings.TrimPrefix(s.Action, "Orb#0.")
				fn, ok := ossaActions[name]
				if !ok {
					return fmt.Errorf("walk %d step %d: no action %q", wi, si, s.Action)
				}
				if err := fn(a); err != nil {
					return fmt.Errorf("walk %d step %d (%s): %w", wi, si, s.Action, err)
				}
				if a.gateOff {
					status, phase, restarting := ossaState(a.h.State())
					return fmt.Errorf("walk %d step %d (%s): the adapter's view says it is not enabled (held=%q status=%s phase=%s restarting=%v)", wi, si, s.Action, a.rt.held(), status, phase, restarting)
				}
			}
			got, err := a.GetState()
			if err != nil {
				return fmt.Errorf("walk %d step %d (%s): %w", wi, si, s.Action, err)
			}
			if diff := ossaStateDiff(s.State, got); diff != "" {
				return fmt.Errorf("walk %d step %d (%s): %s", wi, si, s.Action, diff)
			}
		}
	}
	return nil
}

// TestOrbStatusStaleDuringRestartPaths walks every transition of
// specs/orb_status_stale_during_restart.fizz against a real handle and
// restarter: h.State() must track the build -> container -> resume.sh
// sequence a swap actually runs, at every step, not just before it
// starts and after it lands.
func TestOrbStatusStaleDuringRestartPaths(t *testing.T) {
	a := newOssaAdapter(t)
	if err := walkOssaPaths(t, a); err != nil {
		t.Fatal(err)
	}
}

// The run above proves nothing unless a wrong adapter fails it: here
// SwapFail tells resume.sh to succeed, so the real orb never fails, and
// the very state SwapFail's own step checks (status="failed") disagrees.
func TestOrbStatusStaleDuringRestartPathsCatchesWrongAdapter(t *testing.T) {
	a := newOssaAdapter(t)
	a.swapFailAsOK = true
	if err := walkOssaPaths(t, a); err == nil {
		t.Fatal("a walk whose SwapFail succeeds passed; the walk is not checking state")
	}
}
