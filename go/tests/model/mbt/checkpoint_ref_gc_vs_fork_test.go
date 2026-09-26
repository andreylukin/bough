//go:build !windows

package mbt

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"

	"github.com/andreylukin/bough/internal/serve"
	"github.com/andreylukin/bough/internal/servetest"
	"github.com/andreylukin/bough/plugins/history"
	control "github.com/andreylukin/bough/tests/model/llm"
	"github.com/andreylukin/bough/tests/model/tracecheck"
)

// specs/checkpoint_ref_gc_vs_fork.fizz against a real serve and real
// git: one session per walk in a fresh checkout, each checkpoint a real
// turn pinned by plugins/history.Checkpoints.Pin, Fork the real
// history.Fork (never mints a ref of its own), ResolveFork the real
// history.Restore against the pinned tree. DropRef and GitGC have no
// product code today (AGENTS.md: "there is no reaper for them") so the
// adapter plays the external actor the spec is about, running real git
// itself; ref and collected are read back from git on every GetState,
// never cached, so a real disagreement between the spec's ordering and
// git's actual behaviour (a ref that outlives update-ref -d, an object
// gc does not prune) fails the walk instead of hiding behind a stub.
type ckgfAdapter struct {
	t    *testing.T
	s    *servetest.Server
	dir  string // llm-control's queue
	gate gate
	n    int // walk counter, for the repo dir and turn names

	repo, id string
	ids      []string
	seq      int64 // the current checkpoint's turn seq
	tree     string
	forks    []string // forked session files not yet resolved or abandoned
	forkN    int
	pending  int
	lost     bool

	turn int // control turn counter, unique across walks (one queue per serve)
	held string

	// gcNoop is TestCheckpointRefGCvsForkCatchesWrongAdapter's bug:
	// GitGC claims success without running git gc, so the object it
	// should have pruned is still there.
	gcNoop bool
}

func newCKGFAdapter(t *testing.T) *ckgfAdapter {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("no git")
	}
	s := servetest.Start(t, servetest.Options{Config: controlConfig})
	return &ckgfAdapter{t: t, s: s, dir: control.Dir(s.Home)}
}

func (a *ckgfAdapter) srcPath() string {
	return filepath.Join(a.s.Home, ".bough", "history", a.id+".jsonl")
}

func (a *ckgfAdapter) refName() string { return history.TurnRef(a.id, a.seq) }

func (a *ckgfAdapter) git(args ...string) error {
	cmd := exec.Command("git", append([]string{"-C", a.repo}, args...)...)
	cmd.Env = append(os.Environ(), "HOME="+a.s.Home, "GIT_CONFIG_NOSYSTEM=1")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git %v: %w\n%s", args, err, out)
	}
	return nil
}

// refExists and objectMissing are read straight off git, never cached:
// the spec's ref and collected fields are exactly these facts.
func (a *ckgfAdapter) refExists() bool {
	cmd := exec.Command("git", "-C", a.repo, "rev-parse", "--verify", "--quiet", a.refName())
	return cmd.Run() == nil
}

func (a *ckgfAdapter) objectMissing() bool {
	cmd := exec.Command("git", "-C", a.repo, "cat-file", "-e", a.tree)
	return cmd.Run() != nil
}

// Init starts each walk on a fresh checkout with one real checkpoint
// already pinned (the spec's Init sets ref = True).
func (a *ckgfAdapter) Init() error {
	if err := a.Cleanup(); err != nil {
		return err
	}
	a.n++
	if a.id != "" {
		ctx, cancel := actionCtx()
		a.s.Archive(ctx, a.id)
		cancel()
	}
	a.repo = filepath.Join(a.s.Root, fmt.Sprintf("ckgf%04d", a.n))
	if err := os.MkdirAll(a.repo, 0o755); err != nil {
		return err
	}
	for _, step := range []func() error{
		func() error { return a.git("init", "-q", "-b", "main") },
		func() error { return os.WriteFile(filepath.Join(a.repo, "README"), []byte("base\n"), 0o644) },
		func() error { return a.git("add", "README") },
		func() error {
			return a.git("-c", "user.name=model", "-c", "user.email=model@test", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base")
		},
	} {
		if err := step(); err != nil {
			return err
		}
	}
	ctx, cancel := actionCtx()
	row, err := a.s.CreateSession(ctx, a.repo, "")
	cancel()
	if err != nil {
		return err
	}
	a.id = row.ID
	a.ids = append(a.ids, row.ID)
	a.forks, a.forkN, a.pending, a.lost = nil, 0, 0, false
	a.gate.reset()
	return a.pinCheckpoint()
}

// Cleanup releases a turn a failed step left held, so the next walk's
// session does not inherit a running child.
func (a *ckgfAdapter) Cleanup() error {
	if a.held == "" {
		return nil
	}
	control.Release(a.t, a.dir, a.held)
	a.held = ""
	if a.id == "" {
		return nil
	}
	_, err := waitRow(a.s, a.id, "the held turn to end", func(r serve.Row) bool { return r.Status != serve.StatusRunning })
	return err
}

// pinCheckpoint runs one real, empty turn through the engine: its
// input entry snapshots the working tree before the turn runs and
// plugins/history.Checkpoints.Pin names that tree under
// refs/bough/turns/<session>/<seq>, exactly as a real turn does.
func (a *ckgfAdapter) pinCheckpoint() error {
	a.turn++
	name := fmt.Sprintf("ckgf%04dt%04d", a.n, a.turn)
	// An untracked file, unique per pin: without it the working tree at
	// turn start is identical to the base commit's tree, so the
	// "checkpoint" is the same object HEAD already keeps reachable and
	// DropRef + GitGC could never make it collectible — a fact about
	// this fixture, not about DropRef or GitGC.
	if err := os.WriteFile(filepath.Join(a.repo, "scratch"), []byte(name+"\n"), 0o644); err != nil {
		return err
	}
	control.Queue(a.t, a.dir, name, control.Turn{Mode: "block", Text: "checkpoint " + name})
	a.held = name
	ctx, cancel := actionCtx()
	err := a.s.Prompt(ctx, a.id, "turn "+name)
	cancel()
	if err != nil {
		return err
	}
	control.WaitTaken(a.t, a.dir, name, actionTimeout)
	if _, err := waitRow(a.s, a.id, "running", func(r serve.Row) bool { return r.Status == serve.StatusRunning }); err != nil {
		return err
	}
	control.Release(a.t, a.dir, name)
	a.held = ""
	if _, err := waitRow(a.s, a.id, "done", func(r serve.Row) bool { return r.Status == serve.StatusDone }); err != nil {
		return err
	}
	entries, err := history.Read(a.srcPath())
	if err != nil {
		return err
	}
	seq, tree, ok := lastCheckpointEntry(entries)
	if !ok {
		return fmt.Errorf("checkpoint_ref_gc_vs_fork: turn %s pinned no checkpoint", name)
	}
	a.seq, a.tree = seq, tree
	return nil
}

// lastCheckpointEntry is the seq and tree of the last "input" entry
// that has one: a turn with nothing to snapshot (no git repo) pins
// nothing, which the caller treats as a wrongly-wired adapter, not a
// state fizzbee-mbt should ever see.
func lastCheckpointEntry(entries []history.Entry) (seq int64, tree string, ok bool) {
	for _, e := range entries {
		if e.Kind != "input" {
			continue
		}
		if t, has := e.Data["checkpoint"].(string); has && t != "" {
			seq, tree, ok = e.Seq, t, true
		}
	}
	return seq, tree, ok
}

func (a *ckgfAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Checkpoint", Index: 0}: a}, nil
}

// GetState reads ref and collected straight off git; pending and lost
// are the adapter's own bookkeeping, the one thing counting the forks
// it made and has not yet resolved or abandoned.
func (a *ckgfAdapter) GetState() (map[string]any, error) {
	return map[string]any{
		"ref":       a.refExists(),
		"pending":   a.pending,
		"collected": a.objectMissing(),
		"lost":      a.lost,
	}, nil
}

// Fork is the real history.Fork: it copies the checkpoint's ancestor
// entries into a new session file and never pins a ref of its own, so
// this only records one more resolve owed against the same ref.
func (a *ckgfAdapter) Fork() error {
	if !a.gate.pass(a.refExists()) {
		return nil
	}
	dst := filepath.Join(filepath.Dir(a.srcPath()), fmt.Sprintf("%s-f%d.jsonl", a.id, a.forkN))
	a.forkN++
	if err := history.Fork(a.srcPath(), a.seq, dst); err != nil {
		return err
	}
	a.forks = append(a.forks, dst)
	a.pending++
	return nil
}

// ResolveFork is the real history.Restore against the forked file's own
// copy of the checkpoint, exactly what its /undo would run. An error
// here (the tree object is gone) is the bug the spec forbids reaching:
// require pending > 0 (a fork not yet resolved) never coincides with a
// real collected object, since DropRef and GitGC both require
// pending == 0 first.
func (a *ckgfAdapter) ResolveFork() error {
	if !a.gate.pass(a.pending > 0) {
		return nil
	}
	dst := a.forks[len(a.forks)-1]
	a.forks = a.forks[:len(a.forks)-1]
	entries, err := history.Read(dst)
	if err != nil {
		return err
	}
	_, tree, ok := lastCheckpointEntry(entries)
	if !ok {
		return fmt.Errorf("checkpoint_ref_gc_vs_fork: forked file %s has no checkpoint", dst)
	}
	if _, _, err := history.Restore(a.repo, tree, map[string]string{}, []string{"README"}); err != nil {
		if a.objectMissing() {
			a.lost = true
		}
		return err
	}
	a.pending--
	return nil
}

// AbandonFork drops a pending resolve without ever touching git, the
// way an abandoned child session (deleted, or nobody runs /undo) does.
func (a *ckgfAdapter) AbandonFork() error {
	if !a.gate.pass(a.pending > 0) {
		return nil
	}
	a.forks = a.forks[:len(a.forks)-1]
	a.pending--
	return nil
}

// DropRef plays the reaper (or the person clearing refs) bough itself
// does not have: a real `git update-ref -d`, guarded exactly as the
// spec guards it.
func (a *ckgfAdapter) DropRef() error {
	if !a.gate.pass(a.refExists() && a.pending == 0) {
		return nil
	}
	return a.git("update-ref", "-d", a.refName())
}

// GitGC plays the external `git gc` the spec says only ever runs once
// no ref points at the object. It verifies the object is actually gone
// afterward: a real gc that leaves it (a reflog, a stray reference)
// fails the walk instead of reporting a collected the graph cannot see.
func (a *ckgfAdapter) GitGC() error {
	if !a.gate.pass(!a.refExists() && !a.objectMissing()) {
		return nil
	}
	if a.gcNoop {
		return nil
	}
	// update-ref -d logs the deletion to logs/refs/bough/turns/...
	// (core.logAllRefUpdates defaults to true in a non-bare repo), and a
	// reflog entry keeps gc from pruning the object it points at
	// regardless of --prune=now. Real bough never runs this ref through
	// git itself, so nothing in the product expires that reflog; the
	// external actor the spec is about has to, same as clearing the ref.
	if err := a.git("reflog", "expire", "--expire=now", "--expire-unreachable=now", "--all"); err != nil {
		return err
	}
	if err := a.git("-c", "gc.pruneExpire=now", "gc", "--prune=now", "--quiet"); err != nil {
		return err
	}
	if !a.objectMissing() {
		return fmt.Errorf("checkpoint_ref_gc_vs_fork: git gc did not collect %s", a.tree)
	}
	return nil
}

// NewCheckpoint is another real turn once the old checkpoint is fully
// wound down: a fresh ref for a fresh tree, never the collected one.
func (a *ckgfAdapter) NewCheckpoint() error {
	if !a.gate.pass(!a.refExists() && a.pending == 0) {
		return nil
	}
	return a.pinCheckpoint()
}

func ckgfAction(name string, f func(*ckgfAdapter) error) fmbt.ActionFunc {
	return func(m any, _ []fmbt.Arg) (any, error) { return nil, f(m.(*ckgfAdapter)) }
}

var ckgfActions = map[string]map[string]fmbt.ActionFunc{"Checkpoint": {
	"Fork":          ckgfAction("Fork", (*ckgfAdapter).Fork),
	"ResolveFork":   ckgfAction("ResolveFork", (*ckgfAdapter).ResolveFork),
	"AbandonFork":   ckgfAction("AbandonFork", (*ckgfAdapter).AbandonFork),
	"DropRef":       ckgfAction("DropRef", (*ckgfAdapter).DropRef),
	"GitGC":         ckgfAction("GitGC", (*ckgfAdapter).GitGC),
	"NewCheckpoint": ckgfAction("NewCheckpoint", (*ckgfAdapter).NewCheckpoint),
}}

// Every checkpoint and new-checkpoint is a real turn through a real
// serve, and DropRef/GitGC are real git; the default run is short.
func ckgfOptions() map[string]any {
	return map[string]any{"max-seq-runs": 100, "max-actions": 8, "max-parallel-runs": 0}
}

// walkCKGFPaths walks every generated path against its own serve,
// comparing GetState after every step to the path's expected state —
// the check that does not depend on any of Fork/DropRef/GitGC ever
// showing up in a session transcript, which they do not: only the
// checkpoint turns themselves are recorded, everything else is either
// a separate forked file or plain git run by the adapter as the
// external actor the spec is about.
func walkCKGFPaths(t *testing.T, as []*ckgfAdapter, cover tracecheck.Cover, stopFirst bool) error {
	t.Helper()
	g, err := tracecheck.Load(filepath.Join(filepath.Dir(specPath("checkpoint_ref_gc_vs_fork")), "..", "testdata", "checkpoint_ref_gc_vs_fork"))
	if err != nil {
		return err
	}
	walks := g.Walks(cover, 0)
	var (
		mu     sync.Mutex
		errs   []error
		walked int
		wg     sync.WaitGroup
	)
	start := time.Now()
	for n, a := range as {
		wg.Go(func() {
			for i := n; i < len(walks); i += len(as) {
				err := a.walk(walks[i].Trace)
				mu.Lock()
				walked++
				if err != nil {
					errs = append(errs, fmt.Errorf("path %d: %w", i, err))
					t.Logf("path %d: %v", i, err)
				}
				mu.Unlock()
				if err != nil && stopFirst {
					break
				}
			}
			a.Cleanup()
		})
	}
	wg.Wait()
	t.Logf("%d paths on %d serves in %s, %d failed", len(walks), len(as), time.Since(start).Round(time.Second), len(errs))
	return errors.Join(errs...)
}

func (a *ckgfAdapter) walk(trace []tracecheck.Step) error {
	var did []string
	for j, s := range trace {
		name := strings.TrimPrefix(s.Action, "Checkpoint#0.")
		did = append(did, name)
		var err error
		if s.Action == "Init" {
			err = a.Init()
		} else {
			_, err = ckgfActions["Checkpoint"][name](a, nil)
			if err == nil && a.gate.off {
				err = errors.New("the adapter found it disabled")
			}
		}
		if err != nil {
			return fmt.Errorf("step %d (%s): %w; walked %v", j, name, err, did)
		}
		got, err := a.GetState()
		if err != nil {
			return fmt.Errorf("step %d (%s): state: %w; walked %v", j, name, err, did)
		}
		var diff []string
		for k, v := range s.State {
			f, ok := strings.CutPrefix(k, "Checkpoint#0.")
			if ok && fmt.Sprint(got[f]) != fmt.Sprint(v) {
				diff = append(diff, fmt.Sprintf("%s: spec %v, got %v", f, v, got[f]))
			}
		}
		if len(diff) > 0 {
			return fmt.Errorf("step %d (%s): %s; walked %v", j, name, strings.Join(diff, "; "), did)
		}
	}
	return nil
}

// TestCheckpointRefGCvsFork lets fizzbee-mbt walk the spec at random
// (the exhaustive run only; see runMBT).
func TestCheckpointRefGCvsFork(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCKGFAdapter(t)
	if err := runMBT(t, "checkpoint_ref_gc_vs_fork", a, ckgfActions, ckgfOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// TestCheckpointRefGCvsForkPaths walks every generated path against
// three real serves.
func TestCheckpointRefGCvsForkPaths(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	as := []*ckgfAdapter{newCKGFAdapter(t), newCKGFAdapter(t), newCKGFAdapter(t)}
	if err := walkCKGFPaths(t, as, envCover(), false); err != nil {
		t.Fatalf("spec path: %v", err)
	}
}

// The run above proves nothing unless a GitGC that only claims success
// fails it: the graph expects collected = True right after, and a real
// git cat-file still finding the object is exactly the disagreement
// the spec exists to catch.
func TestCheckpointRefGCvsForkCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newCKGFAdapter(t)
	a.gcNoop = true
	err := walkCKGFPaths(t, []*ckgfAdapter{a}, tracecheck.CoverStates, true)
	if err == nil {
		t.Fatal("a run whose GitGC leaves the object passed; the paths are not checking state")
	}
	t.Logf("caught as expected: %v", err)
}
