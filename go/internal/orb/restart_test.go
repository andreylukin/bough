package orb

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/projectdef"
)

// restartFixture is a running orb for session s1 with a resume.sh, so
// every start leaves a marker in resume.log.
func restartFixture(t *testing.T, slug string) (string, string, *container.Fake, *Orb) {
	t.Helper()
	home := t.TempDir()
	src := newRepo(t)
	newProject(t, home, slug, "  - path: "+src+"\n")
	if err := projectdef.WriteFile(home, slug, projectdef.FileResume, "#!/bin/sh\necho resumed\n"); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		t.Fatal(err)
	}
	rt := container.NewFake()
	o, err := Open(context.Background(), rt, home, "s1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	return home, src, rt, o
}

// setYAML rewrites project.yml with the fixture's repo plus extra lines.
func setYAML(t *testing.T, home, slug, src, extra string) projectdef.Project {
	t.Helper()
	if err := projectdef.WriteFile(home, slug, projectdef.FileYAML, "repos:\n  - path: "+src+"\ncaches: [/root/.cache]\n"+extra); err != nil {
		t.Fatal(err)
	}
	p, err := projectdef.Load(home, slug)
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func resumeStarts(t *testing.T, home string) int {
	t.Helper()
	b, _ := os.ReadFile(filepath.Join(Dir(home, "s1"), "resume.log"))
	return strings.Count(string(b), "== resume.sh start")
}

// Nothing changed: the same container stops and starts, resume.sh reruns.
func TestReplaceSameSpecRestartsInPlace(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home, _, rt, o := restartFixture(t, "same")
	p, _ := projectdef.Load(home, "same")
	next, err := o.Successor(ctx, p)
	if err != nil {
		t.Fatal(err)
	}
	res, err := o.Replace(ctx, next, false)
	if err != nil {
		t.Fatal(err)
	}
	calls := rt.CallList()
	if res.Recreated || count(calls, "remove ") != 0 || count(calls, "stop ") != 1 || count(calls, "start ") != 2 {
		t.Fatalf("recreated=%v calls %v", res.Recreated, calls)
	}
	if n := resumeStarts(t, home); n != 2 {
		t.Fatalf("resume.sh ran %d times", n)
	}
	if st, _ := ReadState(home, "s1"); st.Status != StatusRunning || st.Spec == "" || st.Spec != o.State().Spec {
		t.Fatalf("state after = %+v (old spec %q)", st, o.State().Spec)
	}
	if !strings.Contains(ResumeTail(home, "s1", 20), "resumed") || strings.Count(ResumeTail(home, "s1", 20), "resume.sh start") != 1 {
		t.Fatalf("resume tail = %q", ResumeTail(home, "s1", 20))
	}
}

// A project.yml change the image hash ignores (memory) still recreates
// the container: the spec, not only the image, decides.
func TestReplaceSpecOnlyChangeRecreates(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home, src, rt, o := restartFixture(t, "mem")
	before := rt.CallList()
	p := setYAML(t, home, "mem", src, "memory: 8G\n")
	next, err := o.Successor(ctx, p)
	if err != nil {
		t.Fatal(err)
	}
	res, err := o.Replace(ctx, next, false)
	if err != nil {
		t.Fatal(err)
	}
	calls := rt.CallList()
	if count(calls, "commit ") != count(before, "commit ") || count(calls, "build ") != count(before, "build ") {
		t.Fatalf("image rebuilt for a spec-only change: %v", calls)
	}
	if !res.Recreated || count(calls, "remove "+container.OrbName("s1")) != 1 || rt.LastRun.Memory != "8G" {
		t.Fatalf("recreated=%v memory=%q calls %v", res.Recreated, rt.LastRun.Memory, calls)
	}
	if !slices.Contains(res.Why, "memory") || res.Image != res.PrevImage {
		t.Fatalf("result %+v", res)
	}
	if st, _ := ReadState(home, "s1"); st.Spec == o.State().Spec || st.Spec == "" {
		t.Fatalf("spec key unchanged: %q", st.Spec)
	}
}

// A setup.sh change builds a new image; the new orb execs in the new
// container and the old one refuses to bring its container back.
func TestReplaceImageChangeRecreatesAndExecsNew(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home, _, rt, o := restartFixture(t, "img2")
	projectdef.WriteFile(home, "img2", projectdef.FileSetup, "#!/bin/sh\necho v2\n")
	p, _ := projectdef.Load(home, "img2")
	next, err := o.Successor(ctx, p)
	if err != nil {
		t.Fatal(err)
	}
	if next.spec.Image == o.State().Image {
		t.Fatal("setup.sh change kept the image tag")
	}
	res, err := o.Replace(ctx, next, false)
	if err != nil {
		t.Fatal(err)
	}
	if !res.Recreated || rt.LastRun.Image != next.spec.Image || res.Image != next.spec.Image || !slices.Contains(res.Why, "image") {
		t.Fatalf("result %+v last run %s", res, rt.LastRun.Image)
	}
	if err := next.Command(ctx, "true").Run(); err != nil {
		t.Fatalf("new orb exec: %v", err)
	}
	if err := o.Command(ctx, "true").Run(); err == nil || !strings.Contains(err.Error(), "orb replaced") {
		t.Fatalf("old orb exec = %v, want orb replaced", err)
	}
}

// A build failure is the successor's alone: the running container is
// never stopped, and state.json still describes it.
func TestSuccessorBuildFailureLeavesOldRunning(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home, _, rt, o := restartFixture(t, "bf")
	old := o.State()
	projectdef.WriteFile(home, "bf", projectdef.FileSetup, "#!/bin/sh\nexit 1\n")
	p, _ := projectdef.Load(home, "bf")
	rt.FailBuild, rt.FailBuildLog = errors.New("step failed"), "E: package nope not found\n"
	_, err := o.Successor(ctx, p)
	if err == nil || !strings.Contains(err.Error(), "package nope not found") || !strings.Contains(err.Error(), "restart s1") {
		t.Fatalf("successor err = %v", err)
	}
	calls := rt.CallList()
	if count(calls, "stop ") != 0 || count(calls, "remove ") != 0 {
		t.Fatalf("a failed build touched the container: %v", calls)
	}
	if st, _ := rt.Inspect(ctx, container.OrbName("s1")); st != container.StateRunning {
		t.Fatalf("container %s", st)
	}
	if st, _ := ReadState(home, "s1"); st.Status != StatusRunning || st.Image != old.Image {
		t.Fatalf("state.json = %+v", st)
	}
}

// failImageStart fails Start for one image only: the new container.
type failImageStart struct {
	*container.Fake
	bad string
}

func (r *failImageStart) Start(ctx context.Context, spec container.RunSpec) error {
	if spec.Image == r.bad {
		return fmt.Errorf("no room for %s", spec.Image)
	}
	return r.Fake.Start(ctx, spec)
}

// The new container not starting brings the old one back: the session
// is never left without a shell.
func TestReplaceStartFailureBringsOldBack(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home := t.TempDir()
	newProject(t, home, "sf", "  - path: "+newRepo(t)+"\n")
	p, _ := projectdef.Load(home, "sf")
	rt := &failImageStart{Fake: container.NewFake()}
	o, err := Open(ctx, rt, home, "s1", p, t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	oldTag := o.State().Image
	projectdef.WriteFile(home, "sf", projectdef.FileSetup, "#!/bin/sh\necho v2\n")
	p, _ = projectdef.Load(home, "sf")
	next, err := o.Successor(ctx, p)
	if err != nil {
		t.Fatal(err)
	}
	rt.bad = next.spec.Image
	_, err = o.Replace(ctx, next, false)
	if err == nil || !strings.Contains(err.Error(), "no room") || !strings.Contains(err.Error(), "old container runs again") {
		t.Fatalf("replace err = %v", err)
	}
	if rt.LastRun.Image != oldTag {
		t.Fatalf("last run %s, want the old %s", rt.LastRun.Image, oldTag)
	}
	if st, _ := rt.Inspect(ctx, container.OrbName("s1")); st != container.StateRunning {
		t.Fatalf("container %s", st)
	}
	if err := o.Command(ctx, "true").Run(); err != nil {
		t.Fatalf("old orb exec after fallback: %v", err)
	}
	if st, _ := ReadState(home, "s1"); st.Status != StatusRunning || st.Image != oldTag {
		t.Fatalf("state.json = %+v", st)
	}
}

// A plain start recreates a container whose spec changed (env here, which
// the image hash ignores); a state from before specs were recorded is
// trusted on its image, as before.
func TestStartRecreatesOnSpecChange(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home, src, rt, o := restartFixture(t, "env")
	if err := o.Stop(ctx); err != nil {
		t.Fatal(err)
	}
	p := setYAML(t, home, "env", src, "env:\n  FOO: bar\n")
	if _, err := Open(ctx, rt, home, "s1", p, o.scratch); err != nil {
		t.Fatal(err)
	}
	if n := count(rt.CallList(), "remove "+container.OrbName("s1")); n != 1 || !slices.Contains(rt.LastRun.Env, "FOO=bar") {
		t.Fatalf("env change: removes %d env %v", n, rt.LastRun.Env)
	}
	// Unchanged: reused.
	if _, err := Open(ctx, rt, home, "s1", p, o.scratch); err != nil {
		t.Fatal(err)
	}
	if n := count(rt.CallList(), "remove "); n != 1 {
		t.Fatalf("unchanged spec recreated: %v", rt.CallList())
	}
	// An old state.json with no spec key: reused even though env changed.
	st, _ := ReadState(home, "s1")
	st.Spec = ""
	writeState(home, st)
	p = setYAML(t, home, "env", src, "env:\n  FOO: baz\n")
	if _, err := Open(ctx, rt, home, "s1", p, o.scratch); err != nil {
		t.Fatal(err)
	}
	if n := count(rt.CallList(), "remove "); n != 1 {
		t.Fatalf("a pre-spec container was recreated: %v", rt.CallList())
	}
}

func TestSuccessorRefusesPrimaryChange(t *testing.T) {
	t.Parallel()
	ctx := context.Background()
	home, src, rt, o := restartFixture(t, "prim")
	other := newRepo(t)
	if err := projectdef.WriteFile(home, "prim", projectdef.FileYAML, "repos:\n  - path: "+other+"\n    name: other\n  - path: "+src+"\n"); err != nil {
		t.Fatal(err)
	}
	p, _ := projectdef.Load(home, "prim")
	before := len(rt.CallList())
	_, err := o.Successor(ctx, p)
	if err == nil || !strings.Contains(err.Error(), "first repo changed") {
		t.Fatalf("err = %v", err)
	}
	if len(rt.CallList()) != before {
		t.Fatalf("refused restart touched the runtime: %v", rt.CallList()[before:])
	}
}

func TestRestartRequestFile(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, ok := TakeRestart(home, "s1"); ok {
		t.Fatal("take with no request")
	}
	if err := RequestRestart(home, "s1", RestartRequest{Fresh: true, By: "cli"}); err != nil {
		t.Fatal(err)
	}
	if err := RequestRestart(home, "s1", RestartRequest{By: "agent"}); err != nil {
		t.Fatal(err)
	}
	r, ok := TakeRestart(home, "s1")
	if !ok || !r.Fresh || r.By != "agent" || r.At.IsZero() {
		t.Fatalf("taken %+v %v", r, ok)
	}
	if _, ok := TakeRestart(home, "s1"); ok {
		t.Fatal("request taken twice")
	}
	if err := RequestRestart(home, "../x", RestartRequest{}); err == nil {
		t.Fatal("bad session accepted")
	}
}

// A retarget re-points an open portal at the new container's address and
// keeps its host port, so the URL the person has still works.
func TestRetargetPortals(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			c.Write([]byte("hi"))
			c.Close()
		}
	}()
	gp := ln.Addr().(*net.TCPAddr).Port
	session := "retarget-" + strconv.Itoa(gp)
	running(t, home, session, "10.9.9.9")
	ps, err := OpenPortal(home, session, gp, "web")
	if err != nil {
		t.Fatal(err)
	}
	defer ClosePortal(home, session, gp)
	RetargetPortals(session, "127.0.0.1")
	portals.Lock()
	p := portals.open[session][gp]
	portals.Unlock()
	if got := *p.dial.Load(); got != net.JoinHostPort("127.0.0.1", strconv.Itoa(gp)) || hostPort(p.ln) != ps.Host {
		t.Fatalf("dial %s host %d (was %d)", got, hostPort(p.ln), ps.Host)
	}
	c, err := net.DialTimeout("tcp", "127.0.0.1:"+strconv.Itoa(ps.Host), 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	c.SetReadDeadline(time.Now().Add(2 * time.Second))
	buf := make([]byte, 2)
	if _, err := c.Read(buf); err != nil || string(buf) != "hi" {
		t.Fatalf("through the retargeted portal: %q %v", buf, err)
	}
}
