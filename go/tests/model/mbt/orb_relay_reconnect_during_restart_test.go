//go:build !windows

package mbt

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
)

// specs/orb_relay_reconnect_during_restart.fizz against the real code the
// comment at the top of the spec names: internal/orb's Orb.Command
// (ensureRunningLocked's retired check, ErrReplaced) and Orb.Replace (the
// stop-retire-start swap), with the resolve-then-call, retry-once
// contract that plugins/orb/handle.go's Command applies on top of them.
//
// handle.go is not reachable from this package (its handle type is
// unexported and lives one level above internal/orb), so the adapter
// plays the handle's own algorithm directly against a real *orb.Orb:
// Resolve is handle.current() (capture the orb the handle would hand
// out and the generation it is at), Enter is the call actually reaching
// Orb.Command with that orb, and the retry-once bookkeeping
// (retriedOnce) is exactly handle.Command's `try == 0` branch. Only that
// thin wrapper is reimplemented; the retired flag, ErrReplaced and the
// swap itself are the genuine internal/orb calls, on a container.Fake.
//
// A "call" in flight is a real host subprocess (Orb.Command's argv),
// held open by a control file it polls for so the walk decides when it
// exits; a swap that lands while one is open kills it directly, the way
// a container's death would, since Fake does not do that on its own.
type orrAdapter struct {
	t    *testing.T
	home string
	slug string
	p    projectdef.Project
	gate gate

	n    int // walks so far, for a fresh session id and container each Init
	fake *container.Fake
	cur  *orb.Orb // the orb the handle would currently hand out
	gen  int      // generation a Swap bumps
	at   int      // the generation Resolve captured for the call in flight

	call        string // idle | resolved | executing | replaced_once | failed_no_retry | failed_killed | done
	retriedOnce bool

	resolvedOrb *orb.Orb // what Resolve captured, for Enter to call
	cmd         *exec.Cmd
	cmdCancel   context.CancelFunc
	waitErr     chan error

	ctl     string // holds this walk's per-call release files
	callSeq int
	relFile string

	// skipRetriedOnce is the deliberate wiring bug
	// TestOrbRelayReconnectDuringRestartCatchesWrongAdapter injects:
	// Resolve never marks a retried call, so the adapter reports a second
	// replaced_once as retry-eligible when the spec (and handle.go) allow
	// only one.
	skipRetriedOnce bool
}

// orrMaxGen mirrors the spec's MAX_GEN.
const orrMaxGen = 3

func newOrrAdapter(t *testing.T) *orrAdapter {
	t.Helper()
	home := t.TempDir()
	if _, err := projectdef.CreateEmpty(home, "orr", ""); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(home, "orr")
	if err != nil {
		t.Fatal(err)
	}
	return &orrAdapter{t: t, home: home, slug: "orr", p: p, ctl: t.TempDir()}
}

// Init opens a fresh orb, on a fresh fake runtime, for a session unique
// to this walk: a retired orb from a previous walk must never leak into
// the next one's Resolve.
func (a *orrAdapter) Init() error {
	a.n++
	session := fmt.Sprintf("w%d", a.n)
	a.fake = container.NewFake()
	o, err := orb.Open(context.Background(), a.fake, a.home, session, a.p, a.t.TempDir())
	if err != nil {
		return err
	}
	a.cur, a.gen, a.at = o, 1, 0
	a.call, a.retriedOnce = "idle", false
	a.resolvedOrb, a.cmd, a.cmdCancel, a.waitErr = nil, nil, nil, nil
	a.callSeq++
	a.relFile = filepath.Join(a.ctl, fmt.Sprintf("release-%d", a.callSeq))
	a.gate.reset()
	return nil
}

// Cleanup ends a call the walk left executing and stops the orb, so one
// walk's leftovers never touch the next.
func (a *orrAdapter) Cleanup() error {
	if a.cmd != nil {
		_ = a.cmd.Process.Kill()
		select {
		case <-a.waitErr:
		case <-time.After(5 * time.Second):
		}
		a.cmdCancel()
		a.cmd, a.cmdCancel, a.waitErr = nil, nil, nil
	}
	if a.cur != nil {
		_ = a.cur.Stop(context.Background())
	}
	return nil
}

func (a *orrAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Relay", Index: 0}: a}, nil
}

func (a *orrAdapter) GetState() (map[string]any, error) {
	return map[string]any{"gen": a.gen, "call": a.call, "at": a.at, "retriedOnce": a.retriedOnce}, nil
}

// orrCallArgv is the exec seam's argv: it polls for relFile and exits 0
// once it appears, so Finish decides when a call in flight completes.
func orrCallArgv(relFile string) []string {
	return []string{"sh", "-c", `i=0
while [ ! -f "$1" ]; do
  i=$((i+1))
  if [ $i -gt 3000 ]; then exit 3; fi
  sleep 0.01
done
exit 0`, "sh", relFile}
}

// Resolve is handle.current(): the orb the handle hands out right now,
// and the generation it is at.
func (a *orrAdapter) Resolve() error {
	if !a.gate.pass(a.call == "idle" || a.call == "replaced_once") {
		return nil
	}
	if a.call == "replaced_once" && !a.skipRetriedOnce {
		a.retriedOnce = true
	}
	a.resolvedOrb, a.at = a.cur, a.gen
	a.call = "resolved"
	return nil
}

// Enter is the resolved orb's Command actually taking its lock: retired
// (a Swap already ran since Resolve) fails with ErrReplaced, otherwise
// the exec really starts.
func (a *orrAdapter) Enter() error {
	if !a.gate.pass(a.call == "resolved") {
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*actionTimeout)
	cmd := a.resolvedOrb.Command(ctx, orrCallArgv(a.relFile)...)
	if cmd.Err != nil {
		cancel()
		if !errors.Is(cmd.Err, orb.ErrReplaced) {
			return fmt.Errorf("orb_relay: Command: %w", cmd.Err)
		}
		if a.retriedOnce {
			a.call = "failed_no_retry"
		} else {
			a.call = "replaced_once"
		}
		return nil
	}
	if err := cmd.Start(); err != nil {
		cancel()
		return fmt.Errorf("orb_relay: start the call: %w", err)
	}
	a.cmd, a.cmdCancel = cmd, cancel
	waitErr := make(chan error, 1)
	go func() { waitErr <- cmd.Wait() }()
	a.waitErr = waitErr
	a.call = "executing"
	return nil
}

// Finish releases the call in flight and requires it to exit clean: the
// product, not a kill, ended it.
func (a *orrAdapter) Finish() error {
	if !a.gate.pass(a.call == "executing") {
		return nil
	}
	if err := os.WriteFile(a.relFile, []byte("go"), 0o644); err != nil {
		return fmt.Errorf("orb_relay: release the call: %w", err)
	}
	select {
	case err := <-a.waitErr:
		if err != nil {
			return fmt.Errorf("orb_relay: the call did not exit clean: %w", err)
		}
	case <-time.After(actionTimeout):
		return fmt.Errorf("orb_relay: the call did not finish in time")
	}
	a.cmdCancel()
	a.cmd, a.cmdCancel, a.waitErr = nil, nil, nil
	a.call = "done"
	return nil
}

// Swap is a restart's Replace: stop the old container (killing a call
// executing in it, since nothing waits for a relayed call), retire it,
// and start the successor. Real internal/orb calls throughout.
func (a *orrAdapter) Swap() error {
	if !a.gate.pass(a.gen < orrMaxGen) {
		return nil
	}
	killed := a.call == "executing"
	if killed {
		_ = a.cmd.Process.Kill()
		select {
		case <-a.waitErr:
		case <-time.After(actionTimeout):
		}
		a.cmdCancel()
		a.cmd, a.cmdCancel, a.waitErr = nil, nil, nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), actionTimeout)
	defer cancel()
	next, err := a.cur.Successor(ctx, a.p)
	if err != nil {
		return fmt.Errorf("orb_relay: Successor: %w", err)
	}
	if _, err := a.cur.Replace(ctx, next, false); err != nil {
		return fmt.Errorf("orb_relay: Replace: %w", err)
	}
	a.cur, a.gen = next, a.gen+1
	if killed {
		a.call = "failed_killed"
	}
	return nil
}

// NextCall starts a later call, on a fresh release file so a lingering
// one from the last call cannot immediately end the next.
func (a *orrAdapter) NextCall() error {
	if !a.gate.pass(a.call == "done" || a.call == "failed_killed" || a.call == "failed_no_retry") {
		return nil
	}
	a.call, a.retriedOnce = "idle", false
	a.callSeq++
	a.relFile = filepath.Join(a.ctl, fmt.Sprintf("release-%d", a.callSeq))
	return nil
}

var orrActions = map[string]map[string]fmbt.ActionFunc{"Relay": {
	"Resolve":  action((*orrAdapter).Resolve),
	"Enter":    action((*orrAdapter).Enter),
	"Swap":     action((*orrAdapter).Swap),
	"Finish":   action((*orrAdapter).Finish),
	"NextCall": action((*orrAdapter).NextCall),
}}

// Every step is a real orb open, exec and restart, so a handful of walks
// already covers the graph; more give the retry-bound and kill races
// more chances to interleave.
func orrOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

func TestOrbRelayReconnectDuringRestart(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrrAdapter(t)
	if err := runMBT(t, "orb_relay_reconnect_during_restart", a, orrActions, orrOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a wrong adapter fails it: this one
// never marks a retried call, so it reports a second replaced_once as
// retry-eligible, which the spec's RetryBoundedToOnce forbids.
func TestOrbRelayReconnectDuringRestartCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrrAdapter(t)
	a.skipRetriedOnce = true
	if err := runMBT(t, "orb_relay_reconnect_during_restart", a, orrActions, orrOptions()); err == nil {
		t.Fatal("a run whose Resolve never marks retriedOnce passed; the runner is not checking state")
	}
}
