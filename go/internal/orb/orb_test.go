package orb

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/internal/secrets"
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
	if _, err := EnsureImage(ctx, rt, home, p, nil); err != nil || count(rt.CallList(), "build bough-orb/img:") != 1 {
		t.Fatalf("dockerfile build: %v %v", err, rt.CallList())
	}
	// The setup-script build made the base once; the Dockerfile build did not need it.
	if n := count(rt.CallList(), "build "+projectdef.BaseTag()); n != 1 {
		t.Fatalf("base built %d times: %v", n, rt.CallList())
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
	// The base exists, so the failure is the project's own commit.
	if err := rt.Build(context.Background(), container.BuildSpec{Tag: projectdef.BaseTag()}, nil); err != nil {
		t.Fatal(err)
	}
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
	if fi, err := os.Stat(cacheDir(home, "web", "/root/.cache")); err != nil || !fi.IsDir() {
		t.Fatalf("no cache dir: %v", err)
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

// A container left from before, with no state.json saying which image it
// runs, is replaced, and a failed remove fails the open instead of
// silently restarting the stale container.
func TestOpenReplacesUnprovenContainer(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	p := newProject(t, home, "st", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	if _, err := Open(ctx, rt, home, "s5", p, ""); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(Dir(home, "s5"), stateFile)); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(ctx, rt, home, "s5", p, ""); err != nil {
		t.Fatal(err)
	}
	if n := count(rt.CallList(), "remove "+container.OrbName("s5")); n != 1 {
		t.Fatalf("container with no state not removed: %v", rt.CallList())
	}

	os.WriteFile(filepath.Join(Dir(home, "s5"), stateFile), []byte("{not json"), 0o644)
	rt.FailRemove = errors.New("engine busy")
	_, err := Open(ctx, rt, home, "s5", p, "")
	if err == nil || !strings.Contains(err.Error(), "engine busy") {
		t.Fatalf("open with a failed remove = %v", err)
	}
	if n := count(rt.CallList(), "start "); n != 2 {
		t.Fatalf("stale container started anyway: %v", rt.CallList())
	}
}

// killFake is a runtime with a guest-side kill, like Apple.
type killFake struct {
	*container.Fake
	mu     sync.Mutex
	killed []string
}

func (k *killFake) KillFunc(name string, cmd *exec.Cmd) func() error {
	return func() error {
		k.mu.Lock()
		k.killed = append(k.killed, name)
		k.mu.Unlock()
		return cmd.Process.Kill()
	}
}

// Cancelling an orb command reaches the runtime's guest kill: killing
// the host exec client alone leaves the guest process running.
func TestCommandCancelKillsGuest(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	p := newProject(t, home, "kg", "  - path: "+newRepo(t)+"\n")
	rt := &killFake{Fake: container.NewFake()}
	o, err := Open(context.Background(), rt, home, "s6", p, "")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cmd := o.Command(ctx, "sleep", "30")
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	cancel()
	cmd.Wait()
	rt.mu.Lock()
	defer rt.mu.Unlock()
	if len(rt.killed) != 1 || rt.killed[0] != container.OrbName("s6") {
		t.Fatalf("guest kill = %v", rt.killed)
	}
}

// Secrets reach resume.sh and Command through the exec env, re-read per
// exec, and never the container's run env. Not parallel: it swaps the
// keychain seam.
func TestExecEnvSecrets(t *testing.T) {
	old := secrets.KeychainRead
	t.Cleanup(func() { secrets.KeychainRead = old })
	secrets.KeychainRead = func(service string) (string, error) {
		switch service {
		case "bough/sec/DEVPI_URL":
			return "https://devpi.test/one", nil
		case "bough/sec/LATER":
			return "later-value", nil
		}
		return "", secrets.ErrNotFound
	}
	ctx := context.Background()
	home, scratch := t.TempDir(), t.TempDir()
	newProject(t, home, "sec", "  - path: "+newRepo(t)+"\n")
	projectdef.WriteFile(home, "sec", projectdef.FileResume, "#!/bin/sh\necho \"$DEVPI_URL\" > \"$BOUGH_SCRATCH/resume.out\"\n")
	for name, ref := range map[string]string{"DEVPI_URL": "keychain:bough/sec/DEVPI_URL", "GONE": "keychain:bough/sec/GONE"} {
		if err := projectdef.SetSecret(home, "sec", name, ref); err != nil {
			t.Fatal(err)
		}
	}
	p, err := projectdef.Load(home, "sec")
	if err != nil {
		t.Fatal(err)
	}
	o, err := Open(ctx, container.NewFake(), home, "s7", p, scratch)
	if err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(filepath.Join(scratch, "resume.out")); strings.TrimSpace(string(b)) != "https://devpi.test/one" {
		t.Fatalf("resume env %q", b)
	}
	for _, kv := range o.spec.Env {
		if strings.Contains(kv, "devpi.test") || strings.HasPrefix(kv, "DEVPI_URL=") {
			t.Fatalf("secret in RunSpec env: %v", o.spec.Env)
		}
	}
	// Added mid-session: the next Command sees it without reopening.
	if err := projectdef.SetSecret(home, "sec", "LATER", "keychain:bough/sec/LATER"); err != nil {
		t.Fatal(err)
	}
	out, err := o.Command(ctx, "sh", "-c", `echo "$DEVPI_URL|$LATER|${GONE-unset}"`).Output()
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.TrimSpace(string(out)); got != "https://devpi.test/one|later-value|unset" {
		t.Fatalf("command env %q", got)
	}
}

// A waiter re-hashes under the lock: a definition edited while it waited
// is built once, at the new hash, never the stale one.
func TestEnsureImageRehashUnderLock(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	p := newProject(t, home, "edit", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	rt.AddImage(projectdef.BaseTag())
	oldHash, err := projectdef.ImageHash(home, p)
	if err != nil {
		t.Fatal(err)
	}
	dir := imagesDir(home, "edit")
	os.MkdirAll(dir, 0o755)
	unlock, err := lockFile(filepath.Join(dir, "build.lock"))
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan string)
	go func() {
		tag, err := EnsureImage(ctx, rt, home, p, nil)
		if err != nil {
			t.Error(err)
		}
		done <- tag
	}()
	oldTag := projectdef.ImageTag("edit", oldHash)
	deadline := time.Now().Add(10 * time.Second)
	for count(rt.CallList(), "image-exists "+oldTag) == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if err := projectdef.WriteFile(home, "edit", projectdef.FileSetup, "#!/bin/sh\necho edited\n"); err != nil {
		t.Fatal(err)
	}
	unlock()
	tag := <-done
	newHash, _ := projectdef.ImageHash(home, p)
	if newHash == oldHash || tag != projectdef.ImageTag("edit", newHash) {
		t.Fatalf("tag %s, old %s new %s", tag, oldHash, newHash)
	}
	if n := count(rt.CallList(), "commit "); n != 1 || count(rt.CallList(), "commit "+oldTag) != 0 {
		t.Fatalf("calls %v", rt.CallList())
	}
}

// setup.sh steps reach the runtime as separate layers.
func TestEnsureImageSteps(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	repo := newRepo(t)
	p := newProject(t, home, "steps", "  - path: "+repo+"\n")
	if err := projectdef.WriteFile(home, "steps", projectdef.FileSetup, "#!/bin/bash\n# bough:step a\ntrue\n# bough:step b\n# bough:uses go.sum\ntrue\n"); err != nil {
		t.Fatal(err)
	}
	rt := container.NewFake()
	if _, err := EnsureImage(context.Background(), rt, home, p, nil); err != nil {
		t.Fatal(err)
	}
	s := rt.LastCommit.Steps
	if len(s) != 2 || s[0].Name != "a" || len(s[0].Files) != 0 || len(s[1].Files) != 1 || !strings.HasPrefix(string(s[1].Script), "#!/bin/bash\n") {
		t.Fatalf("steps %+v", s)
	}
	if want := filepath.Join(rt.LastCommit.FilesRoot, filepath.Base(repo), "go.sum"); s[1].Files[0] != want {
		t.Fatalf("file %s, want %s", s[1].Files[0], want)
	}
}

func TestCacheDirNames(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	a, b := cacheDir(home, "p", "/root/.cache/uv"), cacheDir(home, "p", "/cache/cargo")
	if a == b || a != cacheDir(home, "p", "/root/.cache/uv") || cacheDir(home, "q", "/root/.cache/uv") == a {
		t.Fatalf("names %s %s", a, b)
	}
	// Named by path, not index: reordering caches keeps each dir's bind,
	// every session of the project mounts the same one, and none is a
	// named volume (a block device two running VMs cannot share).
	names := func(caches []string) map[string]string {
		o := &Orb{rt: container.NewFake(), home: home, session: "s", project: projectdef.Project{Slug: "p", Dir: t.TempDir(), Def: projectdef.Def{Caches: caches}}}
		mounts, err := o.prepareMounts(context.Background())
		if err != nil {
			t.Fatal(err)
		}
		m := map[string]string{}
		for _, mt := range mounts {
			if mt.Volume {
				t.Fatalf("cache as volume: %+v", mt)
			}
			if slices.Contains(caches, mt.Target) {
				m[mt.Target] = mt.Source
			}
		}
		return m
	}
	x, y := names([]string{"/root/.cache/uv", "/cache/cargo"}), names([]string{"/cache/cargo", "/root/.cache/uv"})
	if len(x) != 2 || x["/root/.cache/uv"] != a || x["/cache/cargo"] != b || y["/root/.cache/uv"] != a || y["/cache/cargo"] != b {
		t.Fatalf("x %v y %v", x, y)
	}
}

func TestPruneImages(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	rt := container.NewFake()
	for _, tag := range []string{"bough-orb/a:1", "bough-orb/a:2", "bough-orb/a:3", "bough-orb/ab:1", "bough-orb/base:1"} {
		rt.AddImage(tag)
	}
	if err := rt.Start(ctx, container.RunSpec{Name: "c", Image: "bough-orb/a:2"}); err != nil {
		t.Fatal(err)
	}
	rt.Stop(ctx, "c")
	if err := pruneImages(ctx, rt, "a", "bough-orb/a:3"); err != nil {
		t.Fatal(err)
	}
	got, _ := rt.Images(ctx)
	if strings.Join(got, ",") != "bough-orb/a:2,bough-orb/a:3,bough-orb/ab:1,bough-orb/base:1" {
		t.Fatalf("images %v", got)
	}
	if pruneImages(ctx, rt, "base", "bough-orb/base:2"); count(rt.CallList(), "remove-image bough-orb/base") != 0 {
		t.Fatal("pruned bough's base image")
	}
}

// A waiter tails the running build's log, then stops when told.
func TestTailFile(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "build.log")
	os.WriteFile(path, []byte("one\n"), 0o644)
	var buf syncBuffer
	stop := tailFile(path, &buf)
	f, _ := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0o644)
	f.WriteString("two\n")
	f.Close()
	stop()
	if buf.String() != "one\ntwo\n" {
		t.Fatalf("tail %q", buf.String())
	}
}

type syncBuffer struct {
	mu sync.Mutex
	b  strings.Builder
}

func (s *syncBuffer) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.Write(p)
}

func (s *syncBuffer) String() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.String()
}

func TestOrbToken(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	p := newProject(t, home, "tk", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	envToken := func(o *Orb) string {
		out, err := o.Command(ctx, "sh", "-c", "printf %s \"$BOUGH_ORB_TOKEN\"").Output()
		if err != nil {
			t.Fatal(err)
		}
		return string(out)
	}
	tokenPath := filepath.Join(Dir(home, "s7"), tokenFile)

	o, err := Open(ctx, rt, home, "s7", p, "")
	if err != nil {
		t.Fatal(err)
	}
	fi, err := os.Stat(tokenPath)
	if err != nil || fi.Mode().Perm() != 0o600 {
		t.Fatalf("token file: %v %v", fi, err)
	}
	tok1, _ := os.ReadFile(tokenPath)
	if len(tok1) < 32 || o.State().ProxyAuth != ProxyAuthToken || envToken(o) != string(tok1) {
		t.Fatalf("fresh orb: token %q auth %q env %q", tok1, o.State().ProxyAuth, envToken(o))
	}
	if b, _ := os.ReadFile(filepath.Join(Dir(home, "s7"), stateFile)); strings.Contains(string(b), string(tok1)) {
		t.Fatal("token leaked into state.json")
	}

	// Reusing the container keeps its token.
	o, err = Open(ctx, rt, home, "s7", p, "")
	if err != nil {
		t.Fatal(err)
	}
	if tok, _ := os.ReadFile(tokenPath); string(tok) != string(tok1) || envToken(o) != string(tok1) {
		t.Fatalf("reuse changed token: %q", tok)
	}

	// A container created before tokens keeps running, unauthenticated,
	// and says how to fix it.
	os.Remove(tokenPath)
	o, err = Open(ctx, rt, home, "s7", p, "")
	if err != nil {
		t.Fatal(err)
	}
	if st := o.State(); st.ProxyAuth != ProxyAuthLegacy || envToken(o) != "" {
		t.Fatalf("legacy orb: %+v env %q", st, envToken(o))
	}
	if n := count(rt.CallList(), "remove "); n != 0 {
		t.Fatalf("legacy orb was recreated: %v", rt.CallList())
	}

	// Recreating it (a new image here) enables the token.
	projectdef.WriteFile(home, "tk", projectdef.FileSetup, "#!/bin/sh\necho v2\n")
	o, err = Open(ctx, rt, home, "s7", p, "")
	if err != nil {
		t.Fatal(err)
	}
	tok2, _ := os.ReadFile(tokenPath)
	if o.State().ProxyAuth != ProxyAuthToken || len(tok2) < 32 || string(tok2) == string(tok1) || envToken(o) != string(tok2) {
		t.Fatalf("recreated: auth %q token %q", o.State().ProxyAuth, tok2)
	}
}

// Open records one timed phase per start step in state.json; a failure
// marks the phase it stopped in, and a restart records its own steps.
func TestOpenRecordsPhases(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	p := newProject(t, home, "ph", "  - path: "+newRepo(t)+"\n")
	rt := container.NewFake()
	o, err := Open(ctx, rt, home, "p1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	want := []string{PhaseSync, PhaseBuild, PhaseWorktree, PhaseContainer, PhaseResume, PhaseReady}
	disk, _ := ReadState(home, "p1")
	if got := phaseNames(disk); !slices.Equal(got, want) || disk.Phase != PhaseReady {
		t.Fatalf("phases %v (phase %q), want %v", got, disk.Phase, want)
	}
	for _, ph := range disk.Phases {
		if ph.StartedAt.IsZero() || ph.EndedAt.IsZero() || ph.EndedAt.Before(ph.StartedAt) || ph.Error != "" {
			t.Fatalf("phase %+v not timed", ph)
		}
	}

	o.Stop(ctx)
	if err := o.Command(ctx, "true").Run(); err != nil {
		t.Fatal(err)
	}
	if got := phaseNames(o.State()); !slices.Equal(got, []string{PhaseContainer, PhaseResume, PhaseReady}) {
		t.Fatalf("restart phases %v", got)
	}

	rt2 := container.NewFake()
	rt2.FailBuild = errors.New("nope")
	projectdef.WriteFile(home, "ph", projectdef.FileSetup, "#!/bin/sh\necho v2\n")
	if _, err := Open(ctx, rt2, home, "p2", p, ""); err == nil {
		t.Fatal("want build failure")
	}
	st, _ := ReadState(home, "p2")
	last := st.Phases[len(st.Phases)-1]
	if st.Phase != PhaseBuild || last.Name != PhaseBuild || last.Error == "" || last.EndedAt.IsZero() {
		t.Fatalf("failed state %+v", st)
	}

	projectdef.WriteFile(home, "ph", projectdef.FileResume, "#!/bin/sh\nexit 3\n")
	if _, err := Open(ctx, rt, home, "p3", p, ""); err != nil {
		t.Fatal(err)
	}
	st, _ = ReadState(home, "p3")
	if st.Phase != PhaseResume || st.Phases[len(st.Phases)-1].Error == "" {
		t.Fatalf("resume failure %+v", st)
	}
}

// stopHookRT runs hook while the runtime is still stopping the container,
// the moment a killed exec returns and its job asks StoppedSince.
type stopHookRT struct {
	*container.Fake
	hook func()
}

func (r *stopHookRT) Stop(ctx context.Context, name string) error {
	r.hook()
	return r.Fake.Stop(ctx, name)
}

// A job killed by the session's own Stop (TUI /orb stop) must see the stop
// already, or it records a failure and wakes a paid turn.
func TestStopMarksStoppedBeforeJobsDie(t *testing.T) {
	ctx := context.Background()
	home := t.TempDir()
	src := newRepo(t)
	p := newProject(t, home, "web", "  - path: "+src+"\n    branch: main\n")
	rt := &stopHookRT{Fake: container.NewFake()}
	o, err := Open(ctx, rt, home, "s1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	seen := false
	rt.hook = func() { seen = o.StoppedSince(started) }
	if err := o.Stop(ctx); err != nil {
		t.Fatal(err)
	}
	if !seen {
		t.Fatal("a job dying mid-stop did not see StoppedSince")
	}
}
