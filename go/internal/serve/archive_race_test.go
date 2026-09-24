package serve

import (
	"context"
	"errors"
	"net/http/httptest"
	"path/filepath"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/container"
)

// hookRT is the fake runtime with a hook on Inspect. Kill asks the
// runtime about the killed session's orb after the process is gone and
// before it returns, which is exactly the window the races below need a
// second client to land in.
type hookRT struct {
	*container.Fake
	mu   sync.Mutex
	hook map[string]func() // orb name -> run once on its next Inspect
}

func (r *hookRT) on(name string, f func()) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.hook == nil {
		r.hook = map[string]func(){}
	}
	r.hook[name] = f
}

func (r *hookRT) Inspect(ctx context.Context, name string) (container.State, error) {
	r.mu.Lock()
	f := r.hook[name]
	delete(r.hook, name)
	r.mu.Unlock()
	if f != nil {
		f()
	}
	return r.Fake.Inspect(ctx, name)
}

// newHookFixture is newFixture with the hooked runtime. The supervisor
// is rebuilt on the same home rather than patched: its reaper reads the
// runtime from another goroutine.
func newHookFixture(t *testing.T, extraEnv ...string) (*fixture, *hookRT) {
	t.Helper()
	f := newFixture(t, extraEnv...)
	f.sup.Close()
	rt := &hookRT{Fake: f.rt}
	sup, err := NewSupervisor(Options{
		Runtime:  rt,
		Exe:      f.sup.exe,
		HistDir:  f.hist,
		MetaPath: filepath.Join(f.home, ".bough", "serve", "meta.json"),
		Env:      f.sup.opt.Env,
		Buffer:   50,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	f.sup = sup
	return f, rt
}

// specs/archive_unarchive.fizz, ArchivedHasNoLiveChild: a Send from
// another client (a second tab, the CLI) that lands while archiving
// kills the child must not spawn a new one for a session that is about
// to read as archived.
func TestArchiveRefusesASendWhileTheChildDies(t *testing.T) {
	t.Parallel()
	f, rt := newHookFixture(t)
	f.seed(t, "s")
	if err := f.sup.Adopt("s"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the child", func() bool { return f.sup.Live("s") })
	var sendErr error
	rt.on(container.OrbName("s"), func() { sendErr = f.sup.Send("s", "hello") })
	if err := f.sup.SetArchived("s", true); err != nil {
		t.Fatal(err)
	}
	if f.sup.Meta("s").Archived && f.sup.Live("s") {
		t.Fatalf("archived with a live child: a Send in the window respawned it (send: %v)", sendErr)
	}
	if !errors.Is(sendErr, ErrArchived) {
		t.Errorf("send while archiving = %v, want ErrArchived", sendErr)
	}
}

// ensure is the one place a session's child is spawned, so it is where
// "archived means no child" holds whatever raced the flag.
func TestEnsureNeverSpawnsAnArchivedSession(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "s")
	if err := f.sup.SetArchived("s", true); err != nil {
		t.Fatal(err)
	}
	if _, err := f.sup.ensure("s"); !errors.Is(err, ErrArchived) {
		t.Errorf("ensure on an archived session = %v, want ErrArchived", err)
	}
	if f.sup.Live("s") {
		t.Error("ensure spawned a child for an archived session")
	}
}

// specs/archive_unarchive.fizz, "Stop and archive": the parent is still
// live while its children are ended, so an agent it spawns then (its
// tools.spawn request landing mid-archive) was never in the list being
// stopped and kept running under an archived parent.
func TestStopAndArchiveLeavesNoAgentTheParentSpawnedMidway(t *testing.T) {
	t.Parallel()
	f, rt := newHookFixture(t, envTurns+"=1")
	api := httptest.NewServer(NewAPI(f.sup))
	t.Cleanup(api.Close)
	af := &apiFixture{fixture: f, srv: api}
	f.seed(t, "parent")
	if err := f.sup.Adopt("parent"); err != nil {
		t.Fatal(err)
	}
	first, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: "parent"}, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "parent and agent live", func() bool { return f.sup.Live("parent") && f.sup.Live(first) })
	var late string
	var lateErr error
	rt.on(container.OrbName(first), func() {
		late, _, lateErr = f.sup.CreateChild(CreateOptions{Prompt: "HANG", SpawnedBy: "parent"}, 0, 0)
	})
	if code, body := af.do(t, "POST", "/api/sessions/parent/archive", `{"stopChildren":true}`); code != 200 {
		t.Fatalf("archive = %d %v", code, body)
	}
	for _, c := range f.sup.Children("parent") {
		if c.Queued || f.sup.Live(c.ID) {
			t.Errorf("agent %s left running by Stop and archive (first %s, spawned midway %q, err %v)", c.ID, first, late, lateErr)
		}
	}
	if late != "" || !errors.Is(lateErr, ErrArchived) {
		t.Errorf("spawn from a parent being archived = %q %v, want refused with ErrArchived", late, lateErr)
	}
}
