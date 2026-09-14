package serve

import (
	"bufio"
	"context"
	"io"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/plugins/history"
)

// reopen closes the fixture's supervisor and starts a new one on the
// same meta.json and history, the way a serve restart does.
func reopen(t *testing.T, f *fixture) {
	t.Helper()
	opt := f.sup.opt
	f.sup.Close()
	sup, err := NewSupervisor(opt)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	f.sup = sup
}

// A restarted serve that re-adopts a reported child does not report
// its old turn again when that process exits.
func TestChildReportSurvivesRestart(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	waitNotices(t, f, "parent", 1)
	reopen(t, f)
	if err := f.sup.Adopt(id); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "adopted", func() bool { return f.sup.childPID(id) != 0 })
	if err := f.sup.Kill(id); err != nil {
		t.Fatal(err)
	}
	if got := waitNotices(t, f, "parent", 1); len(got) != 1 {
		t.Fatalf("notices after restart = %q, want the one report", got)
	}
}

// A child queued when serve stopped starts after the restart and
// reports, instead of being a ghost that counts against the budget.
func TestChildQueueSurvivesRestart(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG a", SpawnedBy: "parent"}, 0, 1); err != nil {
		t.Fatal(err)
	}
	b, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "b", SpawnedBy: "parent"}, 0, 1)
	if err != nil || !queued {
		t.Fatalf("b = %v %v", queued, err)
	}
	reopen(t, f)
	waitFor(t, "b's report", func() bool {
		for _, n := range notices(t, f, "parent") {
			if strings.Contains(n, b+" finished") {
				return true
			}
		}
		return false
	})
	if m := f.sup.Meta(b); m.Task != nil || m.Queued {
		t.Fatalf("b's meta still queued: %+v", m)
	}
	if ids := f.sup.queuedIDs(); len(ids) != 0 {
		t.Fatalf("queue = %v", ids)
	}
}

// A notice to a lease still starting waits for its stdin rather than
// falling back to the file the new process has already read.
func TestNotifyWaitsForStartingChild(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.seed(t, "p")
	ch := newChild("p")
	f.sup.mu.Lock()
	f.sup.kids["p"] = ch
	f.sup.mu.Unlock()
	errc := make(chan error, 1)
	go func() { errc <- f.sup.Notify("p", "late report") }()
	time.Sleep(50 * time.Millisecond)
	pr, pw := io.Pipe()
	ch.cmd, ch.stdin = exec.Command("true"), pw // as start does, before ready closes
	close(ch.ready)
	line, err := bufio.NewReader(pr).ReadString('\n')
	if err != nil || line != `{"notice":"late report"}`+"\n" {
		t.Fatalf("stdin got %q %v", line, err)
	}
	if err := <-errc; err != nil {
		t.Fatal(err)
	}
	if got := notices(t, f, "p"); len(got) != 0 {
		t.Fatalf("notice also stored: %q", got)
	}
	f.sup.mu.Lock()
	delete(f.sup.kids, "p")
	f.sup.mu.Unlock()
	close(ch.done)
}

// A stop that lands while a background agent is still booting kills it
// before it reads its task, instead of a no-op interrupt.
func TestStopBootingChildNeverRunsTask(t *testing.T) {
	t.Parallel()
	f := childFixture(t)
	q := queuedChild{id: history.NewID(), dir: f.home, prompt: "hello"}
	q.extra = []string{"BOUGH_SESSION_ID=" + q.id, "BOUGH_SPAWNED_BY=parent"}
	ch := newChild(q.id)
	ch.booting = true
	f.sup.mu.Lock()
	f.sup.meta[q.id] = SessionMeta{SpawnedBy: "parent"}
	f.sup.running[q.id] = true
	f.sup.kids[q.id] = ch
	f.sup.mu.Unlock()
	if was, err := f.sup.stopChild(q.id); err != nil || was != "running" {
		t.Fatalf("stopChild = %q %v", was, err)
	}
	if err := f.sup.launch(ch, q); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "child gone", func() bool { return !f.sup.Live(q.id) })
	es, _ := history.ReadFile(f.hist + "/" + q.id + ".jsonl")
	for _, e := range es {
		if e.Kind == "input" {
			t.Fatalf("stopped child ran its task: %+v", e)
		}
	}
}

// Archiving with stopChildren ends every child process, idle ones too,
// and stops a project child's orb.
func TestArchiveEndsIdleAndRunningChildren(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envTurns+"=1")
	f.seed(t, "parent")
	_, body := f.do(t, "POST", "/api/sessions", `{"prompt":"hello","spawnedBy":"parent"}`)
	idle := rowOf(t, body)["id"].(string)
	waitNotices(t, f.fixture, "parent", 1)
	_, body = f.do(t, "POST", "/api/sessions", `{"prompt":"HANG","spawnedBy":"parent"}`)
	hang := rowOf(t, body)["id"].(string)
	waitFor(t, "both live", func() bool { return f.sup.Live(idle) && f.sup.Live(hang) })

	// A project child whose process is gone but whose orb still runs.
	orbKid := history.NewID()
	f.seed(t, orbKid, history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": f.home, "mode": "project", "project": "p", "spawned_by": "parent"}})
	f.sup.mu.Lock()
	f.sup.meta[orbKid] = SessionMeta{SpawnedBy: "parent"}
	f.sup.mu.Unlock()
	ctx := context.Background()
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName(orbKid), Image: "img"}); err != nil {
		t.Fatal(err)
	}

	if code, body := f.do(t, "POST", "/api/sessions/parent/archive", `{"stopChildren":true}`); code != 200 {
		t.Fatalf("archive = %d %v", code, body)
	}
	if f.sup.Live(idle) || f.sup.Live(hang) {
		t.Fatalf("children still live: idle=%v hang=%v", f.sup.Live(idle), f.sup.Live(hang))
	}
	if st, err := f.rt.Inspect(ctx, container.OrbName(orbKid)); err != nil || st == container.StateRunning {
		t.Fatalf("orb = %v %v, want stopped", st, err)
	}
}
