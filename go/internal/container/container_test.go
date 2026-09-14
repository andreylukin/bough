package container

import (
	"bytes"
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func TestFakeLifecycle(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	f := NewFake()
	if err := f.Start(ctx, RunSpec{Name: "n", Image: "img"}); err == nil {
		t.Fatal("start without image should fail")
	}
	var log bytes.Buffer
	if err := f.Build(ctx, BuildSpec{Tag: "img"}, &log); err != nil {
		t.Fatal(err)
	}
	if ok, _ := f.ImageExists(ctx, "img"); !ok {
		t.Fatal("image missing after build")
	}
	if err := f.Start(ctx, RunSpec{Name: "n", Image: "img"}); err != nil {
		t.Fatal(err)
	}
	if s, _ := f.Inspect(ctx, "n"); s != StateRunning {
		t.Fatalf("state %s", s)
	}
	dir := t.TempDir()
	out, err := f.Command(ctx, "n", ExecOptions{Workdir: dir, Env: []string{"ORB_X=1"}}, "sh", "-c", "pwd; echo $ORB_X").Output()
	if err != nil {
		t.Fatal(err)
	}
	real, _ := filepath.EvalSymlinks(dir)
	if got := string(out); !strings.Contains(got, real) && !strings.Contains(got, dir) || !strings.HasSuffix(got, "1\n") {
		t.Fatalf("exec output %q", got)
	}
	if err := f.Stop(ctx, "n"); err != nil {
		t.Fatal(err)
	}
	if s, _ := f.Inspect(ctx, "n"); s != StateStopped {
		t.Fatalf("state %s", s)
	}
	if err := f.Command(ctx, "n", ExecOptions{}, "true").Run(); err == nil {
		t.Fatal("exec on stopped container should fail")
	}
	_ = f.Remove(ctx, "n")
	if s, _ := f.Inspect(ctx, "n"); s != StateMissing {
		t.Fatalf("state %s", s)
	}
	f.FailBuild = errors.New("boom")
	if err := f.Build(ctx, BuildSpec{Tag: "x"}, nil); err == nil {
		t.Fatal("FailBuild ignored")
	}
	if len(f.CallList()) == 0 {
		t.Fatal("no calls recorded")
	}
}

func TestStubsNotImplemented(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	for _, r := range []Runtime{Nerdctl{}, Podman{}, Unsupported{OS: "plan9"}} {
		if err := r.Available(ctx); !errors.Is(err, ErrNotImplemented) || !strings.Contains(err.Error(), r.Name()[:4]) {
			t.Fatalf("%s: %v", r.Name(), err)
		}
		if err := r.Command(ctx, "n", ExecOptions{}, "true").Run(); !errors.Is(err, ErrNotImplemented) {
			t.Fatalf("%s command: %v", r.Name(), err)
		}
	}
}

func TestPick(t *testing.T) {
	t.Parallel()
	none := func(string) (string, error) { return "", exec.ErrNotFound }
	podman := func(n string) (string, error) {
		if n == "podman" {
			return "/bin/podman", nil
		}
		return "", exec.ErrNotFound
	}
	if pick("darwin", none).Name() != "apple" || pick("linux", podman).Name() != "podman" || pick("linux", none).Name() != "unsupported" {
		t.Fatal("pick mismatch")
	}
	if OrbName("s1") != "bough-orb-s1" {
		t.Fatal(OrbName("s1"))
	}
}

// fakeBin writes a script that appends its argv to a file and prints
// stdout from $FAKE_OUT, standing in for the container CLI.
func fakeBin(t *testing.T, stdout string, exit int) (*Apple, string) {
	dir := t.TempDir()
	rec := filepath.Join(dir, "argv")
	bin := filepath.Join(dir, "container")
	script := "#!/bin/sh\necho \"$*\" >> " + rec + "\nprintf '%s' '" + stdout + "'\nexit " + string(rune('0'+exit)) + "\n"
	if err := os.WriteFile(bin, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	return &Apple{Bin: bin}, rec
}

func readLines(t *testing.T, p string) []string {
	b, _ := os.ReadFile(p)
	return strings.Split(strings.TrimSpace(string(b)), "\n")
}

func TestAppleArgv(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	got := runArgs(RunSpec{Name: "n", Image: "img", Workdir: "/w", Env: []string{"A=1"}, CPUs: 2, Memory: "8G",
		Mounts: []Mount{{Source: "/h", Target: "/h"}, {Source: "/r", Target: "/r", ReadOnly: true}, {Source: "vol", Target: "/c", Volume: true}}})
	want := []string{"run", "-d", "--name", "n", "-v", "/h:/h", "-v", "/r:/r:ro", "--mount", "type=volume,source=vol,target=/c",
		"-w", "/w", "-e", "A=1", "-c", "2", "-m", "8G", "img", "sleep", "infinity"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("run args\n%v\n%v", got, want)
	}
	if e := execArgs("n", ExecOptions{Interactive: true, Workdir: "/w", Env: []string{"B=2"}}, []string{"ls", "-la"}); !reflect.DeepEqual(e, []string{"exec", "-i", "-w", "/w", "-e", "B=2", "n", "ls", "-la"}) {
		t.Fatal(e)
	}

	// Each Command carries its own exec id so KillFunc targets only it.
	c1, c2 := (&Apple{Bin: "container"}).Command(ctx, "n", ExecOptions{Env: []string{"B=2"}}, "ls"), (&Apple{Bin: "container"}).Command(ctx, "n", ExecOptions{}, "ls")
	if id1, id2 := execID(c1), execID(c2); id1 == "" || id1 == id2 || c1.Args[len(c1.Args)-2] != "n" {
		t.Fatalf("exec ids %q %q args %q", id1, id2, c1.Args)
	}

	// Missing container: inspect fails => run; Remove is a no-op.
	a, rec := fakeBin(t, "", 1)
	_ = a.Start(ctx, RunSpec{Name: "n", Image: "img"})
	if err := a.Remove(ctx, "n"); err != nil {
		t.Fatal(err)
	}
	if l := readLines(t, rec); len(l) != 3 || l[0] != "inspect n" || !strings.HasPrefix(l[1], "run -d --name n") || l[2] != "inspect n" {
		t.Fatalf("calls %q", l)
	}

	// Stopped container: start instead of run.
	a, rec = fakeBin(t, `[{"status":"stopped"}]`, 0)
	if err := a.Start(ctx, RunSpec{Name: "n", Image: "img"}); err != nil {
		t.Fatal(err)
	}
	if l := readLines(t, rec); l[len(l)-1] != "start n" {
		t.Fatalf("calls %q", l)
	}

	var log bytes.Buffer
	if err := a.Build(ctx, BuildSpec{Dir: "/ctx", Tag: "t"}, &log); err != nil {
		t.Fatal(err)
	}
	if l := readLines(t, rec); l[len(l)-1] != "build -t t -f /ctx/Dockerfile --progress plain /ctx" {
		t.Fatalf("calls %q", l)
	}
	if !strings.Contains(log.String(), "stopped") {
		t.Fatalf("build output not streamed: %q", log.String())
	}
}

func TestParseInspect(t *testing.T) {
	t.Parallel()
	for in, want := range map[string]State{"": StateMissing, "[]": StateMissing, `[{"status":"running"}]`: StateRunning, `[{"status":"stopped"}]`: StateStopped} {
		if got, err := parseInspect([]byte(in)); err != nil || got != want {
			t.Fatalf("%q: %s %v", in, got, err)
		}
	}
}

func TestCommitContext(t *testing.T) {
	t.Parallel()
	src := t.TempDir()
	script := filepath.Join(src, "setup.sh")
	lock := filepath.Join(src, "api", "go.sum")
	_ = os.MkdirAll(filepath.Dir(lock), 0o755)
	_ = os.WriteFile(script, []byte("echo hi"), 0o644)
	_ = os.WriteFile(lock, []byte("sum"), 0o644)
	dir := t.TempDir()
	if err := writeCommitContext(dir, CommitSpec{Script: script, Files: []string{lock}, Env: []string{"K=v w"}}); err != nil {
		t.Fatal(err)
	}
	df, _ := os.ReadFile(filepath.Join(dir, "Dockerfile"))
	if want := "FROM " + DefaultBase + "\nENV K=\"v w\"\nCOPY . /bough-setup\nRUN sh /bough-setup/setup.sh\n"; string(df) != want {
		t.Fatalf("dockerfile %q", df)
	}
	if _, err := os.Stat(filepath.Join(dir, "api", "go.sum")); err != nil {
		t.Fatal(err)
	}
}

// With FilesRoot, files keep their relative path: two subdirectory
// lockfiles of one name land apart.
func TestCommitContextFilesRoot(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	script := filepath.Join(root, "setup.sh")
	os.WriteFile(script, []byte("true\n"), 0o644)
	a := filepath.Join(root, "app", "go", "go.sum")
	b := filepath.Join(root, "app", "tools", "go.sum")
	for _, f := range []string{a, b} {
		os.MkdirAll(filepath.Dir(f), 0o755)
		os.WriteFile(f, []byte(f), 0o644)
	}
	dir := t.TempDir()
	if err := writeCommitContext(dir, CommitSpec{Script: script, Files: []string{a, b}, FilesRoot: root}); err != nil {
		t.Fatal(err)
	}
	for _, rel := range []string{"app/go/go.sum", "app/tools/go.sum"} {
		if got, err := os.ReadFile(filepath.Join(dir, rel)); err != nil || !strings.HasSuffix(string(got), rel) {
			t.Errorf("%s = %q, %v", rel, got, err)
		}
	}
}
