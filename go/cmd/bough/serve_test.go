package main

import (
	"fmt"
	"net"
	"os"
	"path/filepath"
	"testing"
)

// serve asks about project sessions' containers (inspect, stop, the
// idle reaper) through the runtime their orb row picks, which they read
// from ~/.bough/bough.yml: a `runtime: fake` (or podman) row must not
// have serve talking to Apple's CLI about containers it never made.
func TestServeOrbRuntimeFollowsTheOrbRow(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if rt, err := serveOrbRuntime(home); err != nil || rt != nil {
		t.Fatalf("no config: runtime = %v, %v; want nil (the supervisor's default)", rt, err)
	}
	cfg := filepath.Join(home, ".bough", "bough.yml")
	if err := os.MkdirAll(filepath.Dir(cfg), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(cfg, []byte("- id: orb\n  plugin: orb\n  config:\n    runtime: fake\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if rt, err := serveOrbRuntime(home); err != nil || rt == nil || rt.Name() != "fake" {
		t.Fatalf("runtime: fake row: runtime = %v, %v", rt, err)
	}
	if err := os.WriteFile(cfg, []byte("- id: orb\n  plugin: orb\n  config:\n    runtime: nope\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := serveOrbRuntime(home); err == nil {
		t.Fatal("an unknown runtime on the orb row was accepted")
	}
}

func TestServeArgsHost(t *testing.T) {
	t.Parallel()
	_, _, _, host, err := serveArgs([]string{"--run", "9001", "--host=bough.example.ts.net"})
	if err != nil || host != "bough.example.ts.net" {
		t.Fatalf("host = %q, %v", host, err)
	}
}

func TestServeArgs(t *testing.T) {
	t.Parallel()
	cases := []struct {
		in         []string
		verb, addr string
		bad        bool
	}{
		{nil, "start", defaultServeAddr, false},
		{[]string{"start"}, "start", defaultServeAddr, false},
		{[]string{"status"}, "status", "", false},
		{[]string{"stop"}, "stop", "", false},
		{[]string{"127.0.0.1:9000"}, "start", "127.0.0.1:9000", false},
		// A bare port binds loopback, never a wildcard: the API has no
		// auth in this phase.
		{[]string{"9000"}, "start", "127.0.0.1:9000", false},
		{[]string{"--run"}, "--run", defaultServeAddr, false},
		{[]string{"--run", "127.0.0.1:9001"}, "--run", "127.0.0.1:9001", false},
		{[]string{"--run", "9001"}, "--run", "127.0.0.1:9001", false},
		{[]string{"--port"}, "", "", true},
		{[]string{"nonsense"}, "", "", true},
		{[]string{"a", "b"}, "", "", true},
		{[]string{"--run", "a", "b"}, "", "", true},
		{[]string{"--run", "--port"}, "", "", true},
		// Off loopback only with the explicit flag.
		{[]string{"0.0.0.0:9000"}, "", "", true},
		{[]string{":9000"}, "", "", true},
		{[]string{"--insecure-bind", "0.0.0.0:9000"}, "start", "0.0.0.0:9000", false},
		{[]string{"--run", "0.0.0.0:9000", "--insecure-bind"}, "--run", "0.0.0.0:9000", false},
		// A proxy's name rides along; it needs a value.
		{[]string{"--run", "9001", "--host=bough.example.ts.net"}, "--run", "127.0.0.1:9001", false},
		{[]string{"--host", "bough.example.ts.net"}, "start", defaultServeAddr, false},
		{[]string{"--host="}, "", "", true},
		{[]string{"--host"}, "", "", true},
	}
	for _, c := range cases {
		verb, addr, _, _, err := serveArgs(c.in)
		if (err != nil) != c.bad || verb != c.verb || addr != c.addr {
			t.Errorf("serveArgs(%v) = %q %q %v, want %q %q bad=%v", c.in, verb, addr, err, c.verb, c.addr, c.bad)
		}
	}
}

// The serve daemon must not land on the ports the other two surfaces
// own: 7683 is the artifacts page server, 7681 the web TUI.
func TestServeAddrIsItsOwnPort(t *testing.T) {
	t.Parallel()
	if defaultServeAddr == defaultWebAddr {
		t.Fatalf("serve reuses the web address %q", defaultServeAddr)
	}
	for _, taken := range []string{"7681", "7683"} {
		if _, port, err := net.SplitHostPort(defaultServeAddr); err != nil || port == taken {
			t.Fatalf("serve binds %q, which is already spoken for (%v)", defaultServeAddr, err)
		}
	}
}

func TestServePidfileRoundTrip(t *testing.T) {
	t.Parallel()
	home := t.TempDir()

	if _, ok := runningServe(home); ok {
		t.Fatal("an empty HOME reports a running daemon")
	}

	done, err := writeServePidfile(home, "127.0.0.1:7684")
	if done == nil || err != nil {
		t.Fatal("writeServePidfile refused to write into a clean HOME")
	}
	w, ok := runningServe(home)
	if !ok {
		t.Fatal("the pidfile we just wrote is not read back")
	}
	if w.pid != os.Getpid() || w.addr != "127.0.0.1:7684" {
		t.Fatalf("pidfile = pid %d addr %q", w.pid, w.addr)
	}
	if cwd, _ := os.Getwd(); w.dir != cwd {
		t.Errorf("dir = %q, want %q", w.dir, cwd)
	}

	done()
	if _, err := os.Stat(servePidfile(home)); !os.IsNotExist(err) {
		t.Errorf("the cleanup left the pidfile behind: %v", err)
	}
	if _, ok := runningServe(home); ok {
		t.Error("a removed pidfile still reports a running daemon")
	}
}

// A pidfile naming a dead pid is stale, and runningServe clears it —
// otherwise `bough serve` would refuse to start forever after a crash.
func TestRunningServeClearsAStalePidfile(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	pf := servePidfile(home)
	if err := os.MkdirAll(filepath.Dir(pf), 0o755); err != nil {
		t.Fatal(err)
	}
	// pid 1 is alive but pid 0x7FFFFFFE is not; a malformed line is
	// stale too.
	for _, body := range []string{
		fmt.Sprintf("%d 127.0.0.1:7684\t/tmp\t(embedded)\t\n", deadPid(t)),
		"garbage\n",
	} {
		if err := os.WriteFile(pf, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
		if _, ok := runningServe(home); ok {
			t.Fatalf("%q reported a running daemon", body)
		}
		if _, err := os.Stat(pf); !os.IsNotExist(err) {
			t.Errorf("%q left a stale pidfile behind", body)
		}
	}
}

// Two supervisors would each spawn a child per session, and two
// writers on one history file is exactly what ConcurrentWriter warns
// about — so a pidfile naming a LIVE foreign pid is never clobbered,
// and the serve that found it does not start.
func TestWriteServePidfileRefusesALiveForeignPid(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	pf := servePidfile(home)
	if err := os.MkdirAll(filepath.Dir(pf), 0o755); err != nil {
		t.Fatal(err)
	}
	// The parent of this test process is alive and is not us.
	foreign := os.Getppid()
	body := fmt.Sprintf("%d 127.0.0.1:7684\t/tmp\t(embedded)\t\n", foreign)
	if err := os.WriteFile(pf, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	if done, err := writeServePidfile(home, "127.0.0.1:7685"); done != nil || err == nil {
		t.Fatalf("started beside a live daemon's pidfile (err %v); it must refuse", err)
	}
	got, err := os.ReadFile(pf)
	if err != nil || string(got) != body {
		t.Fatalf("pidfile = %q, %v; want it untouched", got, err)
	}
}

// A serve's exit cleanup removes the pidfile only while it names that
// serve: a second serve that started while this one was still reaping
// its children (a launchd respawn, a hand-run `serve --run`) owns the
// file by then, and removing it hid that serve from `serve status` and
// let the next `bough serve` start a third beside it.
func TestServePidfileCleanupLeavesAnothersPidfile(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	done, err := writeServePidfile(home, "127.0.0.1:7684")
	if done == nil || err != nil {
		t.Fatal("writeServePidfile refused to write into a clean HOME")
	}
	body := fmt.Sprintf("%d 127.0.0.1:7684\t/tmp\t(embedded)\t\n", os.Getppid())
	if err := os.WriteFile(servePidfile(home), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	done()
	if got, err := os.ReadFile(servePidfile(home)); err != nil || string(got) != body {
		t.Fatalf("pidfile after our cleanup = %q, %v; want the other serve's, untouched", got, err)
	}
}

// `serve stop` and the update's restart drop the pidfile of the serve
// they signalled only once it is gone and only while it still names
// it: a stop that gave up after 5 s used to remove a live, still-reaping
// serve's file, and the next start ran a second supervisor beside it.
func TestReleaseServePidfile(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	pf := servePidfile(home)
	if err := os.MkdirAll(filepath.Dir(pf), 0o755); err != nil {
		t.Fatal(err)
	}
	live, dead := os.Getppid(), deadPid(t)
	line := func(pid int) string { return fmt.Sprintf("%d 127.0.0.1:7684\t/tmp\t(embedded)\t\n", pid) }
	for _, c := range []struct {
		name      string
		file, pid int
		removed   bool
	}{
		{"the signalled serve still reaping", live, live, false},
		{"another serve's file", live, dead, false},
		{"the signalled serve gone", dead, dead, true},
	} {
		if err := os.WriteFile(pf, []byte(line(c.file)), 0o644); err != nil {
			t.Fatal(err)
		}
		releaseServePidfile(home, c.pid)
		_, err := os.Stat(pf)
		if gone := os.IsNotExist(err); gone != c.removed {
			t.Errorf("%s: removed = %v, want %v", c.name, gone, c.removed)
		}
	}
}

// A session in a repo with its own bough.yml runs that file's llm row,
// whatever serve's cwd or HOME config says, and an edit to it shows on
// the next read despite the memo.
func TestConfiguredInReadsTheSessionsDir(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	cfg := filepath.Join(dir, "bough.yml")
	if err := os.WriteFile(cfg, []byte("- id: llm\n  plugin: llm-control\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if d := configuredIn(dir); d.Plugin != "llm-control" || d.Model != "" {
		t.Fatalf("configuredIn = %+v, want llm-control with no model", d)
	}
	if err := os.WriteFile(cfg, []byte("- id: llm\n  plugin: llm-echo\n  config:\n    model: repo-model\n    effort: high\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if d := configuredIn(dir); d.Plugin != "llm-echo" || d.Model != "repo-model" || d.Effort != "high" {
		t.Fatalf("after an edit configuredIn = %+v, want llm-echo repo-model high", d)
	}
}
