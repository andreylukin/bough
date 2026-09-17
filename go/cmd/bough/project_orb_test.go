package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
)

func orbFixture(t *testing.T) (string, *container.Fake, func(string, ...string) (string, error)) {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)
	rt := container.NewFake()
	old := projectRuntime
	projectRuntime = func() container.Runtime { return rt }
	t.Cleanup(func() { projectRuntime = old })
	os.MkdirAll(filepath.Join(home, "repos", "web"), 0o755)
	run := func(stdin string, args ...string) (string, error) {
		var out bytes.Buffer
		err := project(&out, strings.NewReader(stdin), args)
		return out.String(), err
	}
	return home, rt, run
}

func putState(t *testing.T, home string, s orb.State) {
	t.Helper()
	if s.UpdatedAt.IsZero() {
		s.UpdatedAt = time.Now()
	}
	b, _ := json.Marshal(s)
	dir := orb.Dir(home, s.Session)
	os.MkdirAll(dir, 0o755)
	os.WriteFile(filepath.Join(dir, "state.json"), b, 0o644)
}

// A6: plain errors a person can act on, not wrapped os errors.
func TestProjectHumanErrors(t *testing.T) {
	_, _, run := orbFixture(t)
	check := func(want string, args ...string) {
		t.Helper()
		_, err := run("", args...)
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%v: err = %v, want %q", args, err, want)
		}
		if err != nil && strings.Contains(err.Error(), "no such file") {
			t.Errorf("%v: leaked os error: %v", args, err)
		}
	}
	check(`no project "nope"`, "show", "nope")
	check(`no project "nope"`, "set", "nope", "base", "x")
	check(`no project "nope"`, "build", "nope")
	check("needs a repo", "create", "web")
	if _, err := run("", "create", "web", "~/repos/web"); err != nil {
		t.Fatal(err)
	}
	check(`project "web" exists`, "create", "web", "~/repos/web")
	check(`no orb for session "zzz"`, "logs", "zzz")
	check(`no orb for session "zzz"`, "stop", "zzz")
}

// A6: --help prints usage and succeeds.
func TestProjectHelp(t *testing.T) {
	_, _, run := orbFixture(t)
	for _, h := range []string{"--help", "-h", "help"} {
		out, err := run("", h)
		if err != nil || !strings.Contains(out, "usage: bough project") {
			t.Errorf("%s: err=%v out=%q", h, err, out)
		}
	}
	if !strings.Contains(usageText, "\n  project ") {
		t.Error("bough --help does not list project")
	}
}

// A6: list shows each project's image and orb state.
func TestProjectListState(t *testing.T) {
	home, rt, run := orbFixture(t)
	if _, err := run("", "create", "web", "~/repos/web"); err != nil {
		t.Fatal(err)
	}
	out, _ := run("", "list")
	if !strings.Contains(out, "not built") {
		t.Errorf("list before build:\n%s", out)
	}
	p, _ := projectdef.Load(home, "web")
	h, _ := projectdef.ImageHash(home, p)
	rt.AddImage(projectdef.ImageTag("web", h))
	// A failed build that left its tag behind is still a failed build.
	os.MkdirAll(filepath.Dir(orb.ImageLogPath(home, "web")), 0o755)
	os.WriteFile(filepath.Join(filepath.Dir(orb.ImageLogPath(home, "web")), "build.json"), []byte(`{"state":"failed","hash":"`+h+`"}`), 0o644)
	if out, _ = run("", "list"); !strings.Contains(out, "build failed") {
		t.Errorf("list after a failed build:\n%s", out)
	}
	os.Remove(filepath.Join(filepath.Dir(orb.ImageLogPath(home, "web")), "build.json"))
	putState(t, home, orb.State{Session: "s1", Project: "web", Status: orb.StatusStopped})
	putState(t, home, orb.State{Session: "s2", Project: "web", Status: orb.StatusFailed})
	out, _ = run("", "list")
	for _, want := range []string{"SLUG", "web", "built", "1 stopped", "1 failed"} {
		if !strings.Contains(out, want) {
			t.Errorf("list lacks %q:\n%s", want, out)
		}
	}
}

// B2: build, status, logs, stop run in-process on the same orb helpers serve uses.
func TestProjectOrbVerbs(t *testing.T) {
	home, rt, run := orbFixture(t)
	if _, err := run("", "create", "web", "~/repos/web"); err != nil {
		t.Fatal(err)
	}
	out, err := run("", "build", "web")
	if err != nil {
		t.Fatalf("build: %v\n%s", err, out)
	}
	if !strings.Contains(out, "Built bough-") {
		t.Errorf("build output:\n%s", out)
	}
	if b, _ := orb.ReadBuild(home, "web"); b.State != "ok" {
		t.Errorf("build.json state = %q", b.State)
	}

	tag, _ := orb.EnsureImage(t.Context(), rt, home, mustLoad(t, home, "web"), nil)
	rt.Start(t.Context(), container.RunSpec{Name: container.OrbName("s-run"), Image: tag})
	putState(t, home, orb.State{Session: "s-run", Project: "web", Status: orb.StatusRunning, Container: container.OrbName("s-run")})
	putState(t, home, orb.State{Session: "s-bad", Project: "web", Status: orb.StatusFailed, Phase: orb.PhaseSetup, Error: "resume.sh exit 1"})
	os.WriteFile(filepath.Join(orb.Dir(home, "s-bad"), "resume.log"), []byte("npm ERR boom\n"), 0o644)

	out, _ = run("", "status")
	for _, want := range []string{"SESSION", "s-run", "stopped", "s-bad", "failed"} {
		if !strings.Contains(out, want) {
			t.Errorf("status lacks %q:\n%s", want, out)
		}
	}
	out, _ = run("", "status", "s-bad")
	if !strings.Contains(out, "resume.sh exit 1") {
		t.Errorf("status <session> lacks error:\n%s", out)
	}
	out, err = run("", "logs", "s-bad")
	if err != nil || !strings.Contains(out, "npm ERR boom") {
		t.Errorf("logs: %v\n%s", err, out)
	}
	out, err = run("", "logs", "web")
	if err != nil || !strings.Contains(out, "== build.log") {
		t.Errorf("logs <slug>: %v\n%s", err, out)
	}

	out, err = run("", "stop", "s-run")
	if err != nil {
		t.Fatalf("stop: %v\n%s", err, out)
	}
	if s, _ := orb.ReadState(home, "s-run"); s.Status != orb.StatusStopped {
		t.Errorf("state after stop = %q", s.Status)
	}
	if !strings.Contains(strings.Join(rt.CallList(), " "), "stop "+container.OrbName("s-run")) {
		t.Errorf("runtime calls: %v", rt.CallList())
	}
	// A live owner means jobs may run: ask first.
	putState(t, home, orb.State{Session: "s-live", Project: "web", Status: orb.StatusRunning, PID: os.Getpid()})
	out, _ = run("n\n", "stop", "s-live")
	if !strings.Contains(out, "Proceed?") {
		t.Errorf("stop of a live session did not confirm:\n%s", out)
	}
	if s, _ := orb.ReadState(home, "s-live"); s.Status != orb.StatusRunning {
		t.Errorf("declined stop changed state to %q", s.Status)
	}
}

// B6: sessions carries a mode column.
func TestPrintSessionsModeColumn(t *testing.T) {
	var buf bytes.Buffer
	printSessions(&buf, []history.SessionInfo{{ID: "a", Title: "t", Mode: "project", Project: "web"}, {ID: "b", Title: "t"}}, false)
	out := buf.String()
	if !strings.Contains(out, "project:web") || !strings.Contains(out, "local") {
		t.Fatalf("mode column missing:\n%s", out)
	}
}

func mustLoad(t *testing.T, home, slug string) projectdef.Project {
	t.Helper()
	p, err := projectdef.Load(home, slug)
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// A failed build that left its tag behind is rebuilt, not "Up to date".
func TestProjectBuildAfterFailedTag(t *testing.T) {
	home, rt, run := orbFixture(t)
	if _, err := run("", "create", "web", "~/repos/web"); err != nil {
		t.Fatal(err)
	}
	h, _ := projectdef.ImageHash(home, mustLoad(t, home, "web"))
	rt.AddImage(projectdef.ImageTag("web", h))
	dir := filepath.Dir(orb.ImageLogPath(home, "web"))
	os.MkdirAll(dir, 0o755)
	os.WriteFile(filepath.Join(dir, "build.json"), []byte(`{"state":"failed","hash":"`+h+`"}`), 0o644)
	out, err := run("", "build", "web")
	if err != nil || !strings.Contains(out, "Built bough-") {
		t.Fatalf("build: %v\n%s", err, out)
	}
	if b, _ := orb.ReadBuild(home, "web"); b.State != "ok" {
		t.Errorf("build.json state = %q", b.State)
	}
}
