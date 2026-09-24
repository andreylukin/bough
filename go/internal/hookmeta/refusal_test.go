package hookmeta

import "testing"

func TestRefusal(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name             string
		res              map[string]any
		decision, reason string
	}{
		{"nil", nil, "", ""},
		{"nothing", map[string]any{"code": "x"}, "", ""},
		{"deny string", map[string]any{"deny": "no"}, "denied", "no"},
		{"deny empty", map[string]any{"deny": ""}, "denied", "denied"},
		{"deny true", map[string]any{"deny": true}, "denied", "denied"},
		{"deny false", map[string]any{"deny": false}, "", ""},
		{"deny number", map[string]any{"deny": 1}, "", ""},
		{"block string", map[string]any{"block": "stop"}, "blocked", "stop"},
		{"block true", map[string]any{"block": true}, "blocked", "blocked"},
		{"block false", map[string]any{"block": false}, "", ""},
		{"block wins", map[string]any{"block": "b", "deny": "d"}, "blocked", "b"},
		{"false block, deny", map[string]any{"block": false, "deny": "d"}, "denied", "d"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			d, r := Refusal(tc.res)
			if d != tc.decision || r != tc.reason {
				t.Fatalf("Refusal(%v) = %q, %q, want %q, %q", tc.res, d, r, tc.decision, tc.reason)
			}
		})
	}
}
