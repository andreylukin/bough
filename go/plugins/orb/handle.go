package orb

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"sync"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
)

// handle is the "orb" and "orb-state" service from the moment the row
// mounts, while the orb behind it may still be syncing repos, building
// its image or starting its container. The row used to return only once
// all of that was done, and every row after it — the ui, the web — waited
// with it: the composer, the model picker and thinking were dead for a
// whole image build. Now the row returns at once, the start runs on its
// own goroutine, and callers that need the container (tools.bash,
// write, patch) wait on Ready with their own context.
//
// A restart (restart.go) swaps the orb behind the handle and keeps the
// handle: tools and the engine resolve "orb" per call, so the next exec
// lands in the new container without re-Providing the key, which would
// reload every row that reads it (the ui among them).
type handle struct {
	home, session string
	slug          string
	ready         chan struct{} // closed once the start settled, either way
	cancel        context.CancelFunc
	rt            container.Runtime
	scratch       string
	rs            *restarter // nil for a handle built without one (tests)

	mu  sync.Mutex
	o   *iorb.Orb // nil until the start settled well
	err error     // why it did not
	// gate is non-nil while a restart swaps containers; closed when it is
	// done. An exec waits on it rather than reach a container mid-swap.
	gate chan struct{}
	// lastSwap is when the last swap that stopped a container began: a
	// job started before it died with the old container, not of its own
	// failure.
	lastSwap time.Time
	// refresh redoes the row's per-orb work (prompt section, address
	// updates) for the orb now behind the handle; set by each Apply.
	refresh func(quiet bool)
}

// errStarting is what a caller that cannot wait sees.
var errStarting = errors.New("orb still starting")

func newHandle(home, session, slug string, rt container.Runtime, scratch string, cancel context.CancelFunc) *handle {
	return &handle{home: home, session: session, slug: slug, rt: rt, scratch: scratch, ready: make(chan struct{}), cancel: cancel}
}

// beginSwap closes the exec gate for a swap. The returned end puts o
// behind the handle (nil keeps the orb it has) and opens the gate;
// stopped says whether the old container was actually stopped.
//
// lastSwap moves now, before the stop, because the jobs the stop kills
// exit while it runs and ask StoppedSince as they do. A swap that never
// got to stop anything puts it back: those jobs are still running, and
// one that later fails on its own must still report it.
func (h *handle) beginSwap() func(o *iorb.Orb, stopped bool) {
	h.mu.Lock()
	gate := make(chan struct{})
	prev := h.lastSwap
	h.gate, h.lastSwap = gate, time.Now()
	h.mu.Unlock()
	return func(o *iorb.Orb, stopped bool) {
		h.mu.Lock()
		if o != nil {
			h.o, h.err = o, nil
		}
		if !stopped {
			h.lastSwap = prev
		}
		h.gate = nil
		h.mu.Unlock()
		close(gate)
	}
}

// failed records why a restart of a failed start failed again.
func (h *handle) failed(err error) {
	h.mu.Lock()
	if h.o == nil {
		h.err = err
	}
	h.mu.Unlock()
}

// current is the orb behind the handle once no swap is under way,
// bounded by ctx. The gate and the orb are read under one lock: read
// apart, a swap beginning in between hands out the orb being replaced.
func (h *handle) current(ctx context.Context) (*iorb.Orb, error) {
	for {
		h.mu.Lock()
		gate, o := h.gate, h.o
		h.mu.Unlock()
		if gate == nil {
			return o, nil
		}
		select {
		case <-gate:
		case <-ctx.Done():
			return nil, fmt.Errorf("orb: waiting for the orb restart: %w", ctx.Err())
		}
	}
}

func (h *handle) setRefresh(f func(quiet bool)) {
	h.mu.Lock()
	h.refresh = f
	h.mu.Unlock()
}

func (h *handle) doRefresh() {
	h.mu.Lock()
	f := h.refresh
	h.mu.Unlock()
	if f != nil {
		f(true)
	}
}

// Restart asks for the orb to be rebuilt from the project's current
// definition and swapped in when no turn is running; see restart.go.
func (h *handle) Restart(fresh bool, by string) string {
	if h.rs == nil {
		return "orb: restart is not available in this session"
	}
	return h.rs.request(iorb.RestartRequest{Fresh: fresh, By: by, At: time.Now().UTC()})
}

// settle records the start's outcome and releases every waiter.
func (h *handle) settle(o *iorb.Orb, err error) {
	h.mu.Lock()
	h.o, h.err = o, err
	h.mu.Unlock()
	close(h.ready)
}

// Orb is the open orb, or nil while starting or after a failure.
func (h *handle) Orb() *iorb.Orb {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.o
}

// Ready blocks until the start settled or ctx ends. It returns the
// start's error for a failed orb, ctx's error when the caller gave up.
func (h *handle) Ready(ctx context.Context) error {
	select {
	case <-h.ready:
	case <-ctx.Done():
		return fmt.Errorf("orb: waiting for the container to start: %w", ctx.Err())
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.err
}

// Starting reports whether the start has not settled yet.
func (h *handle) Starting() bool {
	select {
	case <-h.ready:
		return false
	default:
		return true
	}
}

// Command is the exec seam tools.bash runs through: it waits for the
// container first, bounded by ctx. A start that failed yields a command
// that fails with the reason, never one that runs on the host.
//
// A swap can still begin after current returned and retire the orb it
// handed out before o.Command takes its lock; that exec fails with
// ErrReplaced, and one retry through the handle waits for the swap and
// runs in whichever container it left behind.
func (h *handle) Command(ctx context.Context, argv ...string) *exec.Cmd {
	failed := func(err error) *exec.Cmd {
		cmd := exec.CommandContext(ctx, "false")
		cmd.Err = err
		return cmd
	}
	if err := h.Ready(ctx); err != nil {
		return failed(err)
	}
	for try := 0; ; try++ {
		o, err := h.current(ctx)
		if err != nil {
			return failed(err)
		}
		if o == nil {
			// A restart of a failed start may have run meanwhile and failed.
			if err = h.Ready(ctx); err == nil {
				err = errors.New("orb: not open")
			}
			return failed(err)
		}
		cmd := o.Command(ctx, argv...)
		if try == 0 && errors.Is(cmd.Err, iorb.ErrReplaced) {
			continue
		}
		return cmd
	}
}

// Root is ~/.bough/orbs/<session>, known before anything is open.
func (h *handle) Root() string { return iorb.Dir(h.home, h.session) }

// State is the orb's state; while starting it is what the start has
// written to state.json so far, phase by phase.
//
// While a swap is under way (h.gate open) the orb behind the handle is
// o, the one BEING REPLACED: beginSwap does not move h.o to the new one
// until the swap lands or fails, so reading through o here returned it
// frozen at whatever it was when the swap began (stopped, its last
// phase, "restart pending" never cleared on the success path) for the
// whole build-container-resume.sh sequence — a caller polling /orb
// status mid-swap saw that stale snapshot instead of the swap's own
// progress. state.json is written by whichever orb is actually doing
// something (o until the stop, the new one from then on), so reading it
// instead while the gate is open tracks the real step.
func (h *handle) State() iorb.State {
	h.mu.Lock()
	gate, o := h.gate, h.o
	h.mu.Unlock()
	if gate == nil && o != nil {
		return o.State()
	}
	st, err := iorb.ReadState(h.home, h.session)
	if err == nil && st.Session != "" {
		return st
	}
	if o != nil {
		return o.State()
	}
	return iorb.State{Session: h.session, Project: h.slug, Status: iorb.StatusStarting}
}

// Line is the one-line status for the TUI's bar.
func (h *handle) Line() string { return iorb.PhaseLine(h.State(), time.Now()) }

// Redactor is the orb's, or nil (nothing to hide) before it is open.
func (h *handle) Redactor() *iorb.Redactor {
	if o := h.Orb(); o != nil {
		return o.Redactor()
	}
	return nil
}

// Redact hides the orb's secrets in s; before the orb is open there are
// none in play.
func (h *handle) Redact(s string) string {
	if o := h.Orb(); o != nil {
		return o.Redact(s)
	}
	return s
}

// StoppedSince: nothing started can have been stopped. A job started
// before a restart's swap was stopped with the old container.
func (h *handle) StoppedSince(t time.Time) bool {
	h.mu.Lock()
	swapped := !h.lastSwap.IsZero() && !h.lastSwap.Before(t)
	h.mu.Unlock()
	if swapped {
		return true
	}
	if o := h.Orb(); o != nil {
		return o.StoppedSince(t)
	}
	return false
}

// OnResume forwards to the orb once there is one; the row sets it after
// the start settles.
func (h *handle) OnResume(f func(iorb.State)) {
	if o := h.Orb(); o != nil {
		o.OnResume(f)
	}
}

// Stop stops the container; a start still in flight is cancelled instead.
func (h *handle) Stop(ctx context.Context) error {
	if h.Starting() {
		h.cancel()
		return errStarting
	}
	if o := h.Orb(); o != nil {
		return o.Stop(ctx)
	}
	return nil
}

// close is the row's unmount: cancel a start in flight and wait for it
// to let go (it is still writing state.json and worktrees under the
// session's directory), then stop an open orb.
//
// A restart asked for with --fresh that never got to swap (a headless
// run ends with its turn) removes the stopped container, so the next
// start creates a new one as asked; without fresh, the next start
// already applies the definition. Worktrees stay either way.
func (h *handle) close() {
	fresh := false
	if h.rs != nil {
		fresh = h.rs.stop()
	}
	h.cancel()
	select {
	case <-h.ready:
	case <-time.After(time.Minute):
	}
	if o := h.Orb(); o != nil {
		stopOrb(o)
		if fresh {
			name := o.State().Container
			if name == "" {
				name = container.OrbName(h.session)
			}
			ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
			defer cancel()
			if err := h.rt.Remove(ctx, name); err != nil {
				fmt.Fprintf(os.Stderr, "bough: orb: remove %s for the fresh restart: %v\n", name, err)
			}
		}
	}
}
