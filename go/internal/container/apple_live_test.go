package container

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

// TestAppleLive exercises the real Apple CLI; it needs `container system
// start` and network for the base image, so it only runs on request.
func TestAppleLive(t *testing.T) {
	t.Parallel()
	if os.Getenv("BOUGH_LIVE_CONTAINER") != "1" {
		t.Skip("set BOUGH_LIVE_CONTAINER=1 to run against Apple container")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()
	a := NewApple()
	if err := a.Available(ctx); err != nil {
		t.Fatal(err)
	}
	id := fmt.Sprint(time.Now().UnixNano())
	name, tag, vol := "bough-orb-livetest-"+id, "bough-orb-livetest:"+id, "bough-orb-livetest-"+id
	// Image and volume too: every run has a fresh id, so leaving them
	// piles up hundreds of MB per run.
	t.Cleanup(func() {
		_ = a.Remove(context.Background(), name)
		_ = exec.Command(a.Bin, "image", "delete", tag).Run()
		_ = exec.Command(a.Bin, "volume", "delete", vol).Run()
	})

	bctx := t.TempDir()
	_ = os.WriteFile(filepath.Join(bctx, "Dockerfile"), []byte("FROM "+DefaultBase+"\nRUN apt-get update -qq && apt-get install -y -qq procps >/dev/null\n"), 0o644)
	var log bytes.Buffer
	if err := a.Build(ctx, BuildSpec{Dir: bctx, Tag: tag}, &log); err != nil {
		t.Fatalf("%v\n%s", err, log.String())
	}
	if ok, err := a.ImageExists(ctx, tag); err != nil || !ok {
		t.Fatalf("image exists %v %v", ok, err)
	}
	if ok, _ := a.ImageExists(ctx, tag+"-nope"); ok {
		t.Fatal("unknown image reported present")
	}
	if err := a.CreateVolume(ctx, vol); err != nil {
		t.Fatal(err)
	}
	if err := a.CreateVolume(ctx, vol); err != nil {
		t.Fatalf("second create: %v", err)
	}
	host := t.TempDir()
	if err := a.Start(ctx, RunSpec{Name: name, Image: tag, Mounts: []Mount{{Source: host, Target: host}, {Source: vol, Target: "/cache", Volume: true}}}); err != nil {
		t.Fatal(err)
	}
	if s, _ := a.Inspect(ctx, name); s != StateRunning {
		t.Fatalf("state %s", s)
	}
	run := func(argv ...string) string {
		out, err := a.Command(ctx, name, ExecOptions{Workdir: host}, argv...).CombinedOutput()
		if err != nil {
			t.Fatalf("%v: %v %s", argv, err, out)
		}
		return string(out)
	}
	if got := run("echo", "ok"); got != "ok\n" {
		t.Fatalf("echo %q", got)
	}
	run("sh", "-c", "echo hi > f && echo v > /cache/v && cat /cache/v")
	fi, err := os.Stat(filepath.Join(host, "f"))
	if err != nil {
		t.Fatal(err)
	}
	if uid := fi.Sys().(*syscall.Stat_t).Uid; int(uid) != os.Getuid() {
		t.Errorf("bind-mount file owned by %d, want host uid %d", uid, os.Getuid())
	}
	if got := run("sh", "-c", "echo ${BOUGH_LIVE_CONTAINER:-unset}"); got != "unset\n" {
		t.Errorf("host env leaked into exec: %q", got)
	}

	// Record whether killing only the client ends the guest process (the
	// contract's open question), then that KillFunc ends its own command
	// but not a sibling (a background job).
	bare := a.Command(ctx, name, ExecOptions{}, "sleep", "301")
	if err := bare.Start(); err != nil {
		t.Fatal(err)
	}
	time.Sleep(2 * time.Second)
	_ = bare.Process.Kill()
	_ = bare.Wait()
	time.Sleep(time.Second)
	t.Logf("client SIGKILL alone ends guest process: %v", !strings.Contains(run("ps", "-eo", "args"), "sleep 301"))
	// Anchored: an unanchored pattern also matches this sh's own argv
	// and pkill kills the shell running it (exit 143).
	run("sh", "-c", "pkill -f '^sleep 301' || true")

	sibling := a.Command(ctx, name, ExecOptions{}, "sleep", "302")
	sleeper := a.Command(ctx, name, ExecOptions{}, "sh", "-c", "sleep 300")
	if err := sibling.Start(); err != nil {
		t.Fatal(err)
	}
	if err := sleeper.Start(); err != nil {
		t.Fatal(err)
	}
	time.Sleep(2 * time.Second)
	_ = a.KillFunc(name, sleeper)()
	_ = sleeper.Wait()
	ps := run("ps", "-eo", "args")
	if strings.Contains(ps, "sleep 300") {
		t.Errorf("guest sleep survived kill:\n%s", ps)
	}
	if !strings.Contains(ps, "sleep 302") {
		t.Errorf("KillFunc killed a sibling command:\n%s", ps)
	}
	_ = a.KillFunc(name, sibling)()
	_ = sibling.Wait()

	if err := a.Stop(ctx, name); err != nil {
		t.Fatal(err)
	}
	if s, _ := a.Inspect(ctx, name); s != StateStopped {
		t.Fatalf("after stop %s", s)
	}
	if err := a.Start(ctx, RunSpec{Name: name, Image: tag}); err != nil {
		t.Fatal(err)
	}
	if got := run("cat", "/cache/v"); got != "v\n" {
		t.Errorf("volume content after restart %q", got)
	}
	if err := a.Remove(ctx, name); err != nil {
		t.Fatal(err)
	}
	if s, _ := a.Inspect(ctx, name); s != StateMissing {
		t.Fatalf("after remove %s", s)
	}
}
