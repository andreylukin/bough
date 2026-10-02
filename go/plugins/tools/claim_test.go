package tools

import (
	"context"
	"strings"
	"testing"
	"time"
)

// Let the waiter finish before claim begins waiting: foreground ownership
// must already be reserved, or a quick job wakes an unnecessary later turn.
func TestBashQuickJobFinishesBeforeClaimWait(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	s.jobs.grace = 5 * time.Second
	s.jobs.pause = func() func() {
		done := make(chan struct{})
		go func() { s.jobs.running.Wait(); close(done) }()
		select {
		case <-done:
		case <-time.After(5 * time.Second):
			t.Fatal("job did not finish before claim waited")
		}
		return func() {}
	}
	out, err := s.bash("echo quick-one", 60)
	if err != nil || !strings.Contains(out, "finished in") || !strings.Contains(out, "quick-one") {
		t.Fatalf("quick foreground result: %q %v", out, err)
	}
	select {
	case <-s.jobs.Wake():
		t.Fatal("a claimed job must not wake a turn")
	default:
	}
	if n := s.jobs.Take(); len(n) != 0 {
		t.Fatalf("a claimed job left a notice: %q", n)
	}
}

func TestBashClaimCancellationHandsBackJob(t *testing.T) {
	t.Parallel()
	s := newTestStats(t)
	s.jobs.grace = 5 * time.Second
	turn, cancel := context.WithCancel(context.Background())
	defer cancel()
	s.jobs.runCtx = func() context.Context { return turn }
	s.jobs.pause = func() func() {
		cancel() // cancel after start, before the foreground wait
		return func() {}
	}
	out, err := s.bash("sleep 30", 60)
	if err != nil || !strings.Contains(out, "job 1 started") {
		t.Fatalf("cancelled claim = %q, %v", out, err)
	}
	if _, err := s.jobs.jobKill(1); err != nil {
		t.Fatal(err)
	}
	select {
	case <-s.jobs.Wake():
	case <-time.After(5 * time.Second):
		t.Fatal("job handed back on turn cancellation did not notify")
	}
	if n := s.jobs.Take(); len(n) != 1 {
		t.Fatalf("notices = %q", n)
	}
}

func TestBashQuickBackgroundJobsStillNotify(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		grace time.Duration
		until string
	}{
		{name: "no grace"},
		{name: "watch pattern", grace: 5 * time.Second, until: "never matches"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			s := newTestStats(t)
			s.jobs.grace = tc.grace
			out, err := s.bash("echo background", 60, tc.until)
			if err != nil || !strings.Contains(out, "job 1 started") {
				t.Fatalf("background result = %q, %v", out, err)
			}
			select {
			case <-s.jobs.Wake():
			case <-time.After(5 * time.Second):
				t.Fatal("background job did not notify")
			}
			if n := s.jobs.Take(); len(n) != 1 || !strings.Contains(n[0], "background") {
				t.Fatalf("notices = %q", n)
			}
		})
	}
}
