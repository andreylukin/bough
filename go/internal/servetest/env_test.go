package servetest

import (
	"strings"
	"testing"
)

func TestChildEnvContainerRuntime(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		extra []string
		want  string
	}{
		{name: "disabled by default", want: "none"},
		{name: "explicit runtime allowed", extra: []string{"BOUGH_CONTAINER="}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, found := "", false
			// exec.Cmd uses the last value for duplicate environment keys.
			for _, kv := range childEnv(t.TempDir(), tc.extra) {
				if v, ok := strings.CutPrefix(kv, "BOUGH_CONTAINER="); ok {
					got, found = v, true
				}
			}
			if !found || got != tc.want {
				t.Fatalf("container runtime = %q (present=%t), want %q", got, found, tc.want)
			}
		})
	}
}
