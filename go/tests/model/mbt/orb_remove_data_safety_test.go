//go:build !windows

package mbt

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/plugins/history"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/orb_remove_data_safety.fizz against what a remove deletes: the
// web's GET .../orb/remove and DELETE .../orb on serve's real API, and
// `bough project rm|prune` through the library calls the CLI makes.
//
// serve runs in process on container.Fake, like orb_lifecycle_test.go:
// a `bough serve` process opens the host's container runtime, which this
// suite may never touch. The session's child is played the same way: a
// real process owns the orb (a stub child serve spawns, or a plain
// process serve did not), and the adapter writes state.json as
// orb.Prepare, prepareMounts and Orb.Stop would for it. The one worktree
// is a real `git worktree` of a real repo, on the real bough/<session>
// branch, so PlanRemove, Remove and `git branch -D` act on real git.
//
// The CLI is package main, and it opens container.Default, so it is not
// run as a process: CliRm, CliPrune, CliYes and CliRmYes call what
// projectRm and projectPrune call (orb.PlanRemove with --branches, then
// orb.RemovePlanned on the same plan after the prompt), and check the
// CLI's own two filters (its kill(pid, 0) owner check, prune's dead
// statuses) against the spec's requires.
//
// ServeRestart is serve's Supervisor.Close and a new Supervisor and API
// over the same home, behind the same URL.
//
// Every field is read off the ground truth (the orb dir, state.json, the
// fake runtime, the owner process, git), except what only a client
// holds (flow and the CLI's plan) and the ghosts, which record how the
// last remove went: the adapter derives them from the state before and
// after the request, the same way the spec's remove does.

// ordsFields is the Orb role's state.
type ordsFields struct {
	orb, file, branch, owner, flow string
	lost, orphaned, blost          string
	tracked, dirty, inYml, ctr     bool
	pdirty, pdel, killed, reported bool
}

func (o ordsFields) state() map[string]any {
	return map[string]any{
		"orb": o.orb, "file": o.file, "tracked": o.tracked, "dirty": o.dirty, "branch": o.branch,
		"inYml": o.inYml, "ctr": o.ctr, "owner": o.owner, "flow": o.flow, "pdirty": o.pdirty, "pdel": o.pdel,
		"lost": o.lost, "orphaned": o.orphaned, "blost": o.blost, "killed": o.killed, "reported": o.reported,
	}
}

// ordsRuntime is container.Fake whose Remove fails while failRemove is
// set: the RuntimeRemoveFails variants. An atomic rather than the fake's
// FailRemove field, because the request that reads it runs in serve's
// handler goroutine.
type ordsRuntime struct {
	*container.Fake
	failRemove atomic.Bool
}

// Only a container that is there can fail to go: every real runtime's
// Remove answers nil for a missing one before it runs anything.
func (r *ordsRuntime) Remove(ctx context.Context, name string) error {
	if st, err := r.Fake.Inspect(ctx, name); err == nil && st != container.StateMissing && r.failRemove.Load() {
		return errors.New("container: model: remove failed")
	}
	return r.Fake.Remove(ctx, name)
}

const ordsRepo = "app"

type ordsAdapter struct {
	t    *testing.T
	root string
	home string
	hist string
	pids string
	src  string // the project's one repo; every walk's branch lives here
	exe  string
	rt   *ordsRuntime
	sup  *serve.Supervisor
	api  atomic.Pointer[serve.API]
	srv  *httptest.Server
	// started is at or after the current supervisor's start: a state.json
	// written later is one serve believes.
	started time.Time

	n    int
	id   string
	slug string
	wt   string

	cli      *exec.Cmd // the owner when it is another bough
	cliWrote time.Time // UpdatedAt of its last state.json write

	flow                  string
	plan                  *orb.RemovePlan // the CLI's, while its prompt waits
	lost, orphaned, blost string
	killed, reported      bool

	gate  gate
	steps []tracecheck.Step
	walks [][]tracecheck.Step

	// cliWritesNothing is the deliberate bug the wrong-adapter test
	// injects: CliWrites does not write state.json, so serve keeps
	// reading the live CLI as dead.
	cliWritesNothing bool

	// seen is what the last GetState observed, until the next action: its
	// gate reads it rather than asking git again (a walk step's cost is
	// its git calls, and the transitions cover is 15k steps).
	seen *ordsFields
}

func newOrdsAdapter(t *testing.T) *ordsAdapter {
	root, err := os.MkdirTemp("", "bords-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(root) })
	root, _ = filepath.EvalSymlinks(root)
	a := &ordsAdapter{t: t, root: root, home: filepath.Join(root, "home"), pids: filepath.Join(root, "pids"),
		src: filepath.Join(root, "src", ordsRepo), rt: &ordsRuntime{Fake: container.NewFake()}}
	a.hist = filepath.Join(a.home, ".bough", "history")
	for _, d := range []string{a.hist, a.pids} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	a.exe = filepath.Join(root, "child.sh")
	if err := os.WriteFile(a.exe, []byte(orbChild), 0o755); err != nil {
		t.Fatal(err)
	}
	a.rt.AddImage("img")
	// -b main and an identity of its own: nothing here may depend on the
	// host's git config.
	if err := ordsGit(a.src, "init", "-q", "-b", "main"); err != nil {
		t.Fatal(err)
	}
	if err := ordsGit(a.src, "commit", "-q", "--allow-empty", "-m", "base"); err != nil {
		t.Fatal(err)
	}
	if err := a.startServe(); err != nil {
		t.Fatal(err)
	}
	a.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		a.api.Load().ServeHTTP(w, r)
	}))
	t.Cleanup(func() {
		a.srv.Close()
		a.sup.Close()
		a.endCLI(syscall.SIGKILL)
	})
	return a
}

// startServe is serve starting: a supervisor and its API over the home.
func (a *ordsAdapter) startServe() error {
	sup, err := serve.NewSupervisor(serve.Options{
		Exe:      a.exe,
		HistDir:  a.hist,
		MetaPath: filepath.Join(a.home, ".bough", "serve", "meta.json"),
		Env:      []string{"HOME=" + a.home, "PATH=" + os.Getenv("PATH"), "MODEL_PIDS=" + a.pids},
		Runtime:  a.rt,
	})
	if err != nil {
		return err
	}
	a.sup, a.started = sup, time.Now()
	a.api.Store(serve.NewAPI(sup))
	return nil
}

// ordsGit runs git with an identity and no signing of its own.
func ordsGit(dir string, args ...string) error {
	_, err := ordsGitOut(dir, args...)
	return err
}

func ordsGitOut(dir string, args ...string) (string, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	full := append([]string{"-C", dir, "-c", "user.name=model", "-c", "user.email=model@example.invalid", "-c", "commit.gpgsign=false"}, args...)
	out, err := exec.Command("git", full...).CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("git %v: %w: %s", args, err, out)
	}
	return strings.TrimSpace(string(out)), nil
}

func (a *ordsAdapter) branchName() string { return "bough/" + a.id }
func (a *ordsAdapter) ctr() string        { return container.OrbName(a.id) }
func (a *ordsAdapter) orbDir() string     { return orb.Dir(a.home, a.id) }

func (a *ordsAdapter) writeYml(withRepo bool) error {
	y := "name: " + a.slug + "\nrepos:\n"
	if withRepo {
		y += "  - path: " + a.src + "\n    name: " + ordsRepo + "\n"
	} else {
		y = "name: " + a.slug + "\nrepos: []\n"
	}
	return writeFileAtomic(filepath.Join(projectdef.Root(a.home), a.slug, projectdef.FileYAML), []byte(y))
}

// Init is the spec's: a session that ran and exited cleanly. Its orb dir
// holds a clean worktree on bough/<session> at the base, state.json says
// stopped and names it, the container is there and stopped.
func (a *ordsAdapter) Init() error {
	a.seen = nil
	a.n++
	a.id, a.slug = fmt.Sprintf("ords%04d", a.n), fmt.Sprintf("q%04d", a.n)
	a.wt = filepath.Join(a.orbDir(), ordsRepo)
	a.cli, a.cliWrote, a.flow, a.plan = nil, time.Time{}, "idle", nil
	a.lost, a.orphaned, a.blost, a.killed, a.reported = "", "", "", false, false
	a.rt.failRemove.Store(false)
	a.gate.reset()
	if err := a.writeYml(true); err != nil {
		return err
	}
	if err := a.appendHistory("meta", map[string]any{"cwd": a.home, "mode": "project", "project": a.slug}); err != nil {
		return err
	}
	if err := a.addWorktree(); err != nil {
		return err
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.rt.Start(ctx, container.RunSpec{Name: a.ctr(), Image: "img"}); err != nil {
		return err
	}
	if err := a.rt.Stop(ctx, a.ctr()); err != nil {
		return err
	}
	st := orb.State{Session: a.id, Project: a.slug, Status: orb.StatusStopped, Container: a.ctr(),
		Worktrees: map[string]string{ordsRepo: a.wt}, Primary: a.wt, UpdatedAt: time.Now().UTC()}
	if err := a.writeState(st); err != nil {
		return err
	}
	st2, err := a.GetState()
	if err != nil {
		return err
	}
	a.steps = []tracecheck.Step{{Action: "Init", State: ordsQualify(st2)}}
	return nil
}

// Cleanup ends the walk's owner and deletes what the walk left, so the
// repo's refs and worktree list stay small over thousands of walks.
func (a *ordsAdapter) Cleanup() error {
	a.seen = nil
	if len(a.steps) > 0 {
		a.walks = append(a.walks, a.steps)
		a.steps = nil
	}
	a.endCLI(syscall.SIGKILL)
	if err := a.sup.Kill(a.id); err != nil {
		return err
	}
	a.rt.failRemove.Store(false)
	ctx, cancel := actionCtx()
	defer cancel()
	if err := orb.Remove(ctx, a.rt, a.home, a.id); err != nil {
		return err
	}
	ordsGit(a.src, "worktree", "prune")
	ordsGit(a.src, "branch", "-D", a.branchName())
	ordsGit(a.src, "update-ref", "-d", "refs/remotes/origin/"+a.branchName())
	return nil
}

func (a *ordsAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Orb", Index: 0}: a}, nil
}

func (a *ordsAdapter) GetState() (map[string]any, error) {
	o, err := a.observe()
	if err != nil {
		return nil, err
	}
	a.seen = &o
	return o.state(), nil
}

func ordsQualify(st map[string]any) map[string]any {
	out := map[string]any{}
	for k, v := range st {
		out["Orb#0."+k] = v
	}
	return out
}

// observe reads the role's fields off the ground truth.
func (a *ordsAdapter) observe() (ordsFields, error) {
	o := ordsFields{flow: a.flow, lost: a.lost, orphaned: a.orphaned, blost: a.blost, killed: a.killed, reported: a.reported}
	if a.plan != nil {
		o.pdirty = len(a.plan.Dirty) > 0
		for _, b := range a.plan.Branches {
			o.pdel = o.pdel || b.Delete
		}
	}
	switch {
	case a.sup.Live(a.id):
		if a.cli != nil {
			return o, errors.New("both serve's child and another bough own the orb")
		}
		o.owner = "serve"
	case a.cli != nil && a.cliWrote.After(a.started):
		o.owner = "cli"
	case a.cli != nil:
		o.owner = "clistale"
	default:
		o.owner = "none"
	}
	ctx, cancel := actionCtx()
	defer cancel()
	cs, err := a.rt.Inspect(ctx, a.ctr())
	if err != nil {
		return o, err
	}
	o.ctr = cs != container.StateMissing
	_, derr := os.Stat(a.orbDir())
	switch {
	case derr == nil:
		o.orb = "present"
	case o.ctr:
		o.orb = "orphan"
	default:
		o.orb = "none"
	}
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return o, err
	}
	switch {
	case st.Session == "":
		o.file = "none"
	case st.Status == orb.StatusStarting && st.Phase == orb.PhaseSync:
		o.file = "sync"
	case st.Status == orb.StatusRunning && o.owner == "none":
		// A running orb whose owner is gone without Orb.Stop (serve's
		// Close SIGKILLs its children) is stopped to everything that
		// reads it: serve shows it stopped (orbState), and prune keeps it
		// as resumable either way. The spec does not tell them apart.
		o.file = "stopped"
	case st.Status == orb.StatusRunning, st.Status == orb.StatusFailed, st.Status == orb.StatusStopped:
		o.file = string(st.Status)
	default:
		return o, fmt.Errorf("state.json status %q phase %q is not one the spec has", st.Status, st.Phase)
	}
	o.tracked = st.Worktrees[ordsRepo] == a.wt
	if _, err := os.Stat(a.wt); err == nil {
		out, err := ordsGitOut(a.wt, "status", "--porcelain")
		if err != nil {
			return o, err
		}
		o.dirty = out != ""
	}
	o.branch, err = a.branchState()
	if err != nil {
		return o, err
	}
	p, err := projectdef.Load(a.home, a.slug)
	if err != nil {
		return o, err
	}
	o.inYml = len(p.Def.Repos) > 0
	return o, nil
}

// branchState is bough/<session> as PlanRemove judges it: merged when an
// ancestor of the base or contained in a remote-tracking ref.
//
// One git call on the common path (the walks run it thousands of times):
// the base is main, so "an ancestor of the base" is main containing it.
func (a *ordsAdapter) branchState() (string, error) {
	out, err := ordsGitOut(a.src, "for-each-ref", "--contains", "refs/heads/"+a.branchName(), "--format=%(refname)", "refs/heads/main", "refs/remotes")
	if err != nil {
		if _, rerr := ordsGitOut(a.src, "rev-parse", "--verify", "--quiet", "refs/heads/"+a.branchName()); rerr != nil {
			return "deleted", nil
		}
		return "", err
	}
	if out != "" {
		return "merged", nil
	}
	return "unmerged", nil
}

// step opens an action: the gate on the spec's require, read off the
// adapter's view.
func (a *ordsAdapter) step(require func(ordsFields) bool) (ordsFields, bool, error) {
	if a.seen != nil {
		o := *a.seen
		a.seen = nil
		return o, a.gate.pass(require(o)), nil
	}
	o, err := a.observe()
	if err != nil {
		return o, false, err
	}
	return o, a.gate.pass(require(o)), nil
}

// --- the owner ---

func (a *ordsAdapter) writeState(st orb.State) error {
	b, err := json.Marshal(st)
	if err != nil {
		return err
	}
	return writeFileAtomic(filepath.Join(a.orbDir(), "state.json"), b)
}

// ownerPID is the pid of the process that owns the orb.
func (a *ordsAdapter) ownerPID() (int, error) {
	if a.cli != nil {
		return a.cli.Process.Pid, nil
	}
	b, err := os.ReadFile(filepath.Join(a.pids, a.id))
	if err != nil {
		return 0, err
	}
	var pid int
	_, err = fmt.Sscan(string(b), &pid)
	return pid, err
}

// write is one state.json write by the owner: edit applies to what is on
// disk, the pid is the owner's and updatedAt moves.
func (a *ordsAdapter) write(edit func(*orb.State)) error {
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return err
	}
	pid, err := a.ownerPID()
	if err != nil {
		return err
	}
	edit(&st)
	st.PID = pid
	st.UpdatedAt = time.Now().UTC()
	if err := a.writeState(st); err != nil {
		return err
	}
	if a.cli != nil {
		a.cliWrote = st.UpdatedAt
	}
	return nil
}

// addWorktree is orb's addWorktree: keep one that is there, else add the
// branch if it exists, else a new branch at the base.
func (a *ordsAdapter) addWorktree() error {
	if _, err := os.Stat(filepath.Join(a.wt, ".git")); err == nil {
		return nil
	}
	ordsGit(a.src, "worktree", "prune")
	if _, err := ordsGitOut(a.src, "rev-parse", "--verify", "--quiet", "refs/heads/"+a.branchName()); err == nil {
		return ordsGit(a.src, "worktree", "add", "--quiet", a.wt, a.branchName())
	}
	return ordsGit(a.src, "worktree", "add", "--quiet", "-b", a.branchName(), a.wt, "HEAD")
}

// prepare is Prepare's first write, starting(sync) with no Worktrees and
// the new owner's pid, into the orb dir it makes if a remove took it.
func (a *ordsAdapter) prepare(o ordsFields) error {
	if o.orb != "present" {
		a.reported, a.lost, a.orphaned, a.blost = false, "", "", ""
	}
	a.killed = false
	if err := os.MkdirAll(a.orbDir(), 0o755); err != nil {
		return err
	}
	return a.write(func(s *orb.State) {
		*s = orb.State{Session: a.id, Project: a.slug, Container: a.ctr(), Status: orb.StatusStarting, Phase: orb.PhaseSync}
	})
}

func (a *ordsAdapter) ResumeByServe() error {
	o, ok, err := a.step(func(o ordsFields) bool { return o.owner == "none" })
	if !ok || err != nil {
		return err
	}
	pidfile := filepath.Join(a.pids, a.id)
	os.Remove(pidfile)
	if err := a.sup.Adopt(a.id); err != nil {
		return err
	}
	if err := waitUntil("the session child's pid", func() bool { _, err := os.Stat(pidfile); return err == nil }); err != nil {
		return err
	}
	return a.prepare(o)
}

func (a *ordsAdapter) ResumeByCli() error {
	o, ok, err := a.step(func(o ordsFields) bool { return o.owner == "none" })
	if !ok || err != nil {
		return err
	}
	a.cli = exec.Command("sleep", "3600")
	if err := a.cli.Start(); err != nil {
		a.cli = nil
		return err
	}
	return a.prepare(o)
}

// SyncOk is prepareMounts: a worktree per repo in project.yml, named in
// state.json, and the start after it (the container runs).
func (a *ordsAdapter) SyncOk() error {
	o, ok, err := a.step(func(o ordsFields) bool { return o.owner != "none" && o.file == "sync" })
	if !ok || err != nil {
		return err
	}
	wts := map[string]string{}
	if o.inYml {
		if err := a.addWorktree(); err != nil {
			return err
		}
		wts[ordsRepo] = a.wt
	}
	ctx, cancel := actionCtx()
	defer cancel()
	if err := a.rt.Start(ctx, container.RunSpec{Name: a.ctr(), Image: "img"}); err != nil {
		return err
	}
	return a.write(func(s *orb.State) {
		s.Status, s.Phase, s.Worktrees, s.Primary = orb.StatusRunning, orb.PhaseReady, wts, wts[ordsRepo]
	})
}

func (a *ordsAdapter) PrepareFailedAtSync() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.owner != "none" && o.file == "sync" })
	if !ok || err != nil {
		return err
	}
	return a.write(func(s *orb.State) { s.Status, s.Error = orb.StatusFailed, "model: sync failed" })
}

func (a *ordsAdapter) AgentEdits() error {
	_, ok, err := a.step(func(o ordsFields) bool {
		return o.owner != "none" && o.file == "running" && o.tracked && !o.dirty
	})
	if !ok || err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(a.wt, fmt.Sprintf("edit-%d.txt", time.Now().UnixNano())), []byte("work\n"), 0o644)
}

func (a *ordsAdapter) AgentCommits() error {
	_, ok, err := a.step(func(o ordsFields) bool {
		return o.owner != "none" && o.file == "running" && o.tracked && o.dirty
	})
	if !ok || err != nil {
		return err
	}
	if err := ordsGit(a.wt, "add", "-A"); err != nil {
		return err
	}
	return ordsGit(a.wt, "commit", "-q", "-m", "work")
}

// AgentPushes is the branch reaching a remote: a remote-tracking ref at
// its tip, which is what PlanRemove asks git about.
func (a *ordsAdapter) AgentPushes() error {
	_, ok, err := a.step(func(o ordsFields) bool {
		return o.owner != "none" && o.file == "running" && o.branch == "unmerged"
	})
	if !ok || err != nil {
		return err
	}
	return ordsGit(a.src, "update-ref", "refs/remotes/origin/"+a.branchName(), a.branchName())
}

// OwnerGone: a running session exits cleanly (Orb.Stop stops the
// container and writes stopped); one in its sync or failed start just
// dies, leaving state.json as it was.
func (a *ordsAdapter) OwnerGone() error {
	o, ok, err := a.step(func(o ordsFields) bool { return o.owner != "none" })
	if !ok || err != nil {
		return err
	}
	sig := syscall.SIGKILL
	if o.file == "running" {
		ctx, cancel := actionCtx()
		defer cancel()
		if err := a.rt.Stop(ctx, a.ctr()); err != nil {
			return err
		}
		if err := a.write(func(s *orb.State) { s.Status = orb.StatusStopped }); err != nil {
			return err
		}
		sig = syscall.SIGTERM
	}
	if a.cli != nil {
		a.endCLI(sig)
		return nil
	}
	pid, err := a.ownerPID()
	if err != nil {
		return err
	}
	if err := syscall.Kill(pid, sig); err != nil {
		return err
	}
	return waitUntil("serve's child to be reaped", func() bool { return !a.sup.Live(a.id) })
}

// CliWrites is the live CLI writing state.json again (an exec that
// restarts the container, a portal): the status it had, a new updatedAt.
func (a *ordsAdapter) CliWrites() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.owner == "clistale" && o.orb == "present" })
	if !ok || err != nil {
		return err
	}
	if a.cliWritesNothing {
		return nil
	}
	return a.write(func(*orb.State) {})
}

func (a *ordsAdapter) RepoRemovedFromProjectYml() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.inYml && o.orb == "present" })
	if !ok || err != nil {
		return err
	}
	return a.writeYml(false)
}

// ServeRestart closes the supervisor (it SIGKILLs its children) and
// starts a new one over the same home, behind the same URL.
func (a *ordsAdapter) ServeRestart() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.owner == "serve" || o.owner == "cli" })
	if !ok || err != nil {
		return err
	}
	if err := a.sup.Close(); err != nil {
		return err
	}
	// The new serve starts strictly after every write so far.
	time.Sleep(2 * time.Millisecond)
	return a.startServe()
}

// --- the web ---

func (a *ordsAdapter) call(method, path string) (int, error) {
	ctx, cancel := actionCtx()
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, method, a.srv.URL+path, nil)
	if err != nil {
		return 0, err
	}
	resp, err := a.srv.Client().Do(req)
	if err != nil {
		return 0, err
	}
	resp.Body.Close()
	return resp.StatusCode, nil
}

func (a *ordsAdapter) orbPath() string { return "/api/sessions/" + url.PathEscape(a.id) + "/orb" }

// OpenRemovePlan is the confirm's GET. ?branches=1: the spec takes the
// worst case, a client of the API that prunes branches (the page itself
// sends no query).
func (a *ordsAdapter) OpenRemovePlan() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.orb == "present" && o.flow == "idle" })
	if !ok || err != nil {
		return err
	}
	code, err := a.call(http.MethodGet, a.orbPath()+"/remove?branches=1")
	if err != nil {
		return err
	}
	if code != http.StatusOK {
		return fmt.Errorf("GET %s/remove = %d, want 200", a.orbPath(), code)
	}
	a.flow, a.killed, a.reported = "web", false, false
	return nil
}

func (a *ordsAdapter) Cancel() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.flow == "web" })
	if !ok || err != nil {
		return err
	}
	a.flow = "idle"
	return nil
}

func (a *ordsAdapter) Confirm() error                   { return a.confirm(false) }
func (a *ordsAdapter) ConfirmRuntimeRemoveFails() error { return a.confirm(true) }

// confirm is the DELETE. What it did is read off the ground truth
// before and after: whether serve's child was ended, whether the orb
// went, and what went with it.
func (a *ordsAdapter) confirm(rtfail bool) error {
	pre, ok, err := a.step(func(o ordsFields) bool { return o.flow == "web" && (o.ctr || !rtfail) })
	if !ok || err != nil {
		return err
	}
	a.flow = "idle"
	a.rt.failRemove.Store(rtfail)
	code, err := a.call(http.MethodDelete, a.orbPath()+"?branches=1")
	a.rt.failRemove.Store(false)
	if err != nil {
		return err
	}
	switch code {
	case http.StatusOK, http.StatusConflict, http.StatusInternalServerError:
	default:
		return fmt.Errorf("DELETE %s = %d", a.orbPath(), code)
	}
	post, err := a.observe()
	if err != nil {
		return err
	}
	if post.orb == "present" {
		a.killed = pre.owner == "serve" && post.owner == "none"
		return nil
	}
	a.killed = false
	a.reported = code != http.StatusOK
	// serve ends its own child before it removes; the owner at delete
	// time is anyone else's process.
	owner := pre.owner
	if owner == "serve" {
		owner = "none"
	}
	a.judge(pre, post, owner, false)
	return nil
}

// judge records how a remove that went through went wrong, the spec's
// remove: stale is a CLI plan made before its prompt.
func (a *ordsAdapter) judge(pre, post ordsFields, owner string, stale bool) {
	age := "fresh"
	if stale {
		age = "stale"
	}
	if pre.dirty {
		if !pre.tracked {
			a.lost = "untracked"
		} else {
			a.lost = age
		}
	}
	if owner != "none" {
		if owner == "clistale" && !stale {
			a.orphaned = "restart"
		} else {
			a.orphaned = age
		}
	}
	if pre.branch == "unmerged" && post.branch == "deleted" {
		a.blost = age
	}
}

// --- the CLI: bough project rm|prune <session> --branches ---

// cliOwnerAlive is projectRm's check: kill(pid, 0) on state.json's pid.
func (a *ordsAdapter) cliOwnerAlive() (bool, error) {
	st, err := orb.ReadState(a.home, a.id)
	if err != nil {
		return false, err
	}
	return st.PID > 0 && syscall.Kill(st.PID, 0) == nil, nil
}

func (a *ordsAdapter) cliPlan(prune bool) error {
	alive, err := a.cliOwnerAlive()
	if err != nil {
		return err
	}
	if alive {
		return errors.New("the CLI refuses: state.json's pid is alive, but the spec's owner is none")
	}
	if prune {
		st, err := orb.ReadState(a.home, a.id)
		if err != nil {
			return err
		}
		if st.Status != orb.StatusFailed && st.Status != orb.StatusStarting && st.Status != orb.StatusBuilding {
			return fmt.Errorf("prune keeps an orb with status %q as resumable", st.Status)
		}
	}
	ctx, cancel := actionCtx()
	defer cancel()
	plan, err := orb.PlanRemove(ctx, a.rt, a.home, a.id, true)
	if err != nil {
		return err
	}
	a.plan, a.flow, a.killed, a.reported = &plan, "cli", false, false
	return nil
}

func (a *ordsAdapter) CliRm() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.orb == "present" && o.flow == "idle" && o.owner == "none" })
	if !ok || err != nil {
		return err
	}
	return a.cliPlan(false)
}

func (a *ordsAdapter) CliPrune() error {
	_, ok, err := a.step(func(o ordsFields) bool {
		return o.orb == "present" && o.flow == "idle" && o.owner == "none" && (o.file == "sync" || o.file == "failed")
	})
	if !ok || err != nil {
		return err
	}
	return a.cliPlan(true)
}

func (a *ordsAdapter) CliYes() error                   { return a.cliYes(false) }
func (a *ordsAdapter) CliYesRuntimeRemoveFails() error { return a.cliYes(true) }

func (a *ordsAdapter) cliYes(rtfail bool) error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.flow == "cli" && (o.ctr || !rtfail) })
	if !ok || err != nil {
		return err
	}
	return a.cliDelete(rtfail)
}

// cliDelete is RemovePlanned on the plan printed before the prompt.
func (a *ordsAdapter) cliDelete(rtfail bool) error {
	pre, err := a.observe()
	if err != nil {
		return err
	}
	plan := *a.plan
	a.plan, a.flow = nil, "idle"
	ctx, cancel := actionCtx()
	defer cancel()
	a.rt.failRemove.Store(rtfail)
	rerr := orb.RemovePlanned(ctx, a.rt, a.home, plan)
	a.rt.failRemove.Store(false)
	post, err := a.observe()
	if err != nil {
		return err
	}
	if post.orb == "present" {
		return nil
	}
	a.reported = rerr != nil
	a.judge(pre, post, pre.owner, true)
	return nil
}

func (a *ordsAdapter) CliNo() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.flow == "cli" })
	if !ok || err != nil {
		return err
	}
	a.plan, a.flow = nil, "idle"
	return nil
}

// CliRmYes is `--yes`: the plan and the remove with no prompt between.
func (a *ordsAdapter) CliRmYes() error {
	_, ok, err := a.step(func(o ordsFields) bool { return o.orb == "present" && o.flow == "idle" && o.owner == "none" })
	if !ok || err != nil {
		return err
	}
	if err := a.cliPlan(false); err != nil {
		return err
	}
	return a.cliDelete(false)
}

func (a *ordsAdapter) endCLI(sig syscall.Signal) {
	if a.cli == nil {
		return
	}
	a.cli.Process.Signal(sig)
	a.cli.Wait()
	a.cli = nil
}

func (a *ordsAdapter) appendHistory(kind string, data map[string]any) error {
	path := filepath.Join(a.hist, a.id+".jsonl")
	entries, _ := history.Read(path)
	b, _ := json.Marshal(history.Entry{Seq: int64(len(entries) + 1), At: time.Now(), Kind: kind, Data: data})
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.Write(append(b, '\n'))
	return err
}

// ordsRecorded runs an action and, when it acted, journals the step and
// the state after it for the walk's trace check.
func ordsRecorded(name string, f func(*ordsAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) {
		a := m.(*ordsAdapter)
		if err := f(a); err != nil || a.gate.off {
			return nil, err
		}
		st, err := a.GetState()
		if err != nil {
			return nil, err
		}
		a.steps = append(a.steps, tracecheck.Step{Action: "Orb#0." + name, State: ordsQualify(st)})
		return nil, nil
	}
}

var ordsActions = map[string]map[string]fmbt.ActionFunc{"Orb": {
	"ResumeByServe":             ordsRecorded("ResumeByServe", (*ordsAdapter).ResumeByServe),
	"ResumeByCli":               ordsRecorded("ResumeByCli", (*ordsAdapter).ResumeByCli),
	"SyncOk":                    ordsRecorded("SyncOk", (*ordsAdapter).SyncOk),
	"PrepareFailedAtSync":       ordsRecorded("PrepareFailedAtSync", (*ordsAdapter).PrepareFailedAtSync),
	"AgentEdits":                ordsRecorded("AgentEdits", (*ordsAdapter).AgentEdits),
	"AgentCommits":              ordsRecorded("AgentCommits", (*ordsAdapter).AgentCommits),
	"AgentPushes":               ordsRecorded("AgentPushes", (*ordsAdapter).AgentPushes),
	"OwnerGone":                 ordsRecorded("OwnerGone", (*ordsAdapter).OwnerGone),
	"CliWrites":                 ordsRecorded("CliWrites", (*ordsAdapter).CliWrites),
	"RepoRemovedFromProjectYml": ordsRecorded("RepoRemovedFromProjectYml", (*ordsAdapter).RepoRemovedFromProjectYml),
	"ServeRestart":              ordsRecorded("ServeRestart", (*ordsAdapter).ServeRestart),
	"OpenRemovePlan":            ordsRecorded("OpenRemovePlan", (*ordsAdapter).OpenRemovePlan),
	"Cancel":                    ordsRecorded("Cancel", (*ordsAdapter).Cancel),
	"Confirm":                   ordsRecorded("Confirm", (*ordsAdapter).Confirm),
	"ConfirmRuntimeRemoveFails": ordsRecorded("ConfirmRuntimeRemoveFails", (*ordsAdapter).ConfirmRuntimeRemoveFails),
	"CliRm":                     ordsRecorded("CliRm", (*ordsAdapter).CliRm),
	"CliPrune":                  ordsRecorded("CliPrune", (*ordsAdapter).CliPrune),
	"CliYes":                    ordsRecorded("CliYes", (*ordsAdapter).CliYes),
	"CliYesRuntimeRemoveFails":  ordsRecorded("CliYesRuntimeRemoveFails", (*ordsAdapter).CliYesRuntimeRemoveFails),
	"CliNo":                     ordsRecorded("CliNo", (*ordsAdapter).CliNo),
	"CliRmYes":                  ordsRecorded("CliRmYes", (*ordsAdapter).CliRmYes),
}}

// ordsStateDiff compares the spec's role fields with the adapter's.
func ordsStateDiff(want, got map[string]any) string {
	var diffs []string
	for k, w := range want {
		if !strings.HasPrefix(k, "Orb#0.") {
			continue // "orb": the role reference itself
		}
		wj, _ := json.Marshal(w)
		gj, _ := json.Marshal(got[k])
		if string(wj) != string(gj) {
			diffs = append(diffs, fmt.Sprintf("%s: spec %s, server %s", strings.TrimPrefix(k, "Orb#0."), wj, gj))
		}
	}
	return strings.Join(diffs, "; ")
}

// ordsWalks are the walks over the checked-in graph under cover.
func ordsWalks(t *testing.T, cover tracecheck.Cover) [][]tracecheck.Step {
	t.Helper()
	b, err := pathsJSONCover("orb_remove_data_safety", cover)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	if len(doc.Paths) == 0 {
		t.Fatal("no walks over testdata/orb_remove_data_safety")
	}
	var out [][]tracecheck.Step
	for _, p := range doc.Paths {
		out = append(out, p.Trace)
	}
	return out
}

// walkOrds drives the adapter down each walk and returns the first step
// whose state is not the spec's.
func walkOrds(a *ordsAdapter, walks [][]tracecheck.Step) (steps int, err error) {
	for pi, tr := range walks {
		if err := a.Init(); err != nil {
			return steps, fmt.Errorf("walk %d: Init: %w", pi, err)
		}
		got := a.steps[0].State
		for si, s := range tr {
			if si > 0 {
				name := strings.TrimPrefix(s.Action, "Orb#0.")
				f, ok := ordsActions["Orb"][name]
				if !ok {
					return steps, fmt.Errorf("walk %d step %d: no adapter action %q", pi, si, s.Action)
				}
				if _, err := f(a, nil); err != nil {
					return steps, fmt.Errorf("walk %d step %d (%s): %w\nwalk so far: %s", pi, si, s.Action, err, ordsTraceNames(tr[:si+1]))
				}
				if a.gate.off {
					o, _ := a.observe()
					return steps, fmt.Errorf("walk %d step %d (%s): the adapter's view says it is not enabled in %v\nwalk so far: %s", pi, si, s.Action, o.state(), ordsTraceNames(tr[:si+1]))
				}
				steps++
				// ordsRecorded journaled the state after the step.
				got = a.steps[len(a.steps)-1].State
			}
			if diff := ordsStateDiff(s.State, got); diff != "" {
				return steps, fmt.Errorf("walk %d step %d (%s): %s\nwalk so far: %s", pi, si, s.Action, diff, ordsTraceNames(tr[:si+1]))
			}
		}
		if err := a.Cleanup(); err != nil {
			return steps, fmt.Errorf("walk %d: Cleanup: %w", pi, err)
		}
	}
	return steps, nil
}

func ordsTraceNames(tr []tracecheck.Step) string {
	var names []string
	for _, s := range tr {
		names = append(names, strings.TrimPrefix(s.Action, "Orb#0."))
	}
	return strings.Join(names, ", ")
}

// ordsShards is how many serves the path walk runs side by side: a step
// is a few git calls and file writes, and one serve took 6.5 minutes
// over the states cover alone.
const ordsShards = 4

// Every settled state (every link under MODEL_COVER=transitions) against
// serve and the orb library, the walks dealt round-robin to ordsShards
// serves; then each walk's own journal is replayed on the graph.
func TestOrbRemoveDataSafetyPaths(t *testing.T) {
	t.Parallel()
	walks := ordsWalks(t, envCover())
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("orb_remove_data_safety")), "..", "testdata", "orb_remove_data_safety"))
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("%d walks (%s cover)", len(walks), envCover())
	for i := range ordsShards {
		t.Run(fmt.Sprint(i), func(t *testing.T) {
			t.Parallel()
			var mine [][]tracecheck.Step
			for j := i; j < len(walks); j += ordsShards {
				mine = append(mine, walks[j])
			}
			a := newOrdsAdapter(t)
			steps, err := walkOrds(a, mine)
			if err != nil {
				t.Fatal(err)
			}
			checkOrdsWalks(t, g, a.walks)
			t.Logf("%d walks, %d steps against serve", len(mine), steps)
		})
	}
}

// checkOrdsWalks replays the walks the adapter journaled on g.
func checkOrdsWalks(t *testing.T, g *tracecheck.Graph, walks [][]tracecheck.Step) {
	t.Helper()
	steps := 0
	for i, w := range walks {
		if v := g.Check(w); v != nil {
			t.Errorf("walk %d is not a path in the model: %v\ntrace: %s", i, v, ordsTraceNames(w))
			return
		}
		steps += len(w) - 1
	}
	if steps == 0 {
		t.Fatal("no walk took an enabled step")
	}
}

// The trace check must be able to fail: a journal whose remove kept
// the orb it reported deleted is not a path in the model.
func TestOrbRemoveDataSafetyTraceRejects(t *testing.T) {
	t.Parallel()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("orb_remove_data_safety")), "..", "testdata", "orb_remove_data_safety"))
	if err != nil {
		t.Fatal(err)
	}
	b, err := pathsJSON("orb_remove_data_safety")
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Paths []struct {
			Trace []tracecheck.Step `json:"trace"`
		} `json:"paths"`
	}
	if err := json.Unmarshal(b, &doc); err != nil {
		t.Fatal(err)
	}
	for _, p := range doc.Paths {
		for i, s := range p.Trace {
			if s.Action != "Orb#0.CliRmYes" || s.State["Orb#0.orb"] != "none" {
				continue
			}
			tr := append([]tracecheck.Step(nil), p.Trace[:i+1]...)
			bad := map[string]any{}
			for k, v := range s.State {
				bad[k] = v
			}
			bad["Orb#0.orb"] = "present"
			tr[i] = tracecheck.Step{Action: s.Action, State: bad}
			if g.Check(p.Trace[:i+1]) != nil {
				t.Fatal("the generated walk itself is not a path")
			}
			v := g.Check(tr)
			if v == nil {
				t.Fatal("a trace whose CliRmYes kept the orb passed the check")
			}
			t.Logf("rejected, as it must be: %v", v)
			return
		}
	}
	t.Fatal("no walk takes CliRmYes to an orb-less state")
}

// A wrong adapter must fail the walk: CliWrites that writes nothing
// leaves serve reading the live CLI as dead. Only the CliWrites links
// show it, so this walks every link.
func TestOrbRemoveDataSafetyPathsCatchWrongAdapter(t *testing.T) {
	t.Parallel()
	a := newOrdsAdapter(t)
	a.cliWritesNothing = true
	_, err := walkOrds(a, ordsWalks(t, tracecheck.CoverTransitions))
	if err == nil {
		t.Fatal("walks whose CliWrites writes nothing passed; the walk is not checking state")
	}
	t.Logf("caught, as it must be: %.300s", err)
}

// The runner's random walks (the exhaustive run only, see runMBT), and
// each walk's journal replayed on the graph.
func TestOrbRemoveDataSafety(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newOrdsAdapter(t)
	if err := runMBT(t, "orb_remove_data_safety", a, ordsActions,
		map[string]any{"max-seq-runs": 200, "max-actions": 12, "max-parallel-runs": 0}); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
	g, err := tracecheck.Load(fizzCheck(t, "orb_remove_data_safety"))
	if err != nil {
		t.Fatal(err)
	}
	checkOrdsWalks(t, g, a.walks)
}
