package ci

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// TestMain keeps the host's git config out of every git the tests run,
// the package's own included: a core.hooksPath ran the user's hooks on
// the fixture's commits, and a core.excludesFile could drop fixture
// files from a snapshot. Production still reads the global config —
// the user's excludes are theirs to apply — so this is set here, for
// the process, rather than in gitEnv. It also serves as the helper
// process for the tests that need bough ci in another process.
func TestMain(m *testing.M) {
	os.Setenv("GIT_CONFIG_GLOBAL", os.DevNull)
	os.Setenv("GIT_CONFIG_NOSYSTEM", "1")
	if home := os.Getenv("BOUGH_CI_TEST_HELPER_HOME"); home != "" {
		os.Exit(helperRun(home))
	}
	os.Exit(m.Run())
}

// helperRun is `bough ci` in the helper process, from its cwd.
func helperRun(home string) int {
	dir, _ := os.Getwd()
	rep, err := Run(context.Background(), Options{Home: home, Dir: dir})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 3
	}
	return rep.ExitCode()
}

// helper is this test binary as a separate `bough ci` in f.repo, with
// extra env.
func (f *fixture) helper(env ...string) *exec.Cmd {
	c := exec.Command(os.Args[0], "-test.run=^$")
	c.Dir = f.repo
	c.Env = append(append(os.Environ(), "BOUGH_CI_TEST_HELPER_HOME="+f.home), env...)
	return c
}

// waitFor polls cond without t: it runs off the test goroutine, where
// t.Fatal is not allowed.
func waitFor(cond func() bool) bool {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if cond() {
			return true
		}
		time.Sleep(10 * time.Millisecond)
	}
	return false
}

func (f *fixture) exists(name string) func() bool {
	return func() bool { _, err := os.Stat(filepath.Join(f.counters, name)); return err == nil }
}

// fixture is a temp repo with a temp HOME. Checks append to counter
// files, so a test can tell a run from a cache hit.
type fixture struct {
	t          *testing.T
	repo, home string
	counters   string
}

func newRepo(t *testing.T, ciYML string) *fixture {
	t.Helper()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	f := &fixture{t: t, repo: t.TempDir(), home: t.TempDir(), counters: t.TempDir()}
	f.git("init", "-q")
	f.write("src/a.txt", "a\n")
	f.write("other.txt", "o\n")
	f.write("keep.txt", "k\n")
	if ciYML != "" {
		f.write(ConfigPath, f.expand(ciYML))
	}
	f.git("add", "-A")
	f.git("commit", "-qm", "init")
	return f
}

// expand replaces $C/<name> with the counter file's absolute path.
func (f *fixture) expand(s string) string {
	return strings.ReplaceAll(s, "$C/", f.counters+"/")
}

func (f *fixture) git(args ...string) string {
	f.t.Helper()
	c := exec.Command("git", append([]string{"-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"}, args...)...)
	c.Dir = f.repo
	out, err := c.CombinedOutput()
	if err != nil {
		f.t.Fatalf("git %v: %v\n%s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

func (f *fixture) write(rel, body string) {
	f.t.Helper()
	p := filepath.Join(f.repo, filepath.FromSlash(rel))
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		f.t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		f.t.Fatal(err)
	}
}

func (f *fixture) count(name string) int {
	b, err := os.ReadFile(filepath.Join(f.counters, name))
	if os.IsNotExist(err) {
		return 0
	}
	if err != nil {
		f.t.Fatal(err)
	}
	return strings.Count(string(b), "x")
}

func (f *fixture) opts() Options { return Options{Home: f.home, Dir: f.repo} }

func (f *fixture) run(o Options) Report {
	f.t.Helper()
	rep, err := Run(context.Background(), o)
	if err != nil {
		f.t.Fatalf("Run: %v", err)
	}
	return rep
}

func state(rep Report, name string) CheckStatus {
	for _, c := range rep.Checks {
		if c.Name == name {
			return c
		}
	}
	return CheckStatus{Name: name, State: "absent"}
}

const twoChecks = `checks:
  src:
    run: echo x >> $C/src
    inputs: ["src/**"]
  whole:
    run: echo x >> $C/whole
    go_cache: true
`

func TestFirstRunExecutesSecondIsCached(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	rep := f.run(f.opts())
	if f.count("src") != 1 || f.count("whole") != 1 || rep.ExitCode() != 0 {
		t.Fatalf("first run: counts %d %d report %+v", f.count("src"), f.count("whole"), rep)
	}
	if st := state(rep, "src"); st.Cached || st.State != StatePass {
		t.Fatalf("first run src: %+v", st)
	}
	rep = f.run(f.opts())
	if f.count("src") != 1 || f.count("whole") != 1 {
		t.Fatalf("second run reran: %d %d", f.count("src"), f.count("whole"))
	}
	if st := state(rep, "whole"); !st.Cached || st.State != StatePass || rep.ExitCode() != 0 {
		t.Fatalf("second run: %+v exit %d", st, rep.ExitCode())
	}
}

func TestEditOutsideInputsDoesNotRerun(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	f.run(f.opts())
	f.write("other.txt", "changed\n") // uncommitted: the working tree is what counts
	rep := f.run(f.opts())
	if f.count("src") != 1 {
		t.Fatalf("src reran on an edit outside its inputs")
	}
	if !state(rep, "src").Cached {
		t.Fatalf("src not cached: %+v", state(rep, "src"))
	}
}

func TestEditInsideInputsReruns(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	f.run(f.opts())
	f.write("src/a.txt", "changed\n")
	f.run(f.opts())
	if f.count("src") != 2 {
		t.Fatalf("src count %d, want 2", f.count("src"))
	}
	// A new untracked file under the inputs is an input too.
	f.write("src/new.txt", "n\n")
	f.run(f.opts())
	if f.count("src") != 3 {
		t.Fatalf("src count %d after new file, want 3", f.count("src"))
	}
}

func TestWholeTreeCheckRerunsOnAnyChange(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks+"  plain:\n    run: echo x >> $C/plain\n")
	f.run(f.opts())
	f.write("other.txt", "changed\n")
	f.run(f.opts())
	if f.count("whole") != 2 || f.count("plain") != 2 {
		t.Fatalf("whole-tree checks: whole %d plain %d, want 2 2", f.count("whole"), f.count("plain"))
	}
}

func TestCheckDefinitionChangeInvalidates(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	f.run(f.opts())
	// Only the command changes: the inputs are the same files.
	f.write(ConfigPath, f.expand(strings.Replace(twoChecks, "echo x >> $C/src", "echo x >>$C/src", 1)))
	f.run(f.opts())
	if f.count("src") != 2 {
		t.Fatalf("src count %d after its run changed, want 2", f.count("src"))
	}
}

func TestConfigReadFromTreeNotWorkdir(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  old:\n    run: echo x >> $C/old\n")
	first := f.git("rev-parse", "HEAD")
	f.write(ConfigPath, f.expand("checks:\n  new:\n    run: echo x >> $C/new\n"))
	f.git("commit", "-qam", "new check")

	o := f.opts()
	o.Tree = first
	rep := f.run(o)
	if state(rep, "old").State != StatePass || state(rep, "new").State != "absent" {
		t.Fatalf("old tree ran the working dir's config: %+v", rep.Checks)
	}
	rep = f.run(f.opts())
	if state(rep, "new").State != StatePass || f.count("old") != 1 || f.count("new") != 1 {
		t.Fatalf("current tree: %+v", rep.Checks)
	}
	if rep.ConfigFrom != ConfigPath {
		t.Fatalf("config from %q", rep.ConfigFrom)
	}
}

func TestFailingCheckExit1AndLogKept(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  bad:\n    run: echo boom-output; exit 3\n  good:\n    run: 'true'\n")
	rep := f.run(f.opts())
	st := state(rep, "bad")
	if st.State != StateFail || st.Result == nil || st.Result.ExitCode != 3 || rep.ExitCode() != 1 {
		t.Fatalf("bad: %+v exit %d", st, rep.ExitCode())
	}
	path, res, err := Log(context.Background(), f.opts(), "bad")
	if err != nil || res.Status != StateFail {
		t.Fatalf("Log: %v %+v", err, res)
	}
	b, _ := os.ReadFile(path)
	if !strings.Contains(string(b), "boom-output") {
		t.Fatalf("log %q lacks the output", b)
	}
	// A failure is a result too: it is cached, not rerun.
	if rep := f.run(f.opts()); !state(rep, "bad").Cached || rep.ExitCode() != 1 {
		t.Fatalf("second run: %+v", rep.Checks)
	}
}

func TestLogWithoutResultErrors(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	if _, _, err := Log(context.Background(), f.opts(), "src"); err == nil || !strings.Contains(err.Error(), "no result for src") {
		t.Fatalf("Log before any run: %v", err)
	}
}

func TestNoWaitUnsettledIs2(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	o := f.opts()
	o.NoWait = true
	rep := f.run(o)
	if rep.ExitCode() != 2 || state(rep, "src").State != StateUnknown || f.count("src") != 0 {
		t.Fatalf("no-wait before any run: %+v", rep.Checks)
	}
	f.run(f.opts())
	if rep := f.run(o); rep.ExitCode() != 0 {
		t.Fatalf("no-wait after a run: %+v", rep.Checks)
	}
	f.write("src/a.txt", "changed\n")
	rep = f.run(o)
	if rep.ExitCode() != 2 || state(rep, "src").State != StateUnknown || state(rep, "whole").State != StateUnknown {
		t.Fatalf("no-wait after an edit: %+v", rep.Checks)
	}
	if f.count("src") != 1 {
		t.Fatal("no-wait ran a check")
	}
}

func TestNoWaitReportsRunningWhileLocked(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	r, err := Open(f.repo)
	if err != nil {
		t.Fatal(err)
	}
	dir := StateDir(f.home, r)
	os.MkdirAll(dir, 0o755)
	lf, held, err := lock(filepath.Join(dir, "lock"), true)
	if err != nil || !held {
		t.Fatal(err)
	}
	defer lf.Close()
	o := f.opts()
	o.NoWait = true
	rep := f.run(o)
	if state(rep, "src").State != StateRunning || rep.ExitCode() != 2 {
		t.Fatalf("locked: %+v", rep.Checks)
	}
}

func TestManualOnlyWithCheck(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks+"  slow:\n    run: echo x >> $C/slow\n    manual: true\n")
	rep := f.run(f.opts())
	if state(rep, "slow").State != StateManual || f.count("slow") != 0 || rep.ExitCode() != 0 {
		t.Fatalf("manual ran unasked: %+v", rep.Checks)
	}
	o := f.opts()
	o.Checks = []string{"slow"}
	rep = f.run(o)
	if f.count("slow") != 1 || len(rep.Checks) != 1 || state(rep, "slow").State != StatePass {
		t.Fatalf("--check slow: %+v", rep.Checks)
	}
}

func TestRerunIgnoresCache(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	f.run(f.opts())
	o := f.opts()
	o.Rerun, o.Checks = true, []string{"src"}
	f.run(o)
	if f.count("src") != 2 || f.count("whole") != 1 {
		t.Fatalf("rerun: src %d whole %d", f.count("src"), f.count("whole"))
	}
}

func TestUnknownCheckErrors(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	o := f.opts()
	o.Checks = []string{"nope"}
	_, err := Run(context.Background(), o)
	if err == nil || !strings.Contains(err.Error(), `no check "nope"`) || !strings.Contains(err.Error(), "src, whole") {
		t.Fatalf("unknown check: %v", err)
	}
}

func TestConcurrentRunsRunOnce(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  slow:\n    run: echo x >> $C/slow; sleep 0.3\n")
	var wg sync.WaitGroup
	reps := make([]Report, 3)
	for i := range reps {
		wg.Add(1)
		go func() {
			defer wg.Done()
			rep, err := Run(context.Background(), f.opts())
			if err != nil {
				t.Error(err)
			}
			reps[i] = rep
		}()
	}
	wg.Wait()
	if f.count("slow") != 1 {
		t.Fatalf("concurrent runs ran the check %d times", f.count("slow"))
	}
	for _, rep := range reps {
		if rep.ExitCode() != 0 {
			t.Fatalf("report: %+v", rep.Checks)
		}
	}
}

func workDir(t *testing.T, f *fixture) string {
	t.Helper()
	r, err := Open(f.repo)
	if err != nil {
		t.Fatal(err)
	}
	return filepath.Join(StateDir(f.home, r), "work")
}

func TestWorktreeKeepsMtimeOfUnchangedFiles(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  all:\n    run: echo x >> $C/all\n")
	f.run(f.opts())
	keep := filepath.Join(workDir(t, f), "keep.txt")
	old := time.Now().Add(-48 * time.Hour).Truncate(time.Second)
	if err := os.Chtimes(keep, old, old); err != nil {
		t.Fatal(err)
	}
	f.write("other.txt", "changed\n")
	f.run(f.opts())
	if f.count("all") != 2 {
		t.Fatalf("count %d", f.count("all"))
	}
	fi, err := os.Stat(keep)
	if err != nil || !fi.ModTime().Equal(old) {
		t.Fatalf("keep.txt mtime moved: %v %v (want %v)", fi.ModTime(), err, old)
	}
	b, _ := os.ReadFile(filepath.Join(workDir(t, f), "other.txt"))
	if string(b) != "changed\n" {
		t.Fatalf("other.txt in worktree: %q", b)
	}
}

func TestWorktreeDropsDeletedAndRestoresDirty(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  messy:\n    run: echo dirty > keep.txt; echo s > stray.txt; echo x >> $C/messy\n")
	f.run(f.opts())
	work := workDir(t, f)
	if b, _ := os.ReadFile(filepath.Join(work, "keep.txt")); string(b) != "dirty\n" {
		t.Fatalf("check did not write: %q", b)
	}
	if err := os.Remove(filepath.Join(f.repo, "other.txt")); err != nil {
		t.Fatal(err)
	}
	f.write(ConfigPath, f.expand("checks:\n  look:\n    run: cat keep.txt; test ! -e stray.txt && test ! -e other.txt\n"))
	rep := f.run(f.opts())
	if st := state(rep, "look"); st.State != StatePass {
		path, _, _ := Log(context.Background(), f.opts(), "look")
		b, _ := os.ReadFile(path)
		t.Fatalf("worktree not reset: %+v\n%s", st, b)
	}
	path, _, _ := Log(context.Background(), f.opts(), "look")
	if b, _ := os.ReadFile(path); string(b) != "k\n" {
		t.Fatalf("keep.txt in worktree: %q", b)
	}
}

func TestRepoKeySharedAcrossWorktrees(t *testing.T) {
	t.Parallel()
	f := newRepo(t, twoChecks)
	wt := filepath.Join(t.TempDir(), "wt")
	f.git("worktree", "add", "-q", "--detach", wt)
	a, err := Open(f.repo)
	if err != nil {
		t.Fatal(err)
	}
	b, err := Open(filepath.Join(wt, "src"))
	if err != nil {
		t.Fatal(err)
	}
	if a.Key != b.Key || b.Top == a.Top {
		t.Fatalf("keys %q %q tops %q %q", a.Key, b.Key, a.Top, b.Top)
	}
	// And a tree checked in one is cached for the other.
	f.run(f.opts())
	rep := f.run(Options{Home: f.home, Dir: wt})
	if !state(rep, "src").Cached || f.count("src") != 1 {
		t.Fatalf("second worktree reran: %+v", rep.Checks)
	}
}

func TestFallbackToProjectFast(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "")
	pdir := filepath.Join(f.home, ".bough", "projects", "p")
	os.MkdirAll(pdir, 0o755)
	yml := "repos:\n  - path: " + f.repo + "\nchecks:\n  fast: echo x >> " + f.counters + "/fast\n  full: echo x >> " + f.counters + "/full\n"
	if err := os.WriteFile(filepath.Join(pdir, "project.yml"), []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	rep := f.run(f.opts())
	if state(rep, "fast").State != StatePass || state(rep, "full").State != StateManual || f.count("fast") != 1 || f.count("full") != 0 {
		t.Fatalf("fallback: %+v", rep.Checks)
	}
	if !strings.Contains(rep.ConfigFrom, "project p") {
		t.Fatalf("config from %q", rep.ConfigFrom)
	}
}

func TestNoConfigErrorIsClear(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "")
	_, err := Run(context.Background(), f.opts())
	if err == nil || !strings.Contains(err.Error(), "no .bough/ci.yml in tree") || !strings.Contains(err.Error(), "checks.fast") {
		t.Fatalf("no config: %v", err)
	}
}

func TestBadDirFailsTheCheck(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  x:\n    run: 'true'\n    dir: missing\n")
	rep := f.run(f.opts())
	if state(rep, "x").State != StateFail {
		t.Fatalf("missing dir: %+v", rep.Checks)
	}
}

func TestParseRejectsBadConfig(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, yml, want string }{
		{"unknown field", "checks:\n  a:\n    run: x\n    input: [a]\n", "input"},
		{"bad name", "checks:\n  'a b':\n    run: x\n", "name must match"},
		{"empty run", "checks:\n  a:\n    run: ''\n", "run is empty"},
		{"escaping dir", "checks:\n  a:\n    run: x\n    dir: ../up\n", "escapes"},
		{"absolute dir", "checks:\n  a:\n    run: x\n    dir: /etc\n", "relative"},
		{"both keys", "checks:\n  a:\n    run: x\n    inputs: [a]\n    go_cache: true\n", "pick one"},
		{"no checks", "", "no checks"},
		{"bad glob", "checks:\n  a:\n    run: x\n    inputs: ['[']\n", "input"},
		{"escaping glob", "checks:\n  a:\n    run: x\n    inputs: ['../x/**']\n", "inside the repository"},
	} {
		_, err := Parse([]byte(tc.yml))
		if err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Errorf("%s: %v (want %q)", tc.name, err, tc.want)
		}
	}
	if _, err := Parse([]byte("checks:\n  ok-1.x:\n    run: go vet ./...\n    dir: go\n")); err != nil {
		t.Fatalf("valid config: %v", err)
	}
}

func TestMatchGlob(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		pat, name string
		want      bool
	}{
		{"src/**", "src/a.txt", true},
		{"src/**", "src/x/y/z.go", true},
		{"src/**", "srcx/a", false},
		{"src/**", "other.txt", false},
		{"**/*.go", "main.go", true},
		{"**/*.go", "a/b/main.go", true},
		{"**/*.go", "a/b/main.js", false},
		{"*.lock", "bun.lock", true},
		{"*.lock", "web/bun.lock", false},
		{"go.mod", "go.mod", true},
		{"a/**/b", "a/b", true},
		{"a/**/b", "a/x/y/b", true},
		{"a/**/b", "a/x/y/c", false},
	} {
		if got := matchGlob(tc.pat, tc.name); got != tc.want {
			t.Errorf("matchGlob(%q, %q) = %v", tc.pat, tc.name, got)
		}
	}
}

func TestRepoWithoutCommits(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	f := &fixture{t: t, repo: t.TempDir(), home: t.TempDir(), counters: t.TempDir()}
	f.git("init", "-q")
	f.write(ConfigPath, f.expand("checks:\n  a:\n    run: echo x >> $C/a\n"))
	rep := f.run(f.opts())
	if state(rep, "a").State != StatePass || f.count("a") != 1 {
		t.Fatalf("no commits: %+v", rep.Checks)
	}
}

func TestInterruptedCheckIsNotStored(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  hang:\n    run: echo x >> $C/hang; sleep 30\n")
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		waitFor(f.exists("hang"))
		cancel()
	}()
	start := time.Now()
	if _, err := Run(ctx, f.opts()); err == nil || !strings.Contains(err.Error(), "interrupted") {
		t.Fatalf("interrupted run: %v", err)
	}
	if time.Since(start) > 10*time.Second {
		t.Fatal("cancel did not kill the check's process group")
	}
	if f.count("hang") != 1 {
		t.Fatalf("the check never started: count %d", f.count("hang"))
	}
	o := f.opts()
	o.NoWait = true
	if rep := f.run(o); state(rep, "hang").State != StateUnknown {
		t.Fatalf("interrupted run was stored: %+v", rep.Checks)
	}
}

// A check killed from outside says nothing about the tree: no result.
func TestKilledCheckIsNotStored(t *testing.T) {
	t.Parallel()
	for _, run := range []string{"kill -KILL $$", "sh -c 'kill -KILL $$'; exit $?"} {
		f := newRepo(t, "checks:\n  oom:\n    run: \""+run+"\"\n")
		if _, err := Run(context.Background(), f.opts()); err == nil || !strings.Contains(err.Error(), "killed") {
			t.Fatalf("%s: %v", run, err)
		}
		o := f.opts()
		o.NoWait = true
		if rep := f.run(o); state(rep, "oom").State != StateUnknown {
			t.Fatalf("%s: killed run was stored: %+v", run, rep.Checks)
		}
	}
}

// bough ci from a git hook: git exports GIT_INDEX_FILE (relative in a
// plain commit, the absolute index.lock under commit -a) and may export
// GIT_DIR. None of it may reach the CI worktree or the user's index,
// and the check must not see it either.
func TestCallerGitEnvDoesNotLeak(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		env  func(repo string) []string
	}{
		{"relative", func(string) []string { return []string{"GIT_INDEX_FILE=.git/index", "GIT_DIR=.git"} }},
		{"absolute", func(repo string) []string {
			return []string{"GIT_INDEX_FILE=" + filepath.Join(repo, ".git", "index"), "GIT_DIR=" + filepath.Join(repo, ".git"), "GIT_WORK_TREE=" + repo}
		}},
	} {
		f := newRepo(t, "checks:\n  env:\n    run: test -z \"$GIT_INDEX_FILE$GIT_DIR$GIT_WORK_TREE\" && test -z \"$(git status --porcelain)\" && echo x >> $C/env\n")
		f.write("untracked.txt", "u\n")
		index := filepath.Join(f.repo, ".git", "index")
		before, err := os.ReadFile(index)
		if err != nil {
			t.Fatal(err)
		}
		if out, err := f.helper(tc.env(f.repo)...).CombinedOutput(); err != nil {
			t.Fatalf("%s: bough ci: %v\n%s", tc.name, err, out)
		}
		if after, _ := os.ReadFile(index); !bytes.Equal(before, after) {
			t.Fatalf("%s: the caller's index was rewritten", tc.name)
		}
		if ls := f.git("ls-files"); strings.Contains(ls, "untracked.txt") {
			t.Fatalf("%s: untracked file landed in the index: %q", tc.name, ls)
		}
		if f.count("env") != 1 {
			t.Fatalf("%s: check did not pass in a clean env", tc.name)
		}
	}
}

func TestInputsAreNormalisedAndMustMatch(t *testing.T) {
	t.Parallel()
	c, err := Parse([]byte("checks:\n  a:\n    run: x\n    inputs: ['./src/**', 'web/', 'a//b/./c', './']\n"))
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(c.Checks["a"].Inputs, " "); got != "src/** web/** a/b/c **" {
		t.Fatalf("normalised inputs: %q", got)
	}
	if _, err := Parse([]byte("checks:\n  a:\n    run: x\n    inputs: ['src/../../x']\n")); err == nil || !strings.Contains(err.Error(), "inside the repository") {
		t.Fatalf("escaping input: %v", err)
	}

	f := newRepo(t, "checks:\n  dot:\n    run: echo x >> $C/dot\n    inputs: ['./src/']\n")
	f.run(f.opts())
	f.write("src/a.txt", "changed\n")
	f.run(f.opts())
	if f.count("dot") != 2 {
		t.Fatalf("./src/ did not key on src: count %d", f.count("dot"))
	}

	f = newRepo(t, "checks:\n  typo:\n    run: echo x >> $C/typo\n    inputs: ['scr/**']\n")
	if _, err := Run(context.Background(), f.opts()); err == nil || !strings.Contains(err.Error(), "match no file") {
		t.Fatalf("inputs matching nothing: %v", err)
	}
	if f.count("typo") != 0 {
		t.Fatal("a check keyed on nothing ran")
	}
}

func TestOnlyManualChecksIsUnsettled(t *testing.T) {
	t.Parallel()
	f := newRepo(t, "checks:\n  slow:\n    run: echo x >> $C/slow\n    manual: true\n")
	rep := f.run(f.opts())
	if !rep.NothingSelected() || rep.ExitCode() != 2 || f.count("slow") != 0 {
		t.Fatalf("all-manual config: %+v exit %d", rep.Checks, rep.ExitCode())
	}
}
