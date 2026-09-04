package loop

// Turn cancellation: the "cancel" service (func()) cancels the turn in
// flight. The runner sees ctx.Err() after the pending LLM call (or
// code run) unwinds, records a "cancelled" entry (model-invisible
// under DefaultProject) and ends the turn with the usual "done".
//
// Steering: the "steer" service (func(text string) bool) hands the
// turn in flight a message without ending it. It never interrupts a
// block mid-execution; the runner lands pending steers at the next
// boundary (between blocks, before each LLM call, and once more
// before the turn's done — a steer sent during the final reply is
// asked about inside the same turn), drops the rest of the current
// reply, and asks the model again with the steer in context. False
// when no turn is running, or once the running turn has taken its
// last boundary (the caller queues it as ordinary input instead:
// nothing is ever stranded, and a turn never runs on a steer's
// behalf after its done).

import (
	"context"
	"sync"
)

// turns hands each input its own cancellable context and remembers
// the live one's cancel func for the "cancel" service, plus the
// steering messages sent since the runner last looked.
type turns struct {
	mu       sync.Mutex
	cancel   context.CancelFunc
	steering bool // a turn runs and has a boundary left for a steer
	steers   []string
	// pending is a cancel that arrived before the turn it belongs to
	// installed its own: see Cancel.
	pending bool
}

// Steer queues text for the turn in flight; false while idle or past
// the turn's last boundary.
func (t *turns) Steer(text string) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.steering {
		return false
	}
	t.steers = append(t.steers, text)
	return true
}

// takeSteers hands the runner every steer sent since the last take.
// A final take shuts the gate in the same instant (the runner is
// about to end the turn), a later non-final one reopens it (the turn
// went on after all).
func (t *turns) takeSteers(final bool) []string {
	t.mu.Lock()
	defer t.mu.Unlock()
	s := t.steers
	t.steers = nil
	t.steering = !final
	return s
}

// Cancel aborts the turn in flight. A cancel that arrives before the
// turn has installed its context is REMEMBERED, not dropped: run
// honours it the moment it starts.
//
// There is a real gap between the two. The ui marks itself running as
// it submits, and only then does the input reach run and get a
// cancellable context, so a ctrl+c in between used to hit `cancel ==
// nil` and do nothing — the ui believed it had cancelled and the turn
// carried on. Pressing ctrl+c the instant you realise the prompt was
// wrong is exactly when the gap is open, and a loaded CI runner
// widened it enough to lose the cancel every time.
//
// It cannot misfire on a later turn: the ui only calls Cancel while a
// turn is in flight (idle, ctrl+c arms the quit instead), so a pending
// flag always belongs to the turn that is about to start.
func (t *turns) Cancel() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.cancel != nil {
		t.cancel()
		return
	}
	t.pending = true
}

// run executes one input under a per-turn child of ctx, taking
// steers from the start; the runner's final take shuts the gate
// before its done.
func (t *turns) run(ctx context.Context, r *runner, input string, emit func(kind, text string)) {
	tctx, cancel := context.WithCancel(ctx)
	t.mu.Lock()
	t.cancel = cancel
	t.steering = true
	pending := t.pending
	t.pending = false
	t.mu.Unlock()
	if pending {
		cancel() // cancelled before it began; the runner unwinds at once
	}
	_ = r.Run(tctx, input, emit)
	t.mu.Lock()
	t.cancel = nil
	t.steering = false
	t.mu.Unlock()
	cancel()
}

// interrupter is the optional slice of the codemode service that
// aborts a running script (the codemode plugin's CodeMode has it).
type interrupter interface {
	Interrupt()
}

// ctxRunner is the optional slice that runs a script under a context,
// so host calls (tools.bash) can die with the turn instead of the
// interrupt waiting for them to return.
type ctxRunner interface {
	RunCtx(ctx context.Context, code string) (string, error)
}

// runCode runs one code block under ctx: a cancel kills a blocking
// host call through the context and interrupts the script for the
// rest. A codemode without RunCtx still gets the interrupt, after the
// host call returns on its own.
func (r *runner) runCode(ctx context.Context, code string) (string, error) {
	type res struct {
		out string
		err error
	}
	ch := make(chan res, 1)
	go func() {
		var out string
		var err error
		if cr, ok := r.code.(ctxRunner); ok {
			out, err = cr.RunCtx(ctx, code)
		} else {
			out, err = r.code.Run(code)
		}
		ch <- res{out, err}
	}()
	select {
	case rr := <-ch:
		return rr.out, rr.err
	case <-ctx.Done():
		select {
		case rr := <-ch: // finished in the same instant
			return rr.out, rr.err
		default:
		}
		if i, ok := r.code.(interrupter); ok {
			i.Interrupt()
		}
		rr := <-ch
		return rr.out, rr.err
	}
}
