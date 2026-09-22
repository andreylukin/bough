package ops

import (
	"context"
	"encoding/json/jsontext"
	"testing"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/operation"
)

// stepHandler is a remote job handler the test drives by hand: finish
// completes an op it accepted.
type stepHandler struct {
	updates chan operation.Operation
	added   chan operation.Operation
}

func newStepHandler() *stepHandler {
	return &stepHandler{updates: make(chan operation.Operation, 16), added: make(chan operation.Operation, 16)}
}

func (h *stepHandler) RemoteJobPlanType() operation.RemoteJobPlanType       { return "test" }
func (h *stepHandler) RemoteJobPlanVersion() operation.RemoteJobPlanVersion { return 1 }
func (h *stepHandler) AddRemoteJob(op operation.Operation) error {
	h.added <- op
	return nil
}
func (h *stepHandler) CancelRemoteJob(operation.ID, string) error { return nil }
func (h *stepHandler) RemoteJobUpdates() <-chan operation.Operation {
	return h.updates
}

func (h *stepHandler) finish(t *testing.T, op operation.Operation, result string) {
	t.Helper()
	state, err := operation.DecodeRemoteJobState(op)
	if err != nil {
		t.Fatal(err)
	}
	state.TerminalResult = result
	step, err := operation.UpdateRemoteJob(op, state, operation.StatusCompleted)
	if err != nil {
		t.Fatal(err)
	}
	h.updates <- *step.Operation
}

func newOp(t *testing.T, id string) operation.Operation {
	t.Helper()
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{Type: "test", Version: 1, Data: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatal(err)
	}
	return operation.Operation{MaxOutputLength: spec.MaxOutputLength, ID: operation.ID(id), Type: spec.Type,
		Version: spec.Version, Status: operation.StatusReady, State: spec.State}
}

func recv(t *testing.T, ch <-chan operation.Operation) operation.Operation {
	t.Helper()
	select {
	case op, ok := <-ch:
		if !ok {
			t.Fatal("updates closed")
		}
		return op
	case <-time.After(5 * time.Second):
		t.Fatal("no update")
	}
	return operation.Operation{}
}

func quiet(t *testing.T, ch <-chan operation.Operation) {
	t.Helper()
	select {
	case op := <-ch:
		t.Fatalf("unexpected update %s %s", op.ID, op.Status)
	case <-time.After(100 * time.Millisecond):
	}
}

func setup(t *testing.T) (*Manager, *stepHandler, context.CancelFunc) {
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	h := newStepHandler()
	return New(ctx, operation.NewLocalOperationManager(ctx, h), nil), h, cancel
}

// The bug this package exists for: a coordinator took the terminal
// update off the channel and died before saving it. The next one
// re-Adds the op from its stale store and must get the result again.
func TestReAddReplaysLatest(t *testing.T) {
	t.Parallel()
	m, h, _ := setup(t)
	op := newOp(t, "op1")
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	accepted := <-h.added
	h.finish(t, accepted, "done!")
	first := recv(t, m.Updates()) // consumed by the coordinator that died
	if first.Status != operation.StatusCompleted {
		t.Fatalf("status %s", first.Status)
	}
	if err := m.Add(op); err != nil { // the new coordinator's re-Add
		t.Fatal(err)
	}
	again := recv(t, m.Updates())
	if again.ID != "op1" || again.Status != operation.StatusCompleted {
		t.Fatalf("replay %s %s", again.ID, again.Status)
	}
	if l, ok := m.Latest("op1"); !ok || l.Status != operation.StatusCompleted {
		t.Fatalf("latest %v %v", l.Status, ok)
	}
	select {
	case <-h.added:
		t.Fatal("a known op reached the handler twice")
	default:
	}
}

// A replay and an update still queued for the same op coalesce: the
// coordinator reads the op once.
func TestQueuedUpdateCoalescesWithReplay(t *testing.T) {
	t.Parallel()
	m, h, _ := setup(t)
	op := newOp(t, "op2")
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	h.finish(t, <-h.added, "r")
	// Let the pump queue the update, then re-Add before anyone reads.
	deadline := time.Now().Add(5 * time.Second)
	for {
		if l, _ := m.Latest("op2"); l.Status == operation.StatusCompleted {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("update never arrived")
		}
		time.Sleep(time.Millisecond)
	}
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	got := recv(t, m.Updates())
	if got.Status != operation.StatusCompleted {
		t.Fatalf("status %s", got.Status)
	}
	quiet(t, m.Updates())
}

// A known op that never progressed is not replayed: its first update
// still reaches whoever reads next.
func TestReAddOfUnprogressedOpIsSilent(t *testing.T) {
	t.Parallel()
	m, h, _ := setup(t)
	op := newOp(t, "op3")
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	accepted := <-h.added
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	quiet(t, m.Updates())
	h.finish(t, accepted, "late")
	if got := recv(t, m.Updates()); got.Status != operation.StatusCompleted {
		t.Fatalf("status %s", got.Status)
	}
}

// Updates is one channel for the session: closed only when its ctx ends.
func TestUpdatesClosesWithTheSession(t *testing.T) {
	t.Parallel()
	m, _, cancel := setup(t)
	ch := m.Updates()
	quiet(t, ch)
	cancel()
	select {
	case _, ok := <-ch:
		if ok {
			t.Fatal("update after cancel")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("updates not closed")
	}
}
