package loop

// Turn cancellation: the "cancel" service (func()) cancels the turn in
// flight. The runner sees ctx.Err() after the pending LLM call (or
// code run) unwinds, records a "cancelled" entry (model-invisible
// under DefaultProject) and ends the turn with the usual "done".
//
// Steering: the "steer" service (func(text string) bool) hands the
// turn in flight a message without ending it. It never interrupts a
// block mid-execution; the runner lands pending steers at the next
// boundary (between blocks, and before each LLM call), drops the rest
// of the current reply, and asks the model again with the steer in
// context. False when no turn is running (the caller queues it as
// ordinary input instead).

import (
	"context"
	"strings"
	"sync"
)

// turns hands each input its own cancellable context and remembers
// the live one's cancel func for the "cancel" service, plus the
// steering messages sent since the runner last looked.
type turns struct {
	mu     sync.Mutex
	cancel context.CancelFunc
	steers []string
}

// Steer queues text for the turn in flight; false while idle.
func (t *turns) Steer(text string) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.cancel == nil {
		return false
	}
	t.steers = append(t.steers, text)
	return true
}

// takeSteers hands the runner every steer sent since the last take.
func (t *turns) takeSteers() []string {
	t.mu.Lock()
	defer t.mu.Unlock()
	s := t.steers
	t.steers = nil
	return s
}

// Cancel aborts the turn in flight; a no-op while idle.
func (t *turns) Cancel() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.cancel != nil {
		t.cancel()
	}
}

// run executes one input under a per-turn child of ctx. A steer that
// arrived after the runner's last boundary (during the final reply)
// missed its turn: it is announced and run as the next turn's input,
// never dropped.
func (t *turns) run(ctx context.Context, r *runner, input string, emit func(kind, text string)) {
	for {
		tctx, cancel := context.WithCancel(ctx)
		t.mu.Lock()
		t.cancel = cancel
		t.mu.Unlock()
		_ = r.Run(tctx, input, emit)
		t.mu.Lock()
		t.cancel = nil
		late := t.steers
		t.steers = nil
		t.mu.Unlock()
		cancel()
		if len(late) == 0 {
			return
		}
		for _, s := range late {
			emit("steer", s)
		}
		input = strings.Join(late, "\n")
	}
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
