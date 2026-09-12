package watch_test

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/serve/watch"
	"github.com/andreylukin/bough/plugins/codemode"
)

// fakeExec returns canned stdout, one entry per call, then repeats the
// last one.
type fakeExec struct {
	outs []string
	err  error
	n    int
}

func (f *fakeExec) Run(ctx context.Context, cmd string) (string, error) {
	if f.err != nil {
		return "", f.err
	}
	out := f.outs[min(f.n, len(f.outs)-1)]
	f.n++
	return out, nil
}

type fakeWake struct {
	texts []string
	err   error
}

func (f *fakeWake) Wake(session, text string) error {
	if f.err != nil {
		return f.err
	}
	f.texts = append(f.texts, text)
	return nil
}

type fakeIdle struct{ busy bool }

func (f *fakeIdle) Idle(string) bool { return !f.busy }

// harness writes one watcher file and wires an engine over it with a
// frozen clock the test advances by hand.
func harness(t *testing.T, body string, ex watch.Execer) (*watch.Engine, *fakeWake, *fakeIdle, *time.Time) {
	t.Helper()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "ci.js"), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	clock := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	w, idle := &fakeWake{}, &fakeIdle{}
	e := &watch.Engine{
		Dir: dir, Session: "s1",
		Eval: codemode.New(2 * time.Second),
		Exec: ex, Wake: w, Busy: idle,
		Now:    func() time.Time { return clock },
		MinGap: time.Minute,
	}
	return e, w, idle, &clock
}

const ciWatcher = `
if (event.phase === "config") return { every: "60s", run: "gh pr checks" }
if (event.prev && event.prev.state === "PENDING" && event.now.state !== "PENDING")
  return { wake: "CI finished: " + event.now.state }
return {}
`

func TestBootstrapFailureIsReported(t *testing.T) {
	t.Parallel()
	e, _, _, _ := harness(t, `if (event.phase === "config") return { every: "banana", run: "x" }`, &fakeExec{outs: []string{""}})
	if err := e.Load(context.Background()); err != nil {
		t.Fatal(err)
	}
	st := e.Status()
	if len(st) != 1 || !st[0].Failing {
		t.Fatalf("want one failing watcher, got %+v", st)
	}
	if st[0].Error == "" || st[0].Name != "ci.js" {
		t.Fatalf("want the parse error named, got %+v", st[0])
	}
}

func TestTransitionFiresOnceAndDedupes(t *testing.T) {
	t.Parallel()
	ex := &fakeExec{outs: []string{`{"state":"PENDING"}`, `{"state":"SUCCESS"}`, `{"state":"SUCCESS"}`}}
	e, wake, _, clock := harness(t, ciWatcher, ex)
	if err := e.Load(context.Background()); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 3; i++ {
		e.Tick(context.Background())
		*clock = clock.Add(2 * time.Minute) // past both the interval and MinGap
	}
	if len(wake.texts) != 1 || wake.texts[0] != "CI finished: SUCCESS" {
		t.Fatalf("want exactly one wake, got %q", wake.texts)
	}
	if st := e.Status(); st[0].LastWoke == nil || st[0].LastRun == nil {
		t.Fatalf("want lastRun and lastWoke set, got %+v", st[0])
	}
}

func TestRateLimitSuppressesAFlap(t *testing.T) {
	t.Parallel()
	// alternates state every poll, and wakes on every change
	body := `
if (event.phase === "config") return { every: "1s", run: "flap" }
if (event.prev && event.prev !== event.now) return { wake: "now " + event.now }
return {}
`
	ex := &fakeExec{outs: []string{"a", "b", "a", "b", "a"}}
	e, wake, _, clock := harness(t, body, ex)
	if err := e.Load(context.Background()); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 5; i++ {
		e.Tick(context.Background())
		*clock = clock.Add(2 * time.Second) // interval elapses, MinGap (1m) does not
	}
	if len(wake.texts) != 1 {
		t.Fatalf("want one wake through the floor, got %q", wake.texts)
	}
}

func TestBusySessionQueuesThenReceives(t *testing.T) {
	t.Parallel()
	ex := &fakeExec{outs: []string{`{"state":"PENDING"}`, `{"state":"SUCCESS"}`}}
	e, wake, idle, clock := harness(t, ciWatcher, ex)
	if err := e.Load(context.Background()); err != nil {
		t.Fatal(err)
	}
	idle.busy = true
	e.Tick(context.Background())
	*clock = clock.Add(2 * time.Minute)
	e.Tick(context.Background())
	if len(wake.texts) != 0 {
		t.Fatalf("busy session was interrupted: %q", wake.texts)
	}
	idle.busy = false
	e.Tick(context.Background())
	if len(wake.texts) != 1 {
		t.Fatalf("want the queued wake delivered once idle, got %q", wake.texts)
	}
}

func TestFailingCommandSurfaces(t *testing.T) {
	t.Parallel()
	e, _, _, _ := harness(t, ciWatcher, &fakeExec{err: errors.New("gh: not found")})
	if err := e.Load(context.Background()); err != nil {
		t.Fatal(err)
	}
	e.Tick(context.Background())
	st := e.Status()
	if !st[0].Failing || st[0].Error != "gh: not found" {
		t.Fatalf("want the command error surfaced, got %+v", st[0])
	}
	if st[0].Every != "1m0s" {
		t.Fatalf("want the bootstrapped interval kept, got %q", st[0].Every)
	}
}

func TestRunRefusesNonLoopback(t *testing.T) {
	t.Parallel()
	if err := watch.CheckLoopback("0.0.0.0:7683"); !errors.Is(err, watch.ErrNotLoopback) {
		t.Fatalf("want a refusal for 0.0.0.0, got %v", err)
	}
	for _, addr := range []string{"127.0.0.1:7683", "localhost:7683", "[::1]:7683"} {
		if err := watch.CheckLoopback(addr); err != nil {
			t.Fatalf("%s: %v", addr, err)
		}
	}
	e := &watch.Engine{}
	if err := e.Run(context.Background(), "0.0.0.0:7683", time.Second); !errors.Is(err, watch.ErrNotLoopback) {
		t.Fatalf("Run started off loopback: %v", err)
	}
}

func TestDedupeSuppressesARepeatedText(t *testing.T) {
	t.Parallel()
	// wakes with the same text on every single poll
	body := `
if (event.phase === "config") return { every: "1s", run: "x" }
return { wake: "same news" }
`
	e, wake, _, clock := harness(t, body, &fakeExec{outs: []string{"x"}})
	if err := e.Load(context.Background()); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 4; i++ {
		e.Tick(context.Background())
		*clock = clock.Add(2 * time.Minute) // past the interval and the floor
	}
	if len(wake.texts) != 1 {
		t.Fatalf("want the repeat deduped, got %q", wake.texts)
	}
}
