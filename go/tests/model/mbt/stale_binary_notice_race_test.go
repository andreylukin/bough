//go:build !windows

package mbt

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	fmbt "github.com/fizzbee-io/fizzbee/mbt/lib/go"
)

// specs/stale_binary_notice_race.fizz against the real staleNotice
// algorithm (cmd/bough/stale.go), copied byte for byte into a scratch
// module and compiled: a _test.go here cannot import package main, so
// this is the only way to run the actual decision code rather than a
// hand-written stand-in. The scratch module is a real git checkout (git
// init, one commit) and the built "stale" binary is invoked as a real
// subprocess for every read, exactly as `bough --version` invokes
// staleNotice on itself. Only mtimes move between actions — the
// checkout's HEAD never changes — which is enough to drive both of
// staleness()'s branches (the mtime one; the revision one is exercised
// by cmd/bough/stale_test.go directly, which needs no process at all).
//
// A virtual clock (a.clock, advanced a minute at a time and applied with
// os.Chtimes) stands in for wall time: the model's Edit/FinishUpdate
// steps happen microseconds apart in a real run, far under the 2s margin
// staleness() uses to ignore a build racing its own sources.

// stalecheckAdapter is both the fmbt.Model and the spec's one Session
// role. GetRoles/GetState below report the fields, but the ground truth
// they read is a real "go build" output and real file mtimes.
type stalecheckAdapter struct {
	t    *testing.T
	root string // scratch checkout: go.mod, stale.go, main.go, .git
	exe  string // root/bin/stale, the "installed" binary
	src  string // root/stale.go, the "source" newestSource sees
	gate gate

	clock                       time.Time
	updating, restarted, missed bool
	underfoot                   bool

	// dropUnderfoot is TestStaleBinaryNoticeRaceCatchesWrongAdapter's bug:
	// Edit stops reporting underfoot, the kind of adapter-side observation
	// slip the runner must catch on the very first real action (a bug
	// buried three actions deep, like FinishUpdate skipping the install,
	// would only be reached by a small fraction of the short random
	// walks below — this one is reachable from Init in a single step).
	dropUnderfoot bool
}

// staleSourcePath is cmd/bough/stale.go, read fresh for every adapter so
// the copy never drifts out of sync with the product it is testing.
func staleSourcePath() string {
	_, file, _, _ := runtime.Caller(0)
	return filepath.Join(filepath.Dir(file), "..", "..", "..", "cmd", "bough", "stale.go")
}

func newStalecheckAdapter(t *testing.T) *stalecheckAdapter {
	t.Helper()
	root := t.TempDir()

	src, err := os.ReadFile(staleSourcePath())
	if err != nil {
		t.Fatalf("read cmd/bough/stale.go: %v", err)
	}
	write := func(name string, b []byte) {
		if err := os.WriteFile(filepath.Join(root, name), b, 0o644); err != nil {
			t.Fatalf("write %s: %v", name, err)
		}
	}
	write("stale.go", src)
	// stale.go's checkoutAbove also calls hasGit and moduleDir, which
	// live in cmd/bough/update.go, not stale.go: copied verbatim too, so
	// the scratch module needs nothing that is not the real product code.
	write("main.go", []byte(`package main

import (
	"fmt"
	"os"
	"path/filepath"
)

func main() { fmt.Print(staleNotice(os.Args[0])) }

func hasGit(dir string) bool {
	_, err := os.Stat(filepath.Join(dir, ".git"))
	return err == nil
}

func moduleDir(root string) (string, error) {
	sub := filepath.Join(root, "go")
	if _, err := os.Stat(filepath.Join(sub, "go.mod")); err == nil {
		return sub, nil
	}
	if _, err := os.Stat(filepath.Join(root, "go.mod")); err == nil {
		return root, nil
	}
	return "", fmt.Errorf("no go.mod in %s or %s", root, sub)
}
`))
	write("go.mod", []byte("module stalecheck\n\ngo 1.21\n"))

	git := func(args ...string) {
		full := append([]string{"-C", root, "-c", "user.email=mbt@bough.test", "-c", "user.name=mbt"}, args...)
		if out, err := exec.Command("git", full...).CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	git("init", "-q")
	git("add", "-A")
	git("commit", "-q", "-m", "init")

	exe := filepath.Join(root, "bin", "stale")
	cmd := exec.Command("go", "build", "-o", exe, ".")
	cmd.Dir = root
	cmd.Env = os.Environ()
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("go build scratch stale checker: %v\n%s", err, out)
	}

	return &stalecheckAdapter{t: t, root: root, exe: exe, src: filepath.Join(root, "stale.go")}
}

// Init resets the walk to a clean install: exe and every .go file share
// one old mtime, so newestSource never outruns the binary.
func (a *stalecheckAdapter) Init() error {
	a.clock = time.Now()
	old := a.clock.Add(-2 * time.Hour)
	for _, name := range []string{"stale.go", "main.go"} {
		if err := os.Chtimes(filepath.Join(a.root, name), old, old); err != nil {
			return err
		}
	}
	if err := os.Chtimes(a.exe, old.Add(time.Hour), old.Add(time.Hour)); err != nil {
		return err
	}
	a.updating, a.restarted, a.missed, a.underfoot = false, false, false, false
	a.gate.reset()
	return nil
}

func (a *stalecheckAdapter) Cleanup() error { return nil }

func (a *stalecheckAdapter) GetRoles() (map[fmbt.RoleId]fmbt.Role, error) {
	return map[fmbt.RoleId]fmbt.Role{{RoleName: "Session", Index: 0}: a}, nil
}

// noticeNow runs the real compiled checker: "" when current, else the
// notice text staleNotice would show.
func (a *stalecheckAdapter) noticeNow() (string, error) {
	out, err := exec.Command(a.exe).CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("run stale checker: %w\n%s", err, out)
	}
	return string(out), nil
}

func (a *stalecheckAdapter) GetState() (map[string]any, error) {
	notice, err := a.noticeNow()
	if err != nil {
		return nil, err
	}
	status := "none"
	if notice != "" {
		status = "stale"
	}
	return map[string]any{
		"notice":    status,
		"underfoot": a.underfoot,
		"updating":  a.updating,
		"restarted": a.restarted,
		"missed":    a.missed,
	}, nil
}

// tick moves the virtual clock forward and stamps path with it.
func (a *stalecheckAdapter) tick(path string) error {
	a.clock = a.clock.Add(time.Minute)
	return os.Chtimes(path, a.clock, a.clock)
}

// Edit: a source edit lands (or a background `git pull` would, at real
// mtime resolution). Only ever moves the checkout further from the exe.
func (a *stalecheckAdapter) Edit() error {
	if !a.gate.pass(!a.updating) {
		return nil
	}
	if err := a.tick(a.src); err != nil {
		return err
	}
	if !a.dropUnderfoot {
		a.underfoot = true
	}
	return nil
}

// StartUpdate: `bough update` begins rebuilding — require mirrors the
// spec's require exactly, reading the real notice off the checker.
func (a *stalecheckAdapter) StartUpdate() error {
	notice, err := a.noticeNow()
	if err != nil {
		return err
	}
	if !a.gate.pass(!a.updating && notice != "") {
		return nil
	}
	a.updating = true
	return nil
}

// EditDuringUpdate: an edit lands while the rebuild is already reading
// the tree — the sharper race the spec's `missed` names.
func (a *stalecheckAdapter) EditDuringUpdate() error {
	if !a.gate.pass(a.updating) {
		return nil
	}
	if err := a.tick(a.src); err != nil {
		return err
	}
	a.underfoot = true
	a.missed = true
	return nil
}

// FinishUpdate: the rebuild lands and the process restarts — a fresh
// binary, mtime-stamped now, which real writes always are after every
// earlier edit's mtime, missed or not: the race is that its *content*
// may not be, which staleNotice cannot see either way.
func (a *stalecheckAdapter) FinishUpdate() error {
	if !a.gate.pass(a.updating) {
		return nil
	}
	if err := a.tick(a.exe); err != nil {
		return err
	}
	a.updating = false
	a.restarted = true
	a.underfoot = false
	return nil
}

var staleBinaryNoticeRaceActions = map[string]map[string]fmbt.ActionFunc{"Session": {
	"Edit":             action((*stalecheckAdapter).Edit),
	"StartUpdate":      action((*stalecheckAdapter).StartUpdate),
	"EditDuringUpdate": action((*stalecheckAdapter).EditDuringUpdate),
	"FinishUpdate":     action((*stalecheckAdapter).FinishUpdate),
}}

// Every step re-runs a real "go build"-produced binary, so a walk is
// cheap but not free; a modest run is enough to cover this small graph.
func staleBinaryNoticeRaceOptions() map[string]any {
	return map[string]any{"max-seq-runs": 30, "max-actions": 8, "max-parallel-runs": 0}
}

func TestStaleBinaryNoticeRace(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newStalecheckAdapter(t)
	if err := runMBT(t, "stale_binary_notice_race", a, staleBinaryNoticeRaceActions, staleBinaryNoticeRaceOptions()); err != nil {
		t.Fatalf("model-based run: %v", err)
	}
}

// The run above proves nothing unless a checker that disagrees with the
// model fails it: this adapter's Edit stops reporting underfoot, wrong
// from the very first real action every walk can take.
func TestStaleBinaryNoticeRaceCatchesWrongAdapter(t *testing.T) {
	t.Parallel()
	fizzTools(t)
	a := newStalecheckAdapter(t)
	a.dropUnderfoot = true
	if err := runMBT(t, "stale_binary_notice_race", a, staleBinaryNoticeRaceActions, staleBinaryNoticeRaceOptions()); err == nil {
		t.Fatal("a run whose Edit never reports underfoot passed; the runner is not checking state")
	}
}
