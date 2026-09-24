package serve

import (
	"encoding/json"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// A background agent whose start goes wrong after serve accepted it
// (tests/model/specs/background_agent_launch_failure.fizz).

// launchFixture is childFixture on a supervisor that execs a link of
// the test binary, so a test can take the executable away, and whose
// children park before their history while hang exists.
func launchFixture(t *testing.T) (f *fixture, exe, hang string) {
	t.Helper()
	dir := t.TempDir()
	hang = filepath.Join(dir, "hang")
	f = newFixture(t, envTurns+"=1", envHangBoot+"="+hang)
	f.seed(t, "parent")
	exe = filepath.Join(dir, "bough-link")
	if err := os.Symlink(f.sup.exe, exe); err != nil {
		t.Fatal(err)
	}
	opt := f.sup.opt
	opt.Exe = exe
	f.sup.Close()
	sup, err := NewSupervisor(opt)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	f.sup = sup
	return f, exe, hang
}

func touch(t *testing.T, path string) {
	t.Helper()
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		t.Fatal(err)
	}
}

// noticeFrom waits for the parent's notice about id that says word.
func noticeFrom(t *testing.T, f *fixture, id, word string) {
	t.Helper()
	waitFor(t, id+" "+word, func() bool {
		for _, n := range notices(t, f, "parent") {
			if strings.Contains(n, id+" "+word) {
				return true
			}
		}
		return false
	})
}

func childStatus(f *fixture, id string) Status {
	for _, c := range f.sup.Children("parent") {
		if c.ID == id {
			return c.Status
		}
	}
	return ""
}

// A spawn whose launch fails is answered with the error and leaves
// nothing: no row listed running forever, no budget spent.
func TestChildLaunchFailureLeavesNothing(t *testing.T) {
	t.Parallel()
	f, exe, _ := launchFixture(t)
	if err := os.Remove(exe); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		if _, _, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 0); err == nil {
			t.Fatal("a spawn with no executable succeeded")
		}
	}
	if r, q, total := f.sup.agentCounts("parent"); r+q+total != 0 {
		t.Fatalf("counts = %d %d %d after two failed spawns, want none", r, q, total)
	}
	if kids := f.sup.Children("parent"); len(kids) != 0 {
		t.Fatalf("children = %+v", kids)
	}
}

// A queued child whose launch fails when its slot frees is reported as
// failed, and its row says so, instead of an "error" event nobody reads.
func TestQueuedChildLaunchFailureIsReported(t *testing.T) {
	t.Parallel()
	f, exe, _ := launchFixture(t)
	a, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG a", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	b, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "b", SpawnedBy: "parent"}, 0, 1)
	if err != nil || !queued {
		t.Fatalf("b = %v %v", queued, err)
	}
	if err := os.Remove(exe); err != nil {
		t.Fatal(err)
	}
	if err := f.sup.Send(a, "go"); err != nil {
		t.Fatal(err)
	}
	noticeFrom(t, f, b, "failed")
	if st := childStatus(f, b); st != StatusError {
		t.Fatalf("b's status = %q, want error", st)
	}
	if m := f.sup.Meta(b); m.Task != nil {
		t.Fatalf("b keeps a task a restart would start: %+v", m)
	}
	waitFor(t, "no slot held", func() bool { r, q, _ := f.sup.agentCounts("parent"); return r == 0 && q == 0 })
}

// A child that dies before it writes any history could not start: its
// parent is told, its row says failed, its slot goes back.
func TestChildDyingBeforeHistoryIsReported(t *testing.T) {
	t.Parallel()
	f, _, hang := launchFixture(t)
	touch(t, hang)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "a live child", func() bool { return f.sup.childPID(id) != 0 })
	if m := f.sup.Meta(id); m.Task == nil {
		t.Fatalf("a booting child has no task on disk, so a restart now would lose it: %+v", m)
	}
	if err := syscall.Kill(f.sup.childPID(id), syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	noticeFrom(t, f, id, "failed")
	if st := childStatus(f, id); st != StatusError {
		t.Fatalf("status = %q, want error", st)
	}
	waitFor(t, "slot freed", func() bool { r, _, _ := f.sup.agentCounts("parent"); return r == 0 })
	if m := f.sup.Meta(id); m.Task != nil {
		t.Fatalf("a dead child keeps its task: %+v", m)
	}
	if got := waitNotices(t, f, "parent", 1); len(got) != 1 {
		t.Fatalf("notices = %q, want one", got)
	}
}

// Stop on a child still booting ends it at once and reports it stopped.
func TestStopBootingChildIsReported(t *testing.T) {
	t.Parallel()
	f, _, hang := launchFixture(t)
	touch(t, hang)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "hello", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "a live child", func() bool { return f.sup.childPID(id) != 0 })
	if was, err := f.sup.stopChild(id); err != nil || was != "running" {
		t.Fatalf("stopChild = %q %v", was, err)
	}
	noticeFrom(t, f, id, "stopped")
	if st := childStatus(f, id); st != StatusStopped {
		t.Fatalf("status = %q, want stopped", st)
	}
	if f.sup.Live(id) {
		t.Fatal("the stopped child is still live")
	}
}

// A child popped off the queue keeps its task until its turn is under
// way, so a restart while it boots starts it again rather than losing
// it; once it ran, a restart never starts it twice.
func TestBootingChildSurvivesRestart(t *testing.T) {
	t.Parallel()
	f, _, hang := launchFixture(t)
	a, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG a", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	b, _, err := f.sup.CreateChild(CreateOptions{Prompt: "b", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "a's task cleared once it runs", func() bool { return f.sup.Meta(a).Task == nil })
	touch(t, hang)
	if err := f.sup.Send(a, "go"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "b booting", func() bool {
		starts, _ := os.ReadFile(f.starts)
		return strings.Contains(string(starts), b)
	})
	if m := f.sup.Meta(b); m.Task == nil {
		t.Fatalf("b lost its task on the way out of the queue: %+v", m)
	}
	reopen(t, f)
	os.Remove(hang)
	noticeFrom(t, f, b, "finished")
	starts, _ := os.ReadFile(f.starts)
	if n := strings.Count(string(starts), a); n != 1 {
		t.Fatalf("a started %d times, want once", n)
	}
	if n := strings.Count(string(starts), b); n != 2 {
		t.Fatalf("b started %d times, want twice (before and after the restart); starts:\n%s\nnotices %q", n, starts, notices(t, f, "parent"))
	}
}

// The running cap a child was queued under holds after a restart: the
// restarted serve had only its default (16) until the next spawn, and
// started every requeued child at once.
func TestRestartKeepsRunningCap(t *testing.T) {
	t.Parallel()
	f, _, hang := launchFixture(t)
	touch(t, hang)
	a, _, err := f.sup.CreateChild(CreateOptions{Prompt: "a", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	b, queued, err := f.sup.CreateChild(CreateOptions{Prompt: "b", SpawnedBy: "parent"}, 0, 1)
	if err != nil || !queued {
		t.Fatalf("b = %v %v", queued, err)
	}
	waitFor(t, "a booting", func() bool {
		starts, _ := os.ReadFile(f.starts)
		return strings.Contains(string(starts), a)
	})
	reopen(t, f)
	waitFor(t, "a started again", func() bool {
		starts, _ := os.ReadFile(f.starts)
		return strings.Count(string(starts), a) == 2
	})
	time.Sleep(200 * time.Millisecond) // a second start would have landed
	if starts, _ := os.ReadFile(f.starts); strings.Contains(string(starts), b) {
		t.Fatalf("b started past a running cap of 1 after the restart:\n%s", starts)
	}
	if ids := f.sup.queuedIDs(); len(ids) != 1 || ids[0] != b {
		t.Fatalf("queue = %v, want [%s]", ids, b)
	}
}

// A queued thread with no prompt started as an empty room: nothing of it
// is left to start, and its idle process ending is no failed start.
func TestQueuedEmptyThreadEndingIsNoFailure(t *testing.T) {
	t.Parallel()
	f, _, _ := launchFixture(t)
	a, _, err := f.sup.CreateChild(CreateOptions{Prompt: "HANG a", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	b, queued, err := f.sup.CreateChild(CreateOptions{SpawnedBy: "parent"}, 0, 1)
	if err != nil || !queued {
		t.Fatalf("b = %v %v", queued, err)
	}
	if err := f.sup.Send(a, "go"); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "b started", func() bool { return f.sup.historyExists(b) })
	if m := f.sup.Meta(b); m.Task != nil {
		t.Fatalf("the started empty thread keeps a task: %+v", m)
	}
	if err := f.sup.Kill(b); err != nil {
		t.Fatal(err)
	}
	waitNotices(t, f, "parent", 1) // a's
	for _, n := range notices(t, f, "parent") {
		if strings.Contains(n, b) {
			t.Fatalf("the empty thread was reported: %q", n)
		}
	}
}

// A task still on disk for a child whose history has a turn (serve died
// between the child's first entry and clearing it) is not started again.
func TestRequeueSkipsAChildThatRan(t *testing.T) {
	t.Parallel()
	f, _, _ := launchFixture(t)
	id := history.NewID()
	f.seed(t, id,
		history.Entry{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": f.home, "spawned_by": "parent"}},
		history.Entry{Seq: 2, Kind: "input", Data: map[string]any{"text": "hello"}},
		history.Entry{Seq: 3, Kind: "done"})
	f.sup.mu.Lock()
	f.sup.meta[id] = SessionMeta{SpawnedBy: "parent", Task: &ChildTask{Dir: f.home, Prompt: "hello"}}
	if err := f.sup.saveMetaLocked(); err != nil {
		t.Fatal(err)
	}
	f.sup.mu.Unlock()
	reopen(t, f)
	if ids := f.sup.queuedIDs(); len(ids) != 0 {
		t.Fatalf("queue = %v, want the child that ran left alone", ids)
	}
	if m := f.sup.Meta(id); m.Task != nil || m.Queued {
		t.Fatalf("meta = %+v", m)
	}
}

// A child with no history is still named by its task in /children, the
// Work dialog's rows: while it boots and after it could not start. Its
// row came from queuedRow, which only read a prompt still in the queue,
// so both were listed as an untitled "Background agent".
func TestUnstartedChildKeepsItsTitle(t *testing.T) {
	t.Parallel()
	f, _, hang := launchFixture(t)
	srv := httptest.NewServer(NewAPI(f.sup))
	t.Cleanup(srv.Close)
	row := func(id string) (string, string) {
		resp, err := srv.Client().Get(srv.URL + "/api/sessions/parent/children")
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		var body struct{ Children []Row }
		if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
			t.Fatal(err)
		}
		for _, r := range body.Children {
			if r.ID == id {
				return r.Title, string(r.Status)
			}
		}
		return "", ""
	}
	touch(t, hang)
	id, _, err := f.sup.CreateChild(CreateOptions{Prompt: "fix the parser", SpawnedBy: "parent"}, 0, 1)
	if err != nil {
		t.Fatal(err)
	}
	waitFor(t, "a live child", func() bool { return f.sup.childPID(id) != 0 })
	if title, st := row(id); title != "fix the parser" || st != "running" {
		t.Fatalf("booting child = %q %q, want its task as title, running", title, st)
	}
	if err := syscall.Kill(f.sup.childPID(id), syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	noticeFrom(t, f, id, "failed")
	if title, st := row(id); title != "fix the parser" || st != "error" {
		t.Fatalf("child that could not start = %q %q, want its task as title, error", title, st)
	}
}
