package orb

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

func git(t *testing.T, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", dir, "-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"}, args...)...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

func newRepo(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	git(t, dir, "init", "-q", "-b", "main")
	os.WriteFile(filepath.Join(dir, "go.sum"), []byte("v1\n"), 0o644)
	os.WriteFile(filepath.Join(dir, "README"), []byte("hi\n"), 0o644)
	git(t, dir, "add", "-A")
	git(t, dir, "commit", "-qm", "init")
	return dir
}

// newProject writes a definition for the given repos lines.
func newProject(t *testing.T, home, slug, reposYAML string) projectdef.Project {
	t.Helper()
	if _, err := projectdef.Create(home, slug); err != nil {
		t.Fatal(err)
	}
	if err := projectdef.WriteFile(home, slug, projectdef.FileYAML, "repos:\n"+reposYAML+"caches: [/root/.cache]\n"); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		t.Fatal(err)
	}
	return p
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

func TestEnsureImage(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	p := newProject(t, home, "img", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	tag, err := EnsureImage(ctx, rt, home, p, nil)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(tag, "bough-orb/img:") || count(rt.CallList(), "commit ") != 1 {
		t.Fatalf("tag %s calls %v", tag, rt.CallList())
	}
	b, _ := ReadBuild(home, "img")
	if b.State != "ok" || b.Tag != tag {
		t.Fatalf("build %+v", b)
	}
	// A waiter (image already there) must not truncate the log.
	os.WriteFile(ImageLogPath(home, "img"), []byte("keep"), 0o644)
	if _, err := EnsureImage(ctx, rt, home, p, nil); err != nil {
		t.Fatal(err)
	}
	if got, _ := os.ReadFile(ImageLogPath(home, "img")); string(got) != "keep" || count(rt.CallList(), "commit ") != 1 {
		t.Fatalf("second call rebuilt or truncated: %q", got)
	}
	// A Dockerfile wins and changes the tag.
	projectdef.WriteFile(home, "img", projectdef.FileDockerfile, "FROM debian\n")
	if _, err := EnsureImage(ctx, rt, home, p, nil); err != nil || count(rt.CallList(), "build ") != 1 {
		t.Fatalf("dockerfile build: %v %v", err, rt.CallList())
	}
}

func TestEnsureImageConcurrent(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "conc", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	var wg sync.WaitGroup
	for range 8 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if _, err := EnsureImage(context.Background(), rt, home, p, nil); err != nil {
				t.Error(err)
			}
		}()
	}
	wg.Wait()
	if n := count(rt.CallList(), "commit "); n != 1 {
		t.Fatalf("built %d times", n)
	}
}

func TestEnsureImageFailed(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "bad", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	rt.FailBuild = errors.New("boom")
	var tee strings.Builder
	if _, err := EnsureImage(context.Background(), rt, home, p, &tee); err == nil {
		t.Fatal("no error")
	}
	b, _ := ReadBuild(home, "bad")
	if b.State != "failed" || !strings.Contains(b.Error, "boom") {
		t.Fatalf("build %+v", b)
	}
	log, _ := os.ReadFile(ImageLogPath(home, "bad"))
	if !strings.Contains(string(log), "fake commit") || !strings.Contains(string(log), "boom") || !strings.Contains(tee.String(), "fake commit") {
		t.Fatalf("log %q tee %q", log, tee.String())
	}
}

func TestOpenPathRepo(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	src := newRepo(t)
	p := newProject(t, home, "web", "  - path: "+src+"\n    branch: main\n")
	scratch := t.TempDir()
	rt := container.NewFake()

	o, err := Open(ctx, rt, home, "s1", p, scratch)
	if err != nil {
		t.Fatal(err)
	}
	st := o.State()
	wt := filepath.Join(home, ".bough", "orbs", "s1", filepath.Base(src))
	if st.Status != StatusRunning || st.Primary != wt || o.Root() != Dir(home, "s1") {
		t.Fatalf("state %+v", st)
	}
	if b := git(t, wt, "branch", "--show-current"); b != "bough/s1" {
		t.Fatalf("branch %q", b)
	}
	if onDisk, _ := ReadState(home, "s1"); onDisk.Status != StatusRunning || onDisk.PID != os.Getpid() {
		t.Fatalf("state.json %+v", onDisk)
	}
	if count(rt.CallList(), "volume bough-cache-web-0") != 1 {
		t.Fatalf("no cache volume: %v", rt.CallList())
	}

	out, err := o.Command(ctx, "sh", "-c", "pwd; echo $BOUGH_SCRATCH").Output()
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Fields(string(out)); len(got) != 2 || !strings.HasSuffix(got[0], filepath.Base(src)) || got[1] != scratch {
		t.Fatalf("exec out %q", out)
	}

	// Stopped behind our back: the next Command restarts it.
	os.WriteFile(filepath.Join(wt, "dirty.txt"), []byte("x"), 0o644)
	if err := o.Stop(ctx); err != nil {
		t.Fatal(err)
	}
	if s, _ := ReadState(home, "s1"); s.Status != StatusStopped {
		t.Fatalf("after stop %s", s.Status)
	}
	if err := o.Command(ctx, "true").Run(); err != nil {
		t.Fatalf("restart exec: %v", err)
	}
	if o.State().Status != StatusRunning {
		t.Fatal("not running after restart")
	}

	// Resume: same worktree reused, uncommitted work kept.
	o2, err := Open(ctx, rt, home, "s1", p, scratch)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(o2.State().Primary, "dirty.txt")); err != nil {
		t.Fatal("resume did not reuse worktree")
	}
	if n := count(rt.CallList(), "remove "); n != 0 {
		t.Fatalf("same image removed container: %v", rt.CallList())
	}

	// A changed recipe means a new tag: the old container must be replaced.
	projectdef.WriteFile(home, "web", projectdef.FileSetup, "#!/bin/sh\necho v2\n")
	if _, err := Open(ctx, rt, home, "s1", p, scratch); err != nil {
		t.Fatal(err)
	}
	if n := count(rt.CallList(), "remove "+container.OrbName("s1")); n != 1 {
		t.Fatalf("stale container not removed: %v", rt.CallList())
	}

	if err := Remove(ctx, rt, home, "s1"); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(Dir(home, "s1")); !os.IsNotExist(err) {
		t.Fatal("orb dir kept")
	}
	if list := git(t, src, "worktree", "list"); strings.Contains(list, "bough/s1") {
		t.Fatalf("worktree still registered: %s", list)
	}
	if st, _ := rt.Inspect(ctx, container.OrbName("s1")); st != container.StateMissing {
		t.Fatal("container kept")
	}
}

func TestOpenResumeFailureUsable(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	p := newProject(t, home, "rf", "  - path: "+newRepo(t)+"\n")
	projectdef.WriteFile(home, "rf", projectdef.FileResume, "#!/bin/sh\necho resuming\nexit 3\n")
	rt := container.NewFake()
	o, err := Open(ctx, rt, home, "s2", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	if st := o.State(); st.Status != StatusFailed || !strings.Contains(st.Error, "resume.sh") {
		t.Fatalf("state %+v", st)
	}
	if log, _ := os.ReadFile(filepath.Join(Dir(home, "s2"), "resume.log")); !strings.Contains(string(log), "resuming") {
		t.Fatalf("resume.log %q", log)
	}
	if err := o.Command(ctx, "true").Run(); err != nil {
		t.Fatalf("orb unusable: %v", err)
	}
}

func TestOpenRemoteRepo(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	src := newRepo(t)
	p := newProject(t, home, "rem", "  - remote: "+src+"\n    name: app\n")
	rt := container.NewFake()
	o, err := Open(ctx, rt, home, "s3", p, "")
	if err != nil {
		t.Fatal(err)
	}
	wt := o.State().Worktrees["app"]
	if _, err := os.Stat(filepath.Join(wt, "go.sum")); err != nil {
		t.Fatalf("worktree %s: %v", wt, err)
	}
	if _, err := os.Stat(projectdef.CacheGitDir(home, "rem", p.Def.Repos[0])); err != nil {
		t.Fatal("no cache clone")
	}
}

func TestOpenFailureWritesState(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "ff", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	rt.FailBuild = errors.New("nope")
	if _, err := Open(context.Background(), rt, home, "s4", p, ""); err == nil || !strings.Contains(err.Error(), "orb: open s4") {
		t.Fatalf("err %v", err)
	}
	if st, _ := ReadState(home, "s4"); st.Status != StatusFailed || st.Error == "" {
		t.Fatalf("state %+v", st)
	}
	if st, err := ReadState(home, "missing"); err != nil || st.Status != StatusNone {
		t.Fatalf("missing state %+v %v", st, err)
	}
}
