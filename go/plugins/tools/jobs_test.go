package tools

import (
	"context"
	"strings"
	"testing"
	"time"
)

// waitFor polls until cond or the deadline; background jobs are
// inherently asynchronous, so the tests never sleep a fixed time.
func waitFor(t *testing.T, why string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", why)
}

func newTestStats(t *testing.T) *Stats {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	return &Stats{jobs: newJobs(ctx)}
}

// A limit turns tools.bash into a background job: it returns at once,
// long before the command it started could finish.
func TestBashWithLimitReturnsImmediately(t *testing.T) {
	s := newTestStats(t)
	start := time.Now()
	out, err := s.bash("sleep 30", 60)
	if err != nil {
		t.Fatalf("bash: %v", err)
	}
	if d := time.Since(start); d > 2*time.Second {
		t.Fatalf("background bash blocked for %s", d)
	}
	if !strings.Contains(out, "job 1 started") {
		t.Fatalf("no job handle in %q", out)
	}
	if got, _ := s.jobs.jobs(); !strings.Contains(got, "running") {
		t.Fatalf("job not running: %q", got)
	}
	if _, err := s.jobs.jobKill(1); err != nil {
		t.Fatalf("kill: %v", err)
	}
}

// A finished job queues a notice and signals the wake channel — the
// two things the loop needs to bring the agent back.
func TestFinishedJobNotifies(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("echo hello-from-job", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	select {
	case <-s.jobs.Wake():
	case <-time.After(5 * time.Second):
		t.Fatal("no wake signal")
	}
	notices := s.jobs.Take()
	if len(notices) != 1 || !strings.Contains(notices[0], "hello-from-job") {
		t.Fatalf("notice missing the output: %q", notices)
	}
	if got := s.jobs.Take(); len(got) != 0 {
		t.Fatalf("Take did not drain: %q", got)
	}
}

// The watch pattern reports while the job is still running, and does
// not stop it.
func TestUntilFiresWhileRunning(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("echo READY; sleep 30", 60, "READY"); err != nil {
		t.Fatalf("bash: %v", err)
	}
	select {
	case <-s.jobs.Wake():
	case <-time.After(5 * time.Second):
		t.Fatal("no wake signal for the match")
	}
	n := s.jobs.Take()
	if len(n) != 1 || !strings.Contains(n[0], "matched") {
		t.Fatalf("not a match notice: %q", n)
	}
	if b := s.jobs.find(1); b == nil || b.done {
		t.Fatal("the match killed the job")
	}
	s.jobs.jobKill(1)
}

// jobWait blocks until the job exits and hands back its output.
func TestJobWait(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("sleep 0.2; echo done-waiting", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	out, err := s.jobs.jobWait(1, 5)
	if err != nil {
		t.Fatalf("jobWait: %v", err)
	}
	if !strings.Contains(out, "done-waiting") || !strings.Contains(out, "exited 0") {
		t.Fatalf("jobWait output: %q", out)
	}
}

// A killed job stops running and says why.
func TestJobKill(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("sleep 30", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	if _, err := s.jobs.jobKill(1); err != nil {
		t.Fatalf("kill: %v", err)
	}
	waitFor(t, "the job to stop", func() bool {
		b := s.jobs.find(1)
		b.mu.Lock()
		defer b.mu.Unlock()
		return b.done
	})
	if _, err := s.jobs.job(9); err == nil {
		t.Fatal("an unknown job id must be an error")
	}
}

// The limit kills a job that overruns it.
func TestJobLimitKills(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("sleep 30", 0.2); err != nil {
		t.Fatalf("bash: %v", err)
	}
	select {
	case <-s.jobs.Wake():
	case <-time.After(5 * time.Second):
		t.Fatal("the limit did not fire")
	}
	n := s.jobs.Take()
	if len(n) != 1 || !strings.Contains(n[0], "killed after") {
		t.Fatalf("expected a kill notice, got %q", n)
	}
}

// Without a limit bash is unchanged: foreground, output returned.
func TestBashWithoutLimitIsForeground(t *testing.T) {
	s := newTestStats(t)
	out, err := s.bash("echo foreground")
	if err != nil {
		t.Fatalf("bash: %v", err)
	}
	if strings.TrimSpace(out) != "foreground" {
		t.Fatalf("output: %q", out)
	}
	if got, _ := s.jobs.jobs(); got != "no background jobs" {
		t.Fatalf("a foreground call created a job: %q", got)
	}
}

// A bad limit or a bad watch pattern is a clear error, not a job.
func TestBadArguments(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("true", "not-a-duration"); err == nil {
		t.Fatal("a nonsense limit must be an error")
	}
	if _, err := s.bash("true", 60, "["); err == nil {
		t.Fatal("a bad regexp must be an error")
	}
}

// Output far over the cap keeps both ends: the head says what the
// command started doing, the tail says how it ended.
func TestJobOutputKeepsBothEnds(t *testing.T) {
	s := newTestStats(t)
	if _, err := s.bash("seq 1 200000", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	waitFor(t, "the job to finish", func() bool {
		b := s.jobs.find(1)
		b.mu.Lock()
		defer b.mu.Unlock()
		return b.done
	})
	out, err := s.jobs.job(1)
	if err != nil {
		t.Fatalf("job: %v", err)
	}
	for _, want := range []string{"\n1\n", "bytes cut", "\n200000\n"} {
		if !strings.Contains(out, want) {
			t.Fatalf("missing %q in the kept output", want)
		}
	}
}

// Cancelling the turn (esc) kills the foreground command and every
// subagent, but NOT a background job: that is the whole point of
// starting one. The job hangs off the plugin's context, so a cancelled
// script context leaves it running.
func TestBackgroundJobSurvivesTurnCancel(t *testing.T) {
	s := newTestStats(t)
	turn, cancel := context.WithCancel(context.Background())
	s.runCtx = func() context.Context { return turn }

	if _, err := s.bash("sleep 0.3; echo SURVIVED", 60); err != nil {
		t.Fatalf("bash: %v", err)
	}
	cancel() // the user pressed esc

	// A foreground call under the same cancelled context does die.
	if _, err := s.bash("echo foreground"); err == nil {
		t.Fatal("a foreground command must not survive the cancel")
	}

	select {
	case <-s.jobs.Wake():
	case <-time.After(5 * time.Second):
		t.Fatal("the background job died with the turn")
	}
	n := s.jobs.Take()
	if len(n) != 1 || !strings.Contains(n[0], "SURVIVED") {
		t.Fatalf("job notice = %q", n)
	}
}
