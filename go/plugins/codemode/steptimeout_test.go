package codemode

import (
	"testing"
	"time"
)

func TestStepTimeoutEnv(t *testing.T) {
	t.Setenv("BOUGH_CODEMODE_TIMEOUT", "")
	if d := StepTimeout(); d != 30*time.Second {
		t.Fatalf("default = %s, want 30s", d)
	}
	t.Setenv("BOUGH_CODEMODE_TIMEOUT", "2s")
	if d := StepTimeout(); d != 2*time.Second {
		t.Fatalf("env 2s = %s", d)
	}
	t.Setenv("BOUGH_CODEMODE_TIMEOUT", "junk")
	if d := StepTimeout(); d != 30*time.Second {
		t.Fatalf("junk = %s, want 30s", d)
	}
}
