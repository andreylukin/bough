// Package toolreg is W3's (go/docs/unreal-engine.md §5.4). This file is
// the B0 stub for the slice of the frozen surface the projector compiles
// against. W4 wrote it because B0 never ran; W3's real package replaces
// it, and this file is deleted at integration.
package toolreg

import (
	"encoding/json"

	"github.com/unreallabsai/unreal-agent/harness/operation"
	"github.com/unreallabsai/unreal-agent/harness/tool"
)

const PlanType operation.RemoteJobPlanType = "bough.call"
const PlanVersion operation.RemoteJobPlanVersion = 1

// Plan is RemoteJobPlan.Data for a bough.call op.
type Plan struct {
	Call string          `json:"call"`
	Tool string          `json:"tool"`
	Args json.RawMessage `json:"args"`
}

// Handle is RemoteJobState.Handle, written by boughcall on terminal updates.
type Handle struct {
	Detail    string         `json:"detail,omitempty"`
	Data      map[string]any `json:"data,omitempty"` // Result.Data
	Error     string         `json:"error,omitempty"`
	MS        int64          `json:"ms,omitempty"`
	Spill     string         `json:"spill,omitempty"`
	Truncated bool           `json:"truncated,omitempty"`
}

// Render is the pure result text for a call. Stub: nothing is terminal.
func Render(callID string, status tool.CallStatus, ops []operation.Operation) (text string, h Handle, terminal bool) {
	return "", Handle{}, false
}
