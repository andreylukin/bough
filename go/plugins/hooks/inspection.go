package hooks

import (
	"encoding/json"
	"slices"
)

// Inspection is diagnostic, not another hook output limit. Oversize values
// are omitted as a whole so a partial object can never masquerade as JSON.
const maxInspectionBytes = 64 * 1024

func snapshot(v any) (data json.RawMessage, size int, truncated bool, failure string) {
	defer func() {
		if recover() != nil {
			data, size, truncated, failure = nil, 0, false, "JSON snapshot panicked"
		}
	}()
	b, err := json.Marshal(v)
	if err != nil {
		return nil, 0, false, "value is not JSON serializable"
	}
	if len(b) > maxInspectionBytes {
		return nil, len(b), true, ""
	}
	return b, len(b), false, ""
}

func (f *Fire) captureInput(v any) {
	f.Input, f.InputBytes, f.InputTruncated, f.InputError = snapshot(v)
}

func (f *Fire) captureOutput(v any) {
	f.Output, f.OutputBytes, f.OutputTruncated, f.OutputError = snapshot(v)
}

func cloneFire(f Fire) Fire {
	f.Input = slices.Clone(f.Input)
	f.Output = slices.Clone(f.Output)
	f.Truncated = slices.Clone(f.Truncated)
	return f
}
