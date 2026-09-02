package loop

// Turn cancellation: the "cancel" service (func()) cancels the turn in
// flight. The runner sees ctx.Err() after the pending LLM call (or
// code run) unwinds, records a "cancelled" entry (model-invisible
// under DefaultProject) and ends the turn with the usual "done".

import (
	"context"
	"sync"
)

// turns hands each input its own cancellable context and remembers
// the live one's cancel func for the "cancel" service.
type turns struct {
	mu     sync.Mutex
	cancel context.CancelFunc
}

// Cancel aborts the turn in flight; a no-op while idle.
func (t *turns) Cancel() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.cancel != nil {
		t.cancel()
	}
}

// run executes one input under a per-turn child of ctx.
func (t *turns) run(ctx context.Context, r *runner, input string, emit func(kind, text string)) {
	tctx, cancel := context.WithCancel(ctx)
	t.mu.Lock()
	t.cancel = cancel
	t.mu.Unlock()
	_ = r.Run(tctx, input, emit)
	t.mu.Lock()
	t.cancel = nil
	t.mu.Unlock()
	cancel()
}

// interrupter is the optional slice of the codemode service that
// aborts a running script (the codemode plugin's CodeMode has it).
type interrupter interface {
	Interrupt()
}

// runCode runs one code block, interrupting it when ctx is cancelled
// mid-run (a host call such as tools.bash still returns on its own
// first: the interrupt lands when the script resumes).
func (r *runner) runCode(ctx context.Context, code string) (string, error) {
	type res struct {
		out string
		err error
	}
	ch := make(chan res, 1)
	go func() {
		out, err := r.code.Run(code)
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
