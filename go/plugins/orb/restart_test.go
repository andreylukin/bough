package orb

import (
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/tools"
)

// fakeNotices is the job-notices service: what the restarter reports
// through, and the jobs it counts as stopped.
type fakeNotices struct {
	mu      sync.Mutex
	texts   []string
	running []tools.Running
}

func (n *fakeNotices) Notify(text string) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.texts = append(n.texts, text)
}

func (n *fakeNotices) Running() []tools.Running {
	n.mu.Lock()
	defer n.mu.Unlock()
	return append([]tools.Running(nil), n.running...)
}

func (n *fakeNotices) all() []string {
	n.mu.Lock()
	defer n.mu.Unlock()
	return append([]string(nil), n.texts...)
}

func (n *fakeNotices) last() string {
	all := n.all()
	if len(all) == 0 {
		return ""
	}
	return all[len(all)-1]
}

func gitRepo(t *testing.T) string {
	t.Helper()
	repo := filepath.Join(t.TempDir(), "app")
	if err := os.MkdirAll(repo, 0o755); err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(repo, "README"), []byte("hi\n"), 0o644)
	for _, args := range [][]string{{"init", "-q", "-b", "main"}, {"add", "."}, {"commit", "-qm", "init"}} {
		c := exec.Command("git", args...)
		c.Dir = repo
		c.Env = append(os.Environ(), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t")
		if out, err := c.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	return repo
}

type restartEnv struct {
	home, repo, slug string
	rt               container.Runtime
	fake             *container.Fake
	h                *handle
	n                *fakeNotices
}

func (e *restartEnv) yaml(t *testing.T, extra string) {
	t.Helper()
	if err := projectdef.WriteFile(e.home, e.slug, projectdef.FileYAML, "repos:\n  - path: "+e.repo+"\n    branch: main\n"+extra); err != nil {
		t.Fatal(err)
	}
}

func (e *restartEnv) calls() []string { return e.fake.CallList() }

// newRestartEnv is a handle over an orb opened on a fake runtime, with a
// restarter whose poll and settle are short. rt wraps fake when a test
// needs to hold or fail a runtime call; failOpen fails the first start.
func newRestartEnv(t *testing.T, slug string, wrap func(*container.Fake) container.Runtime, failOpen bool) *restartEnv {
	t.Helper()
	e := &restartEnv{home: t.TempDir(), repo: gitRepo(t), slug: slug, fake: container.NewFake(), n: &fakeNotices{}}
	e.rt = container.Runtime(e.fake)
	if wrap != nil {
		e.rt = wrap(e.fake)
	}
	if _, err := projectdef.Create(e.home, slug); err != nil {
		t.Fatal(err)
	}
	e.yaml(t, "")
	if err := projectdef.WriteFile(e.home, slug, projectdef.FileResume, "#!/bin/sh\necho resumed-ok\n"); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(e.home, slug)
	if err != nil {
		t.Fatal(err)
	}
	if failOpen {
		e.fake.FailBuild = errors.New("setup.sh exit 1")
	}
	scratch := t.TempDir()
	o, err := iorb.Open(context.Background(), e.rt, e.home, "s1", p, scratch)
	e.h = newHandle(e.home, "s1", slug, e.rt, scratch, func() {})
	e.h.settle(o, err)
	if (err != nil) != failOpen {
		t.Fatalf("open: %v", err)
	}
	kctx := kernel.NewContext()
	kctx.Provide("job-notices", e.n)
	e.h.rs = newRestarter(e.h, kctx)
	e.h.rs.poll, e.h.rs.settle = 10*time.Millisecond, 10*time.Millisecond
	e.h.rs.prep = func(ctx context.Context, rt container.Runtime, home, session string, p projectdef.Project, scratch string) (*iorb.Orb, error) {
		return iorb.Prepare(ctx, rt, home, session, p, scratch)
	}
	e.h.rs.setNotify(e.n.Notify)
	e.h.rs.start()
	t.Cleanup(e.h.close)
	return e
}

// An idle session's restart swaps the orb behind the same handle: the
// next command runs in the new container, the notice names the image and
// carries resume.log, and a job from before counts as stopped by the orb.
func TestRestartSwapsHandle(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "swap", nil, false)
	old := e.h.Orb()
	beforeSwap := time.Now()
	e.yaml(t, "memory: 8G\n")
	if out := e.h.Restart(false, "person"); !strings.Contains(out, "scheduled") {
		t.Fatalf("restart = %q", out)
	}
	waitFor(t, "the swap", func() bool { return e.h.Orb() != old && e.n.last() != "" })
	if err := e.h.Command(context.Background(), "true").Run(); err != nil {
		t.Fatalf("command after the swap: %v", err)
	}
	if e.fake.LastRun.Memory != "8G" {
		t.Fatalf("new container spec %+v", e.fake.LastRun)
	}
	calls := e.calls()
	if last := calls[len(calls)-1]; last != "exec "+container.OrbName("s1")+" true" {
		t.Fatalf("last call %q", last)
	}
	notice := e.n.last()
	if !strings.Contains(notice, "orb restarted for project swap") || !strings.Contains(notice, e.h.Orb().State().Image) ||
		!strings.Contains(notice, "recreated (memory)") || !strings.Contains(notice, "resume.log:") || !strings.Contains(notice, "resumed-ok") {
		t.Fatalf("notice = %q", notice)
	}
	if !e.h.StoppedSince(beforeSwap) {
		t.Fatal("a job started before the swap is not marked stopped by the orb")
	}
	if e.h.StoppedSince(time.Now().Add(time.Second)) {
		t.Fatal("a job started after the swap is marked stopped")
	}
	if st, _ := iorb.ReadState(e.home, "s1"); st.Restart != "" || st.Status != iorb.StatusRunning {
		t.Fatalf("state after = %+v", st)
	}
}

// Asked during a turn (the agent's own bash), the build runs at once but
// the swap waits for "done"; a subagent finishing is not the turn ending.
func TestRestartDeferredToTurnEnd(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "defer", nil, false)
	old := e.h.Orb()
	e.h.rs.observe("call")
	before := len(e.calls())
	e.yaml(t, "memory: 4G\n")
	if out := e.h.Restart(false, "agent"); !strings.Contains(out, "when the current turn ends") {
		t.Fatalf("restart = %q", out)
	}
	waitFor(t, "the build", func() bool {
		st, _ := iorb.ReadState(e.home, "s1")
		return len(e.calls()) > before && st.Restart == iorb.RestartPending && count(e.calls()[before:], "image-exists ") > 0
	})
	if line := e.h.Line(); !strings.Contains(line, "restart pending") {
		t.Errorf("bar line = %q", line)
	}
	time.Sleep(100 * time.Millisecond)
	if n := count(e.calls()[before:], "stop ") + count(e.calls()[before:], "remove "); n != 0 || e.h.Orb() != old {
		t.Fatalf("swapped mid-turn: %v", e.calls()[before:])
	}
	e.h.rs.observe("sub:done")
	e.h.rs.observe("title")
	time.Sleep(100 * time.Millisecond)
	if e.h.Orb() != old {
		t.Fatal("a subagent's done swapped the orb")
	}
	e.h.rs.observe("done")
	waitFor(t, "the swap at turn end", func() bool { return e.h.Orb() != old })
}

func count(calls []string, prefix string) int {
	n := 0
	for _, c := range calls {
		if strings.HasPrefix(c, prefix) {
			n++
		}
	}
	return n
}

// A build that fails keeps the old orb and says why, with the log.
func TestRestartBuildFailureKeepsOrb(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "bfail", nil, false)
	old := e.h.Orb()
	projectdef.WriteFile(e.home, "bfail", projectdef.FileSetup, "#!/bin/sh\nexit 1\n")
	e.fake.FailBuild, e.fake.FailBuildLog = errors.New("step failed"), "E: unable to locate package nope\n"
	e.h.Restart(false, "person")
	waitFor(t, "the failure notice", func() bool { return e.n.last() != "" })
	notice := e.n.last()
	if !strings.Contains(notice, "failed to build its image") || !strings.Contains(notice, "unable to locate package nope") || !strings.Contains(notice, "old orb keeps running") {
		t.Fatalf("notice = %q", notice)
	}
	if e.h.Orb() != old || count(e.calls(), "stop ") != 0 {
		t.Fatalf("a failed build swapped or stopped: %v", e.calls())
	}
	if st, _ := iorb.ReadState(e.home, "s1"); st.Restart != "" || st.Status != iorb.StatusRunning {
		t.Fatalf("state = %+v", st)
	}
	if err := e.h.Command(context.Background(), "true").Run(); err != nil {
		t.Fatalf("old orb unusable: %v", err)
	}
}

// The notice says which background jobs died with the old container.
func TestRestartReportsStoppedJobs(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "jobs", nil, false)
	e.n.mu.Lock()
	e.n.running = []tools.Running{{ID: 1, Cmd: "npm run dev"}, {ID: 2, Cmd: "tail -f log"}}
	e.n.mu.Unlock()
	out, err := orbCommand(e.h, e.home, e.n.Running)("restart")
	if err != nil || !strings.Contains(out, "2 running job(s) will stop") {
		t.Fatalf("/orb restart = %q, %v", out, err)
	}
	waitFor(t, "the notice", func() bool { return e.n.last() != "" })
	if n := e.n.last(); !strings.Contains(n, "2 background job(s) stopped with the old container: npm run dev, tail -f log") || !strings.Contains(n, "restarted in place") {
		t.Fatalf("notice = %q", n)
	}
}

// Another process (the CLI, the guest's relayed bough, serve) asks
// through restart.json; the session picks it up.
func TestRestartRequestFileWatched(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "file", nil, false)
	old := e.h.Orb()
	if err := iorb.RequestRestart(e.home, "s1", iorb.RestartRequest{Fresh: true, By: "cli"}); err != nil {
		t.Fatal(err)
	}
	waitFor(t, "the swap", func() bool { return e.h.Orb() != old && e.n.last() != "" })
	if n := e.n.last(); !strings.Contains(n, "recreated (fresh)") {
		t.Fatalf("notice = %q", n)
	}
	if _, err := os.Stat(filepath.Join(iorb.Dir(e.home, "s1"), "restart.json")); !os.IsNotExist(err) {
		t.Fatalf("request file left behind: %v", err)
	}
}

// A session whose first start failed gets its orb from a restart once
// the definition is fixed, and the row's section is redone for it.
func TestRestartAfterFailedStart(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "late", nil, true)
	if e.h.Orb() != nil {
		t.Fatal("failed open has an orb")
	}
	var mu sync.Mutex
	var refreshed []bool
	e.h.setRefresh(func(bool) {
		mu.Lock()
		refreshed = append(refreshed, e.h.Orb() != nil)
		mu.Unlock()
	})
	e.fake.FailBuild = nil
	if out := e.h.Restart(false, "person"); !strings.Contains(out, "has no orb") {
		t.Fatalf("restart = %q", out)
	}
	waitFor(t, "the orb", func() bool { return e.h.Orb() != nil && e.n.last() != "" })
	if err := e.h.Ready(context.Background()); err != nil {
		t.Fatalf("Ready after the restart: %v", err)
	}
	if err := e.h.Command(context.Background(), "true").Run(); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(refreshed) != 1 || !refreshed[0] {
		t.Fatalf("refresh calls %v", refreshed)
	}
}

// holdCommit holds image commits while hold is set, so a test can ask
// again while a build runs.
type holdCommit struct {
	*container.Fake
	mu      sync.Mutex
	hold    chan struct{}
	entered chan struct{}
}

func (r *holdCommit) Commit(ctx context.Context, spec container.CommitSpec, log io.Writer) error {
	r.mu.Lock()
	hold, entered := r.hold, r.entered
	r.hold = nil
	r.mu.Unlock()
	if hold != nil {
		close(entered)
		<-hold
	}
	return r.Fake.Commit(ctx, spec, log)
}

// A second request during a build drops the first build's result: one
// swap, to the newer definition.
func TestRestartCoalescesNewerRequest(t *testing.T) {
	t.Parallel()
	var hc *holdCommit
	e := newRestartEnv(t, "coal", func(f *container.Fake) container.Runtime {
		hc = &holdCommit{Fake: f}
		return hc
	}, false)
	old := e.h.Orb()
	hold, entered := make(chan struct{}), make(chan struct{})
	hc.mu.Lock()
	hc.hold, hc.entered = hold, entered
	hc.mu.Unlock()
	projectdef.WriteFile(e.home, "coal", projectdef.FileSetup, "#!/bin/sh\necho v2\n")
	e.h.Restart(false, "person")
	<-entered
	projectdef.WriteFile(e.home, "coal", projectdef.FileSetup, "#!/bin/sh\necho v3\n")
	e.h.Restart(false, "agent")
	close(hold)
	waitFor(t, "the swap", func() bool { return e.h.Orb() != old && e.n.last() != "" })
	time.Sleep(100 * time.Millisecond)
	p, _ := projectdef.Load(e.home, "coal")
	hash, err := projectdef.ImageHash(e.home, p)
	if err != nil {
		t.Fatal(err)
	}
	want := projectdef.ImageTag("coal", hash)
	if n := count(e.calls(), "stop "); n != 1 || e.fake.LastRun.Image != want || len(e.n.all()) != 1 {
		t.Fatalf("stops %d image %s want %s notices %v", n, e.fake.LastRun.Image, want, e.n.all())
	}
}

// failingStop fails Stop while fail is set.
type failingStop struct {
	*container.Fake
	mu   sync.Mutex
	fail error
}

func (r *failingStop) Stop(ctx context.Context, name string) error {
	r.mu.Lock()
	fail := r.fail
	r.mu.Unlock()
	if fail != nil {
		return fail
	}
	return r.Fake.Stop(ctx, name)
}

// A swap whose stop failed stopped nothing: the old jobs still run, and
// one that later fails on its own must not read as stopped by the orb.
func TestRestartStopFailureKeepsJobsFailing(t *testing.T) {
	t.Parallel()
	var fs *failingStop
	e := newRestartEnv(t, "nostop", func(f *container.Fake) container.Runtime {
		fs = &failingStop{Fake: f}
		return fs
	}, false)
	old := e.h.Orb()
	before := time.Now()
	fs.mu.Lock()
	fs.fail = errors.New("engine wedged")
	fs.mu.Unlock()
	e.h.Restart(false, "person")
	waitFor(t, "the failure notice", func() bool { return e.n.last() != "" })
	if n := e.n.last(); !strings.Contains(n, "engine wedged") || !strings.Contains(n, "old orb keeps running") {
		t.Fatalf("notice = %q", n)
	}
	if e.h.Orb() != old {
		t.Fatal("a failed stop swapped the orb")
	}
	if e.h.StoppedSince(before) {
		t.Fatal("a job from before a swap that stopped nothing reads as stopped with the orb")
	}
}

// A turn that has written its "input" but emitted nothing live yet (the
// person sent a message during the build) holds the swap until "done".
func TestRestartWaitsForTurnThatHasNotEmitted(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "quiet", nil, false)
	old := e.h.Orb()
	hist := filepath.Join(t.TempDir(), "s1.jsonl")
	os.WriteFile(hist, []byte(`{"seq":1,"at":"2020-01-01T00:00:00Z","kind":"input","data":{"text":"old session"}}`+"\n"), 0o644)
	e.h.rs.mu.Lock()
	e.h.rs.histPath = hist
	e.h.rs.mu.Unlock()
	// An input from before the handle (a resumed session) is no turn:
	// that request swaps.
	e.h.Restart(false, "person")
	waitFor(t, "the first swap", func() bool { return e.h.Orb() != old && e.n.last() != "" })
	old = e.h.Orb()
	notices := len(e.n.all())

	f, _ := os.OpenFile(hist, os.O_APPEND|os.O_WRONLY, 0o644)
	f.WriteString(`{"seq":2,"at":"` + time.Now().UTC().Format(time.RFC3339Nano) + `","kind":"input","data":{"text":"hi"}}` + "\n")
	f.Close()
	e.h.Restart(false, "person")
	waitFor(t, "the build", func() bool {
		st, _ := iorb.ReadState(e.home, "s1")
		return st.Restart == iorb.RestartPending
	})
	time.Sleep(150 * time.Millisecond)
	if e.h.Orb() != old {
		t.Fatal("swapped while a turn was open by its input")
	}
	e.h.rs.observe("done")
	waitFor(t, "the swap at turn end", func() bool { return e.h.Orb() != old && len(e.n.all()) > notices })
}

// A headless run exits with its turn: a restart asked in it builds and
// swaps nothing, says so, and a fresh one removes the container at exit
// so the next start creates a new one.
func TestRestartHeadlessDefersToNextStart(t *testing.T) {
	t.Parallel()
	e := newRestartEnv(t, "hl", nil, false)
	old := e.h.Orb()
	e.h.rs.mu.Lock()
	e.h.rs.headless = true
	e.h.rs.mu.Unlock()
	before := len(e.calls())
	out := e.h.Restart(true, "agent")
	if !strings.Contains(out, "not applied in this run") || !strings.Contains(out, "creates a new container") {
		t.Fatalf("restart = %q", out)
	}
	time.Sleep(100 * time.Millisecond)
	if e.h.Orb() != old || len(e.calls()) != before {
		t.Fatalf("headless restart did something: %v", e.calls()[before:])
	}
	e.h.close()
	calls := e.calls()[before:]
	if count(calls, "stop ") != 1 || count(calls, "remove "+container.OrbName("s1")) != 1 {
		t.Fatalf("exit calls %v, want a stop then the fresh remove", calls)
	}
}
