package tools

import (
	"strings"
	"sync"
	"testing"
	"time"
)

type fakeRecorder struct {
	mu      sync.Mutex
	entries []map[string]any
}

func (f *fakeRecorder) record(kind string, data map[string]any) {
	if kind != "job" {
		return
	}
	f.mu.Lock()
	f.entries = append(f.entries, data)
	f.mu.Unlock()
}

func (f *fakeRecorder) snapshot() []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]map[string]any(nil), f.entries...)
}

// A background job writes a typed started entry, then a finished one
// carrying the exit code, so serve needs no regex over the text.
func TestJobWritesTypedEntries(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	rec := &fakeRecorder{}
	s.jobs.record = rec.record
	if _, err := s.bash("exit 3", 60, "never"); err != nil {
		t.Fatalf("bash: %v", err)
	}
	waitFor(t, "the finished entry", func() bool { return len(rec.snapshot()) == 2 })
	got := rec.snapshot()
	if got[0]["event"] != "started" || got[0]["id"] != 1 || got[0]["cmd"] != "exit 3" || got[0]["until"] != "never" {
		t.Fatalf("started entry = %v", got[0])
	}
	if _, has := got[0]["exit"]; has {
		t.Fatalf("started entry has an exit: %v", got[0])
	}
	if got[1]["event"] != "finished" || got[1]["id"] != 1 || got[1]["exit"] != 3 {
		t.Fatalf("finished entry = %v", got[1])
	}
}

// A job killed at its limit records exit -1; no "until" is omitted.
func TestJobTimeoutRecordsMinusOne(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	rec := &fakeRecorder{}
	s.jobs.record = rec.record
	if _, err := s.bash("sleep 30", "200ms"); err != nil {
		t.Fatalf("bash: %v", err)
	}
	waitFor(t, "the finished entry", func() bool { return len(rec.snapshot()) == 2 })
	got := rec.snapshot()
	if _, has := got[0]["until"]; has {
		t.Fatalf("until without a pattern: %v", got[0])
	}
	if got[1]["exit"] != -1 {
		t.Fatalf("finished entry = %v, want exit -1", got[1])
	}
}

// Without a history-record service jobs still run and notify.
func TestJobWithoutRecorder(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	if _, err := s.bash("true", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	waitFor(t, "the notice", func() bool { return len(s.jobs.Take()) == 1 })
}

// stoppedOrb says every job ended because its orb was stopped.
type stoppedOrb struct{ fakeOrb }

func (o *stoppedOrb) StoppedSince(time.Time) bool { return true }

// A job killed by Stop orb is not news: it records stopped, queues no
// notice (so no paid wake-up turn) and never reads as failed.
func TestJobKilledByOrbStopWakesNothing(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	rec := &fakeRecorder{}
	s.jobs.record = rec.record
	o := &stoppedOrb{fakeOrb{root: t.TempDir()}}
	s.jobs.project = &projectMode{slug: "demo", orb: func() (orbExec, error) { return o, nil }}
	if _, err := s.bash("exit 137", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	waitFor(t, "the finished entry", func() bool { return len(rec.snapshot()) == 2 })
	if got := rec.snapshot()[1]; got["stopped"] != true {
		t.Fatalf("finished entry = %v, want stopped", got)
	}
	select {
	case <-s.jobs.Wake():
		t.Fatalf("a stopped job woke the agent: %q", s.jobs.Take())
	case <-time.After(300 * time.Millisecond):
	}
	if out, _ := s.jobs.jobs(); !strings.Contains(out, "[stopped with the orb]") {
		t.Fatalf("jobs = %q", out)
	}
}
