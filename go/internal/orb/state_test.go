package orb

import (
	"encoding/json"
	"strings"
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

func phaseNames(st State) []string {
	var out []string
	for _, p := range st.Phases {
		out = append(out, p.Name)
	}
	return out
}

func TestPhaseLine(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 17, 10, 0, 30, 0, time.UTC)
	cases := []struct {
		st   State
		want string
	}{
		{State{Project: "web", Status: StatusBuilding, Phase: PhaseBuild, Phases: []Phase{{Name: PhaseSync, StartedAt: now.Add(-40 * time.Second), EndedAt: now.Add(-30 * time.Second)}, {Name: PhaseBuild, StartedAt: now.Add(-12 * time.Second)}}}, "orb web · build image 12s"},
		{State{Project: "web", Status: StatusRunning, Phase: PhaseReady}, "orb web · running"},
		{State{Project: "web", Status: StatusStopped, Phase: PhaseReady}, "orb web · stopped"},
		{State{Project: "web", Status: StatusFailed, Phase: PhaseResume, Phases: []Phase{{Name: PhaseResume, Error: "exit 3"}}}, "orb web · failed at resume.sh"},
		{State{Project: "web"}, "orb web · starting"},
	}
	for _, c := range cases {
		if got := PhaseLine(c.st, now); got != c.want {
			t.Errorf("PhaseLine(%s/%s) = %q, want %q", c.st.Status, c.st.Phase, got, c.want)
		}
	}
}

// A running phase has no endedAt on the wire: the web reads its presence.
func TestRunningPhaseOmitsEnd(t *testing.T) {
	t.Parallel()
	b, _ := json.Marshal(Phase{Name: PhaseBuild, StartedAt: time.Now()})
	if strings.Contains(string(b), "endedAt") {
		t.Fatalf("phase json = %s", b)
	}
}
