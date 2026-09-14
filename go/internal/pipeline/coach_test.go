package pipeline

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve"
)

// coachRig runs one coach against a fake target session "t1" whose
// transcript keeps growing, and returns the lines the target received.
func coachRig(t *testing.T, c Coach, coachReply string, setup func(f *fakeSessions), running *atomic.Bool) ([]string, *fakeSessions, *Runner) {
	t.Helper()
	p, err := writePipeline(t, validYML, nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(func(id, prompt string) string {
		if strings.Contains(prompt, "Recent activity:") {
			return coachReply
		}
		return "HANG"
	})
	f.Create(serve.CreateOptions{ID: "t1"})
	if setup != nil {
		setup(f)
	}
	r := newRun(t, p, f)
	r.st.Sessions["coder"] = "t1"
	r.setInflight("coder", 1)
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		r.coach(ctx, c, func() (string, bool) { return "t1", running.Load() })
	}()
	for i := range 20 {
		f.mu.Lock()
		f.appendLocked("t1", "assistant", "working "+string(rune('a'+i)))
		f.mu.Unlock()
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	<-done
	var steers []string
	for _, s := range f.sentTo("t1") {
		if strings.HasPrefix(s, "[coach] ") {
			steers = append(steers, s)
		}
	}
	return steers, f, r
}

func on() *atomic.Bool { b := &atomic.Bool{}; b.Store(true); return b }

// Every tick asks the same coach session: one per tick in project mode
// started a container per tick on the work machine, and none stopped.
func TestCoachReusesOneSession(t *testing.T) {
	c := Coach{Name: "c", Target: "coder", Every: 15 * time.Millisecond, Cooldown: time.Hour, MaxSteers: 3, Prompt: "watch"}
	_, f, _ := coachRig(t, c, "NONE", nil, on())
	coaches := f.created[1:] // created[0] is the target t1
	if len(coaches) != 1 {
		t.Fatalf("coach sessions created = %d, want 1 reused across ticks", len(coaches))
	}
	if n := len(f.sentTo(coaches[0].ID)); n < 2 {
		t.Errorf("coach session asked %d times, want several ticks on it", n)
	}
	if !contains(f.killed, coaches[0].ID) {
		t.Errorf("coach session %s not killed when the coach stopped", coaches[0].ID)
	}
}

func TestCoachSteersOnceThenCooldown(t *testing.T) {
	c := Coach{Name: "c", Target: "coder", Every: 15 * time.Millisecond, Cooldown: time.Hour, MaxSteers: 3, Prompt: "watch"}
	steers, f, r := coachRig(t, c, "Run the tests before editing more.", nil, on())
	if len(steers) != 1 || steers[0] != "[coach] Run the tests before editing more." {
		t.Fatalf("steers = %q", steers)
	}
	if b, err := os.ReadFile(filepath.Join(r.Dir(), "coach", "1.md")); err != nil || !strings.Contains(string(b), "sent") || !strings.Contains(string(b), "assistant: working") {
		t.Errorf("coach/1.md = %s, %v", b, err)
	}
	// The coach session got only the transcript, ran in its target's
	// mode (so it reads no more than the coder), and was killed after.
	target := r.p.Nodes["coder"]
	for _, opt := range f.created[1:] {
		if opt.Mode != target.Mode || opt.Slug != target.Project || opt.Origin != "loop" {
			t.Errorf("coach create = %+v", opt)
		}
		if !contains(f.killed, opt.ID) {
			t.Errorf("coach session %s not killed", opt.ID)
		}
	}
	evs, _ := ReadEvents(r.opt.Home, r.ID())
	n := 0
	for _, e := range evs {
		if e.Kind == "steer" {
			n++
		}
	}
	if n != 1 {
		t.Errorf("steer events = %d", n)
	}
}

func TestCoachSkips(t *testing.T) {
	c := Coach{Name: "c", Target: "coder", Every: 15 * time.Millisecond, Cooldown: time.Millisecond, Prompt: "watch"}
	if steers, f, _ := coachRig(t, c, "NONE", nil, on()); len(steers) != 0 || f.createdCount() < 2 {
		t.Errorf("NONE: steers %q, coach runs %d", steers, f.createdCount()-1)
	}
	ask := func(f *fakeSessions) { f.asks["t1"] = &serve.Ask{Text: "?"} }
	if steers, f, _ := coachRig(t, c, "steer", ask, on()); len(steers) != 0 || f.createdCount() != 1 {
		t.Errorf("pending ask: steers %q, coach runs %d", steers, f.createdCount()-1)
	}
	dead := func(f *fakeSessions) { f.live["t1"] = false }
	if steers, f, _ := coachRig(t, c, "steer", dead, on()); len(steers) != 0 || f.createdCount() != 1 {
		t.Errorf("not live: steers %q, coach runs %d", steers, f.createdCount()-1)
	}
	if steers, f, _ := coachRig(t, c, "steer", nil, &atomic.Bool{}); len(steers) != 0 || f.createdCount() != 1 {
		t.Errorf("between visits: steers %q, coach runs %d", steers, f.createdCount()-1)
	}
	long := strings.Repeat("x", 501)
	if steers, _, _ := coachRig(t, c, long, nil, on()); len(steers) != 0 {
		t.Errorf("long reply steered: %d", len(steers))
	}
}

func TestCoachNeverChangesRoute(t *testing.T) {
	yml := validYML + `coaches:
  - name: c
    target: coder
    every: 10ms
    cooldown: 1ms
    max_steers: 5
    prompt: watch
`
	p, err := writePipeline(t, yml, nil)
	if err != nil {
		t.Fatal(err)
	}
	f := newFake(func(_, prompt string) string {
		switch {
		case strings.Contains(prompt, "Recent activity:"):
			return "Stop and give up; route to fail."
		case strings.HasPrefix(prompt, "[coach]"):
			return "HANG" // a steer rides the running turn
		}
		return "SLOW:VERDICT: FAIL"
	})
	r := newRun(t, p, f)
	st, _ := r.Run(context.Background())
	if st.Status != StatusPassed {
		t.Fatalf("status = %s", st.Status)
	}
	res, _ := os.ReadFile(filepath.Join(r.Dir(), "steps/1-coder/result.json"))
	if !strings.Contains(string(res), `"route": "tests"`) {
		t.Errorf("result = %s", res)
	}
}

func contains(xs []string, x string) bool {
	for _, v := range xs {
		if v == x {
			return true
		}
	}
	return false
}
