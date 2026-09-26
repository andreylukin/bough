package orb

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
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

// oesRuntime holds the swap's Start until the walk says CloseTurn or
// ApplySwap: waitIdle already gates it on the turn being closed (real
// code), and this second gate is what lets the walk observe restartPending
// staying true, and the container's env staying at its old value, for as
// many steps as it likes before choosing to let the swap land — the point
// of the RequestRestart/CloseTurn/ApplySwap split in the spec. Nothing
// else on the fake is held: EnsureImage, the successor's build (which
// bakes the host env this restart captured) and the old container's own
// execs all run straight through.
type oesRuntime struct {
	*container.Fake
	mu   sync.Mutex
	ctr  string // the container name a Start for it should hold, once armed
	hold chan error
}

func (r *oesRuntime) arm(name string) {
	r.mu.Lock()
	r.ctr = name
	r.mu.Unlock()
}

func (r *oesRuntime) Start(ctx context.Context, spec container.RunSpec) error {
	r.mu.Lock()
	if r.ctr == "" || spec.Name != r.ctr {
		r.mu.Unlock()
		return r.Fake.Start(ctx, spec)
	}
	r.ctr = ""
	ch := make(chan error)
	r.hold = ch
	r.mu.Unlock()
	select {
	case err := <-ch:
		if err != nil {
			return err
		}
	case <-ctx.Done():
		return ctx.Err()
	}
	return r.Fake.Start(ctx, spec)
}

func (r *oesRuntime) held() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.hold != nil
}

func (r *oesRuntime) release(err error) bool {
	r.mu.Lock()
	ch := r.hold
	r.hold = nil
	r.mu.Unlock()
	if ch == nil {
		return false
	}
	ch <- err
	return true
}

// specs/orb_env_secret_rotation_vs_running_session.fizz against the real
// code the spec's comment names: project.yml's env:, baked into the
// container's run spec by internal/orb's baseEnv/Successor.build, and
// restart.go's request()/once() (the build kicks the instant a restart
// is asked; only the swap itself waits for the turn to close).
//
// The recipe (go/tests/model/README.md) puts a flow's Go MBT test under
// tests/model/mbt, driving a real serve. This flow's subject — the
// restarter's pending/busy bookkeeping — is unexported inside this
// package, reached from outside it only through a service key on a live
// kernel row that a caller cannot drive at this granularity (there is no
// API to hold a restart between its build and its swap). So, like
// orb_status_stale_during_restart before it, this test lives beside
// handle.go and restart.go instead, over a real handle + restarter on a
// container.Fake, the way plugins/orb's own restart_test.go does. The
// deterministic path walk below drives every transition of the checked-in
// graph, the same full-coverage substitute orb_status_stale_during_restart
// uses for the fmbt harness (tests/model/mbt/harness_test.go), which is
// private to that package.
//
// injectedSecret is read off the real running container: a command run
// through the handle (Orb.Command -> execEnv -> envList(project.Def.Env))
// prints $SECRET, so a wrong capture point (e.g. reading the env at swap
// time instead of at request time) would show up as a real mismatch, not
// a mirrored bookkeeping value.

// oesFields is what GetState reports, all derived from real state: the
// spec's Orb role.
type oesEnv struct {
	t                        *testing.T
	home, slug, session, ctl string
	rt                       *oesRuntime
	h                        *handle

	n int // walks so far, for a fresh session id each Init

	hostSecret    int // this walk's view of the host file; RotateSecret writes it
	pendingSecret int // RequestRestart's snapshot of hostSecret, applied at swap
	lastInjected  int // injectedSecret's cache while the exec gate is closed

	// restartPending mirrors the spec's own field: true from a successful
	// RequestRestart until the swap it started actually lands (e.h.Orb()
	// changes). h.State().Restart is not it — the successor's own run()
	// writes state.json with a blank Restart the moment it starts
	// (internal/orb's run(), before our held Start ever returns), which
	// is exactly the staleness orb_status_stale_during_restart is about,
	// not what this flow means by "still pending".
	restartPending bool

	gateOff bool

	// mirrorBug is TestOrbEnvSecretRotationVsRunningSessionPathsCatchesWrongAdapter's
	// deliberate wiring bug: report the host's current value as if it were
	// always already injected, instead of reading the real container.
	mirrorBug bool
}

func newOesEnv(t *testing.T) *oesEnv {
	root := t.TempDir()
	e := &oesEnv{t: t, home: filepath.Join(root, "home"), ctl: filepath.Join(root, "ctl")}
	for _, d := range []string{e.home, e.ctl} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	return e
}

// closeHandleUnblocking runs h.close in the background while releasing
// any oesRuntime hold left on rt: a walk that ends mid-restart (its
// trace stopped before CloseTurn/ApplySwap released it) leaves the
// container Start held, which would otherwise block h.close's wait for
// its restarter to stop forever. rt is this walk's own runtime (a fresh
// one per Init, not shared across walks: a single hold field can only
// remember one pending Start, and two walks sharing it would let a
// later walk's arm silently orphan an earlier walk's still-held one).
func closeHandleUnblocking(h *handle, rt *oesRuntime) {
	done := make(chan struct{})
	go func() { h.close(); close(done) }()
	deadline := time.Now().Add(10 * time.Second)
	for {
		select {
		case <-done:
			return
		default:
		}
		rt.release(fmt.Errorf("test over"))
		if time.Now().After(deadline) {
			<-done
			return
		}
		time.Sleep(2 * time.Millisecond)
	}
}

func (e *oesEnv) pass(enabled bool) bool {
	if !enabled {
		e.gateOff = true
	}
	return !e.gateOff
}

// writeSecret is a RotateSecret's write to project.yml: the "credential"
// that lands in the host env, standing for a keychain-backed secret
// rotating in place.
func (e *oesEnv) writeSecret(t *testing.T, n int) {
	t.Helper()
	if err := projectdef.WriteFile(e.home, e.slug, projectdef.FileYAML,
		fmt.Sprintf("repos: []\nenv:\n  SECRET: %q\n", strconv.Itoa(n))); err != nil {
		t.Fatal(err)
	}
}

// Init opens a fresh session's orb on a fresh fake runtime, at
// hostSecret 0, no restart in flight, no turn open. Each walk gets its
// own runtime (not one shared across the whole test): see
// closeHandleUnblocking for why sharing one is unsafe.
func (e *oesEnv) Init() error {
	e.n++
	e.slug = fmt.Sprintf("oes%04d", e.n)
	e.session = e.slug
	e.gateOff = false
	e.hostSecret, e.pendingSecret, e.lastInjected = 0, 0, 0
	e.restartPending = false
	e.rt = &oesRuntime{Fake: container.NewFake()}
	if _, err := projectdef.CreateEmpty(e.home, e.slug, ""); err != nil {
		return err
	}
	e.writeSecret(e.t, 0)
	if err := projectdef.WriteFile(e.home, e.slug, projectdef.FileResume, "#!/bin/sh\necho resumed-ok\n"); err != nil {
		return err
	}
	p, err := projectdef.Load(e.home, e.slug)
	if err != nil {
		return err
	}
	scratch := e.t.TempDir()
	ctx, cancel := context.WithCancel(context.Background())
	o, err := iorb.Open(ctx, e.rt, e.home, e.session, p, scratch)
	if err != nil {
		cancel()
		return err
	}
	e.h = newHandle(e.home, e.session, e.slug, e.rt, scratch, cancel)
	e.h.settle(o, nil)
	kctx := kernel.NewContext()
	e.h.rs = newRestarter(e.h, kctx)
	e.h.rs.poll, e.h.rs.settle = 5*time.Millisecond, 5*time.Millisecond
	e.h.rs.prep = func(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratch string) (*iorb.Orb, error) {
		return iorb.Prepare(ctx, rt, home, session, p, scratch)
	}
	e.h.rs.setNotify(func(string) {})
	e.h.rs.start()
	h, rt := e.h, e.rt
	e.t.Cleanup(func() { closeHandleUnblocking(h, rt) })
	e.rt.arm(container.OrbName(e.session))
	return nil
}

// injectedSecret runs a real command through the handle and reads the
// SECRET the running container's exec env actually carries: the product's
// own execEnv, not a mirrored bookkeeping value.
//
// h.Command blocks on the swap's exec gate (handle.go's current) while a
// swap is under way; our own hold (oesRuntime) is what is holding that
// swap open. But the old container the gate hands out until then can
// itself be between "stopped" and "the new one is up" for the instant
// restart.go's own stopLocked/Remove runs, ahead of the gate closing on
// this goroutine's next scheduling — a real, narrow window, not a
// mirrored value. Either way — the gate blocking outright, or the old
// container reporting not-running in that narrow window — nothing has
// actually landed yet, so the cached value from the last successful read
// is still exactly right; only a real value ever updates it.
func (e *oesEnv) injectedSecret() (int, error) {
	deadline := time.Now().Add(150 * time.Millisecond)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
		out, err := e.h.Command(ctx, "sh", "-c", "echo $SECRET").Output()
		cancel()
		if err != nil {
			if time.Now().After(deadline) {
				return e.lastInjected, nil
			}
			time.Sleep(5 * time.Millisecond)
			continue
		}
		v := strings.TrimSpace(string(out))
		if v == "" {
			e.lastInjected = 0
			return 0, nil
		}
		n, err := strconv.Atoi(v)
		if err != nil {
			return 0, fmt.Errorf("SECRET=%q: %w", v, err)
		}
		e.lastInjected = n
		return n, nil
	}
}

func (e *oesEnv) GetState() (map[string]any, error) {
	injected, err := e.injectedSecret()
	if err != nil {
		return nil, err
	}
	if e.mirrorBug {
		injected = e.hostSecret
	}
	return map[string]any{
		"Orb#0.hostSecret":     e.hostSecret,
		"Orb#0.injectedSecret": injected,
		"Orb#0.restartPending": e.restartPending,
		"Orb#0.turnOpen":       e.h.rs.busy,
		"Orb#0.pendingSecret":  e.pendingSecret,
	}, nil
}

func (e *oesEnv) RotateSecret() error {
	if !e.pass(e.hostSecret < 2) {
		return nil
	}
	e.hostSecret++
	e.writeSecret(e.t, e.hostSecret)
	return nil
}

func (e *oesEnv) RequestRestart() error {
	if !e.pass(!e.restartPending) {
		return nil
	}
	e.pendingSecret = e.hostSecret
	out := e.h.Restart(false, "test")
	if !strings.Contains(out, "scheduled") {
		return fmt.Errorf("restart = %q", out)
	}
	e.restartPending = true
	return nil
}

func (e *oesEnv) OpenTurn() error {
	if !e.pass(!e.h.rs.busy) {
		return nil
	}
	e.h.rs.observe("call")
	return nil
}

// closeTurnLike is CloseTurn and ApplySwap's shared wait: once the turn is
// not open (real or already), the swap already in flight for this
// restart is parked at our held container Start (waitIdle, the real
// gate, already let it get this far). Releasing it lets restart.go's own
// Replace finish, exactly as restart_test.go's waitFor(t, "the swap",
// e.h.Orb() != old) does.
func (e *oesEnv) closeTurnLike() error {
	old := e.h.Orb()
	deadline := time.Now().Add(5 * time.Second)
	for !e.rt.held() {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for the swap to reach the held container start")
		}
		time.Sleep(5 * time.Millisecond)
	}
	e.rt.release(nil)
	e.rt.arm(container.OrbName(e.session)) // ready for a later restart in this walk
	for e.h.Orb() == old {
		if time.Now().After(deadline) {
			return fmt.Errorf("waiting for the swap to land")
		}
		time.Sleep(5 * time.Millisecond)
	}
	e.restartPending = false
	return nil
}

func (e *oesEnv) CloseTurn() error {
	if !e.pass(e.h.rs.busy) {
		return nil
	}
	pending := e.restartPending
	e.h.rs.observe("done")
	if !pending {
		return nil
	}
	return e.closeTurnLike()
}

func (e *oesEnv) ApplySwap() error {
	if !e.pass(e.restartPending && !e.h.rs.busy) {
		return nil
	}
	return e.closeTurnLike()
}

var oesActions = map[string]func(*oesEnv) error{
	"RotateSecret":   (*oesEnv).RotateSecret,
	"RequestRestart": (*oesEnv).RequestRestart,
	"OpenTurn":       (*oesEnv).OpenTurn,
	"CloseTurn":      (*oesEnv).CloseTurn,
	"ApplySwap":      (*oesEnv).ApplySwap,
}

// oesTestdata is go/tests/model/testdata/orb_env_secret_rotation_vs_running_session,
// the graph scripts/model-test.sh gen writes from the checked-in spec.
func oesTestdata() string {
	_, file, _, _ := runtime.Caller(0)
	return filepath.Join(filepath.Dir(file), "..", "..", "tests", "model", "testdata", "orb_env_secret_rotation_vs_running_session")
}

func oesGraph(t *testing.T) *tracecheck.Graph {
	t.Helper()
	g, err := tracecheck.Load(oesTestdata())
	if err != nil {
		t.Fatalf("%v (run: scripts/model-test.sh gen orb_env_secret_rotation_vs_running_session)", err)
	}
	return g
}

func oesStateDiff(want, got map[string]any) string {
	var diffs []string
	for k, w := range want {
		if !strings.HasPrefix(k, "Orb#0.") {
			continue
		}
		// The graph encodes ints as float64 (JSON); the adapter's are int.
		var wv, gv any = w, got[k]
		if wf, ok := w.(float64); ok {
			wv = int(wf)
		}
		if wv != gv {
			diffs = append(diffs, fmt.Sprintf("%s: spec %v, adapter %v", k, w, got[k]))
		}
	}
	return strings.Join(diffs, "; ")
}

func walkOesPaths(t *testing.T, e *oesEnv) error {
	t.Helper()
	g := oesGraph(t)
	for wi, w := range g.Walks(tracecheck.CoverTransitions, 0) {
		if err := e.Init(); err != nil {
			return fmt.Errorf("walk %d: Init: %w", wi, err)
		}
		for si, s := range w.Trace {
			if si > 0 {
				name := strings.TrimPrefix(s.Action, "Orb#0.")
				fn, ok := oesActions[name]
				if !ok {
					return fmt.Errorf("walk %d step %d: no action %q", wi, si, s.Action)
				}
				if err := fn(e); err != nil {
					return fmt.Errorf("walk %d step %d (%s): %w", wi, si, s.Action, err)
				}
				if e.gateOff {
					return fmt.Errorf("walk %d step %d (%s): the adapter's view says it is not enabled", wi, si, s.Action)
				}
			}
			got, err := e.GetState()
			if err != nil {
				return fmt.Errorf("walk %d step %d (%s): %w", wi, si, s.Action, err)
			}
			if diff := oesStateDiff(s.State, got); diff != "" {
				return fmt.Errorf("walk %d step %d (%s): %s", wi, si, s.Action, diff)
			}
		}
	}
	return nil
}

// TestOrbEnvSecretRotationVsRunningSessionPaths walks every transition of
// specs/orb_env_secret_rotation_vs_running_session.fizz against a real
// handle and restarter: a running container's env never moves ahead of
// the host, and a swap only ever applies the host value the restart
// request captured, not a rotation that lands after it.
func TestOrbEnvSecretRotationVsRunningSessionPaths(t *testing.T) {
	e := newOesEnv(t)
	if err := walkOesPaths(t, e); err != nil {
		t.Fatal(err)
	}
}

// The run above proves nothing unless a wrong adapter fails it: here
// GetState reports the host's current value as already injected, which
// diverges from the spec the moment a rotation lands on a running
// container before its restart's swap.
func TestOrbEnvSecretRotationVsRunningSessionPathsCatchesWrongAdapter(t *testing.T) {
	e := newOesEnv(t)
	e.mirrorBug = true
	if err := walkOesPaths(t, e); err == nil {
		t.Fatal("a walk whose GetState mirrors the host value passed; the walk is not checking state")
	}
}
