package container

import (
	"bytes"
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
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
	want := []string{"run", "-d", "--init", "--name", "n", "-v", "/h:/h", "-v", "/r:/r:ro", "--mount", "type=volume,source=vol,target=/c",
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

	// Secret values never reach argv: -e NAME only, value via cmd.Env.
	c3 := (&Apple{Bin: "container"}).Command(ctx, "n", ExecOptions{Env: []string{"B=2"}, Secrets: []string{"DEVPI_TOKEN=fake-s3cr3t"}}, "true")
	if strings.Contains(strings.Join(c3.Args, " "), "fake-s3cr3t") || !slices.Contains(c3.Args, "DEVPI_TOKEN") {
		t.Fatalf("secret argv %q", c3.Args)
	}
	if !slices.Contains(c3.Env, "DEVPI_TOKEN=fake-s3cr3t") {
		t.Fatal("secret missing from the exec client's env")
	}

	// Missing container: inspect fails => run; Remove is a no-op.
	a, rec := fakeBin(t, "", 1)
	_ = a.Start(ctx, RunSpec{Name: "n", Image: "img"})
	if err := a.Remove(ctx, "n"); err != nil {
		t.Fatal(err)
	}
	if l := readLines(t, rec); len(l) != 3 || l[0] != "inspect n" || !strings.HasPrefix(l[1], "run -d --init --name n") || l[2] != "inspect n" {
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
	for in, want := range map[string]State{"": StateMissing, "[]": StateMissing, `[{"status":"running"}]`: StateRunning, `[{"status":"stopped"}]`: StateStopped, `[{"id":"n","status":{"state":"running","networks":[]}}]`: StateRunning, `[{"status":{"state":"stopped"}}]`: StateStopped} {
		if got, err := parseInspect([]byte(in)); err != nil || got != want {
			t.Fatalf("%q: %s %v", in, got, err)
		}
	}
}

// One COPY+RUN layer per step, no ENV, and only each step's declared
// files, copied right before its RUN.
func TestCommitContext(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	a := filepath.Join(root, "app", "go", "go.sum")
	b := filepath.Join(root, "app", "tools", "go.sum")
	for _, f := range []string{a, b} {
		os.MkdirAll(filepath.Dir(f), 0o755)
		os.WriteFile(f, []byte(f), 0o644)
	}
	dir := t.TempDir()
	spec := CommitSpec{FilesRoot: root, Steps: []Step{
		{Name: "apt", Script: []byte("#!/bin/bash\napt-get update\n")},
		{Name: "deps", Script: []byte("echo deps\n"), Files: []string{a}},
	}}
	if err := writeCommitContext(dir, spec); err != nil {
		t.Fatal(err)
	}
	df, _ := os.ReadFile(filepath.Join(dir, "Dockerfile"))
	want := "FROM " + DefaultBase + "\n" +
		"COPY steps/01-apt.sh /bough-setup/steps/\n" +
		"RUN bash /bough-setup/steps/01-apt.sh\n" +
		"COPY steps/02-deps.sh /bough-setup/steps/\n" +
		"COPY lock/app/go/go.sum /bough-setup/lock/app/go/go.sum\n" +
		"RUN sh /bough-setup/steps/02-deps.sh\n"
	if string(df) != want {
		t.Fatalf("dockerfile %q\nwant %q", df, want)
	}
	if got, err := os.ReadFile(filepath.Join(dir, "lock", "app", "go", "go.sum")); err != nil || string(got) != a {
		t.Fatalf("go.sum = %q, %v", got, err)
	}
	if _, err := os.Stat(filepath.Join(dir, "lock", "app", "tools", "go.sum")); err == nil {
		t.Fatal("undeclared file copied")
	}
	if got, _ := os.ReadFile(filepath.Join(dir, "steps", "01-apt.sh")); string(got) != "#!/bin/bash\napt-get update\n" {
		t.Fatalf("step script %q", got)
	}
	if err := writeCommitContext(t.TempDir(), CommitSpec{FilesRoot: root, Steps: []Step{{Name: "x", Files: []string{"/elsewhere"}}}}); err == nil {
		t.Fatal("file outside FilesRoot accepted")
	}
}

func TestParseImageLists(t *testing.T) {
	t.Parallel()
	tags, err := parseImageList([]byte(`[{"configuration":{"descriptor":{},"name":"bough-orb/a:1"},"id":"x"},{"configuration":{"name":"debian:bookworm"}}]`))
	if err != nil || strings.Join(tags, ",") != "bough-orb/a:1,debian:bookworm" {
		t.Fatalf("tags = %v, %v", tags, err)
	}
	refs, err := parseContainerImages([]byte(`[{"configuration":{"id":"bough-orb-s","image":{"descriptor":{},"reference":"bough-orb/a:1"}},"status":"running"}]`))
	if err != nil || strings.Join(refs, ",") != "bough-orb/a:1" {
		t.Fatalf("refs = %v, %v", refs, err)
	}
}

func TestPublishLoopbackOnly(t *testing.T) {
	t.Parallel()
	got := runArgs(RunSpec{Name: "n", Image: "img", Ports: []PortMap{{Host: 3000, Guest: 3000}, {Host: 8080, Guest: 80}}})
	want := []string{"run", "-d", "--init", "--name", "n", "-p", "127.0.0.1:3000:3000", "-p", "127.0.0.1:8080:80", "img", "sleep", "infinity"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("run args\n%v\n%v", got, want)
	}
}

// Shape from `container inspect` on 1.1.0: status.networks[].ipv4Address.
func TestParseAddress(t *testing.T) {
	t.Parallel()
	out := []byte(`[{"status":{"state":"running","networks":[{"hostname":"n","ipv4Address":"192.168.64.41/24","ipv4Gateway":"192.168.64.1","network":"default"}]}}]`)
	if ip, err := parseAddress(out); err != nil || ip != "192.168.64.41" {
		t.Fatalf("parseAddress = %q %v", ip, err)
	}
	if ip, err := parseAddress([]byte(`[{"status":{"state":"stopped","networks":[]}}]`)); err != nil || ip != "" {
		t.Fatalf("stopped = %q %v", ip, err)
	}
}
