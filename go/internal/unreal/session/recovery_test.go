//go:build !windows

package session

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"testing"

	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/session"
	"github.com/unreallabsai/unreal-agent/harness/sessionstore/localfile"

	"github.com/andreylukin/bough/internal/unreal/toolreg"
)

func recoveryOperation(t *testing.T, plan operation.RemoteJobPlanType, version operation.RemoteJobPlanVersion, status operation.Status) operation.Operation {
	t.Helper()
	spec, err := operation.NewRemoteJobSpec(operation.RemoteJobPlan{Type: plan, Version: version, Data: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatal(err)
	}
	return operation.Operation{ID: "recovery-op", Type: spec.Type, Version: spec.Version, Status: status, State: spec.State, MaxOutputLength: spec.MaxOutputLength}
}

func TestRecoveryLeavesReadyAndForeignOperationsAlone(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		op   operation.Operation
	}{
		{"ready native", recoveryOperation(t, toolreg.PlanType, toolreg.PlanVersion, operation.StatusReady)},
		{"foreign plan", recoveryOperation(t, "other.plan", 1, operation.StatusAwaiting)},
		{"foreign version", recoveryOperation(t, toolreg.PlanType, toolreg.PlanVersion+1, operation.StatusAwaiting)},
		{"primitive", operation.Operation{ID: "primitive", Type: operation.TypeValue, Status: operation.StatusAwaiting}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			// No store: these snapshots must never reach SaveOperation.
			r := &Runtime{}
			if changed, err := r.recoverNativeOperations(context.Background(), []operation.Operation{tc.op}); changed || err != nil {
				t.Fatalf("recovery changed an operation it does not own: %v %v", changed, err)
			}
		})
	}
}

func TestRecoveryPropagatesSaveFailure(t *testing.T) {
	t.Parallel()
	store, err := localfile.New(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	const sid = "save-failure"
	if _, err := store.Create(context.Background(), session.ID(sid)); err != nil {
		t.Fatal(err)
	}
	r := &Runtime{sid: sid, store: store}
	// An unregistered ID makes the real store reject the terminal save.
	// Open must fail rather than resume with an unpersisted recovery.
	op := recoveryOperation(t, toolreg.PlanType, toolreg.PlanVersion, operation.StatusAwaiting)
	if changed, err := r.recoverNativeOperations(context.Background(), []operation.Operation{op}); changed || !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("failed save reported successful recovery: %v %v", changed, err)
	}
}
