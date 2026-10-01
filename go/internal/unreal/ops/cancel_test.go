//go:build !windows

package ops

import (
	"context"
	"encoding/json/jsontext"
	"sync/atomic"
	"testing"
	"time"

	"github.com/unreallabsai/unreal-agent/harness/operation"

	"github.com/andreylukin/bough/internal/agenttools"
	"github.com/andreylukin/bough/internal/unreal/boughcall"
	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

type cancelHandler struct {
	*stepHandler
	canceled chan operation.ID
}

func (h *cancelHandler) AddRemoteJob(op operation.Operation) error {
	h.added <- op
	if op.Status == operation.StatusCanceling {
		step, err := operation.CancelRemoteJob(op)
		if err != nil {
			return err
		}
		h.updates <- *step.Operation
	}
	return nil
}

func (h *cancelHandler) CancelRemoteJob(id operation.ID, _ string) error {
	h.canceled <- id
	return nil
}

// delayedAdd holds registration after the wrapper has accepted an ID,
// just as the coordinator can pause after publishing its call start.
type delayedAdd struct {
	operation.Manager
	ctx     context.Context
	entered chan struct{}
	release chan struct{}
}

func (m *delayedAdd) Add(op operation.Operation) error {
	close(m.entered)
	select {
	case <-m.release:
		return m.Manager.Add(op)
	case <-m.ctx.Done():
		return m.ctx.Err()
	}
}

func TestCancelBeforeAddDoesNotStartReadyOperation(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	h := &cancelHandler{stepHandler: newStepHandler(), canceled: make(chan operation.ID, 4)}
	m := New(ctx, operation.NewLocalOperationManager(ctx, h), nil)
	op := newOp(t, "cancel-before-add")
	if err := m.Cancel(op.ID, "stop"); err != nil {
		t.Fatal(err)
	}
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	if got := <-h.added; got.Status != operation.StatusCanceling {
		t.Fatalf("already canceled operation reached its handler as %s", got.Status)
	}
	if got := recv(t, m.Updates()); got.Status != operation.StatusCanceled {
		t.Fatalf("terminal update = %s, want canceled", got.Status)
	}
}

func TestCancelDuringAddSurvivesRegistration(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	h := &cancelHandler{stepHandler: newStepHandler(), canceled: make(chan operation.ID, 4)}
	delayed := &delayedAdd{Manager: operation.NewLocalOperationManager(ctx, h), ctx: ctx, entered: make(chan struct{}), release: make(chan struct{})}
	m := New(ctx, delayed, nil)
	op := newOp(t, "cancel-during-add")
	done := make(chan error, 1)
	go func() { done <- m.Add(op) }()
	<-delayed.entered
	if err := m.Cancel(op.ID, "stop"); err != nil {
		t.Fatal(err)
	}
	close(delayed.release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	select {
	case got := <-h.canceled:
		if got != op.ID {
			t.Fatalf("canceled %s, want %s", got, op.ID)
		}
	case <-time.After(time.Second):
		t.Fatal("cancellation was lost before inner registration")
	}
}

func TestCancelBeforeNativeAddNeverCallsTool(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	var called atomic.Bool
	h := boughcall.New(boughcall.Options{Tools: func(string) (agenttools.Tool, bool) {
		return agenttools.Tool{Name: "write", Call: func(context.Context, agenttools.Call) (agenttools.Result, error) {
			called.Store(true)
			return agenttools.Result{Text: "changed"}, nil
		}}, true
	}})
	t.Cleanup(func() { h.Close(); cancel() })
	m := New(ctx, operation.NewLocalOperationManager(ctx, h), nil)
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{
		Type: toolreg.PlanType, Version: toolreg.PlanVersion,
		Data: jsontext.Value(`{"call":"write-1","tool":"write","args":{}}`),
	})
	if err != nil {
		t.Fatal(err)
	}
	op := operation.Operation{ID: "native", Type: spec.Type, Version: spec.Version,
		Status: operation.StatusReady, State: spec.State, MaxOutputLength: spec.MaxOutputLength}
	if err := m.Cancel(op.ID, "stop"); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		if err := m.Add(op); err != nil {
			t.Fatal(err)
		}
		if got := recv(t, m.Updates()); got.Status != operation.StatusCanceled {
			t.Fatalf("native result = %s, want canceled", got.Status)
		}
		if called.Load() {
			t.Fatal("an already canceled native operation ran the tool")
		}
	}
}

func TestCancelBeforePrimitiveAdd(t *testing.T) {
	t.Parallel()
	m, _, _ := setup(t)
	spec, err := operation.NewValueSpec(jsontext.Value(`"unused"`))
	if err != nil {
		t.Fatal(err)
	}
	op := operation.Operation{ID: "primitive", Type: spec.Type, Version: spec.Version,
		Status: operation.StatusReady, State: spec.State}
	if err := m.Cancel(op.ID, "stop"); err != nil {
		t.Fatal(err)
	}
	if err := m.Add(op); err != nil {
		t.Fatal(err)
	}
	if got := recv(t, m.Updates()); got.Status != operation.StatusCanceled {
		t.Fatalf("primitive result = %s, want canceled", got.Status)
	}
}
