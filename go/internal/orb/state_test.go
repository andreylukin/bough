package orb

import (
	"testing"
	"time"
)

func TestMarkStoppedAndRestore(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if prev, err := MarkStopped(home, "none"); err != nil || prev.Session != "" {
		t.Fatalf("no state: %+v %v", prev, err)
	}
	if err := writeState(home, State{Session: "s", Project: "p", Status: StatusRunning, PID: 7}); err != nil {
		t.Fatal(err)
	}
	before := time.Now()
	prev, err := MarkStopped(home, "s")
	if err != nil || prev.Status != StatusRunning {
		t.Fatalf("prev = %+v %v", prev, err)
	}
	st, _ := ReadState(home, "s")
	if st.Status != StatusStopped || st.PID != 7 || st.UpdatedAt.Before(before.Add(-time.Second)) {
		t.Fatalf("marked = %+v", st)
	}
	if err := Restore(home, prev); err != nil {
		t.Fatal(err)
	}
	if st, _ := ReadState(home, "s"); st.Status != StatusRunning {
		t.Fatalf("restored = %+v", st)
	}
}
