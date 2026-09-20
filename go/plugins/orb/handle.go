package orb

import (
	"context"
	"errors"
	"fmt"
	"os/exec"
	"sync"
	"time"

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
type handle struct {
	home, session string
	slug          string
	ready         chan struct{} // closed once the start settled, either way
	cancel        context.CancelFunc

	mu  sync.Mutex
	o   *iorb.Orb // nil until the start settled well
	err error     // why it did not
}

// errStarting is what a caller that cannot wait sees.
var errStarting = errors.New("orb still starting")

func newHandle(home, session, slug string, cancel context.CancelFunc) *handle {
	return &handle{home: home, session: session, slug: slug, ready: make(chan struct{}), cancel: cancel}
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
func (h *handle) Command(ctx context.Context, argv ...string) *exec.Cmd {
	if err := h.Ready(ctx); err != nil {
		cmd := exec.CommandContext(ctx, "false")
		cmd.Err = err
		return cmd
	}
	return h.Orb().Command(ctx, argv...)
}

// Root is ~/.bough/orbs/<session>, known before anything is open.
func (h *handle) Root() string { return iorb.Dir(h.home, h.session) }

// State is the orb's state; while starting it is what the start has
// written to state.json so far, phase by phase.
func (h *handle) State() iorb.State {
	if o := h.Orb(); o != nil {
		return o.State()
	}
	st, err := iorb.ReadState(h.home, h.session)
	if err != nil || st.Session == "" {
		return iorb.State{Session: h.session, Project: h.slug, Status: iorb.StatusStarting}
	}
	return st
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

// StoppedSince: nothing started can have been stopped.
func (h *handle) StoppedSince(t time.Time) bool {
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
func (h *handle) close() {
	h.cancel()
	select {
	case <-h.ready:
	case <-time.After(time.Minute):
	}
	if o := h.Orb(); o != nil {
		stopOrb(o)
	}
}
