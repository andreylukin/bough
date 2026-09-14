package main

import (
	"os"
	"testing"
)

// Not parallel: t.Setenv forbids it, and the vars are process-wide.
// After the take, a tools.bash child must not inherit either var.
func TestTakeSessionEnvClears(t *testing.T) {
	t.Setenv("BOUGH_SPAWNED_BY", "parent-1")
	t.Setenv("BOUGH_SESSION_ID", "child-1")
	by, id := takeSessionEnv()
	if by != "parent-1" || id != "child-1" {
		t.Fatalf("takeSessionEnv = %q, %q", by, id)
	}
	for _, k := range []string{"BOUGH_SPAWNED_BY", "BOUGH_SESSION_ID"} {
		if _, set := os.LookupEnv(k); set {
			t.Fatalf("%s still set after take", k)
		}
	}
}
