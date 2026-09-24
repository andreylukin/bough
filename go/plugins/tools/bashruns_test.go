package tools

import "testing"

// The loop diffs Bash() around a block to stamp that block's exit code;
// Take must not reset the count it diffs, or a turn's second block would
// look like it ran nothing.
func TestBashCountsSurviveTake(t *testing.T) {
	t.Parallel()
	s := &Stats{}
	if n, _ := s.Bash(); n != 0 {
		t.Fatalf("fresh runs = %d, want 0", n)
	}
	s.exited(0)
	s.exited(2)
	if n, exit := s.Bash(); n != 2 || exit != 2 {
		t.Fatalf("Bash() = %d, %d; want 2, 2", n, exit)
	}
	if _, exit, ran := s.Take(); !ran || exit != 2 {
		t.Fatalf("Take() exit=%d ran=%v; want 2, true", exit, ran)
	}
	s.exited(0)
	if n, exit := s.Bash(); n != 3 || exit != 0 {
		t.Fatalf("after Take, Bash() = %d, %d; want 3, 0", n, exit)
	}
}
