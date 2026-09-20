package orb

import (
	"context"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

// A project whose orb cannot open (here a repo path that does not exist;
// on the work machine a setup.sh that failed the image build) used to fail
// the row, and the strict first mount killed the session process before it
// read a single message. The session must stay up and say why.
func TestProjectOrbOpenFailureKeepsSessionUp(t *testing.T) {
	home := t.TempDir()
	if _, err := projectdef.Create(home, "broken"); err != nil {
		t.Fatal(err)
	}
	// Written behind WriteFile's back: the repo went away after the save.
	if err := os.WriteFile(filepath.Join(projectdef.Root(home), "broken", projectdef.FileYAML), []byte("repos:\n  - path: "+filepath.Join(home, "no-such-repo")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	userHome = func() (string, error) { return home, nil }
	chdir = func(string) error { return nil }

	secs := &fakeSections{m: map[string]string{}}
	ctx := kernel.NewContext()
	ctx.Provide("session-mode", "project")
	ctx.Provide("session-project", "broken")
	ctx.Provide("prompt-sections", secs)
	ctx.Provide("codemode", codemode.New(5*time.Second))
	rows := []kernel.Row{
		{ID: "history", Plugin: "history", Config: map[string]any{"file": filepath.Join(home, ".bough", "history", "s1.jsonl")}},
		{ID: "scratchpad", Plugin: "scratchpad", Config: map[string]any{"dir": filepath.Join(home, "scratch")}},
		{ID: "orb", Plugin: "orb", Config: map[string]any{"runtime": "fake"}},
	}
	if err := ctx.Mount(rows); err != nil {
		t.Fatalf("a failed orb open killed the mount: %v", err)
	}
	defer ctx.Unmount()
	// The orb service is there from the mount, but it never runs anything
	// on the host: a command against the failed start fails with the reason.
	h, err := kernel.Get[*handle](ctx, "orb")
	if err != nil {
		t.Fatal(err)
	}
	if err := h.Ready(context.Background()); err == nil {
		t.Fatal("a failed open reported ready")
	}
	if c := h.Command(context.Background(), "true"); c.Err == nil || !strings.Contains(c.Err.Error(), "no-such-repo") {
		t.Errorf("Command after a failed open: err = %v", c.Err)
	}
	waitFor(t, "the failed prompt section", func() bool { return strings.Contains(secs.get("orb"), "failed to start") })
	if text := secs.get("orb"); !strings.Contains(text, "bough project write broken setup.sh") {
		t.Errorf("orb prompt section = %q", text)
	}
}

func brokenProject(t *testing.T, home, slug string) {
	t.Helper()
	if _, err := projectdef.Create(home, slug); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(projectdef.Root(home), slug, projectdef.FileYAML), []byte("repos:\n  - path: "+filepath.Join(home, "no-such-repo")+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
}

func mountBroken(t *testing.T, home, uiMode, origin string) (*kernel.Context, error) {
	t.Helper()
	ctx := kernel.NewContext()
	ctx.Provide("ui-mode", uiMode)
	ctx.Provide("origin", origin)
	ctx.Provide("session-mode", "project")
	ctx.Provide("session-project", "broken")
	ctx.Provide("prompt-sections", &fakeSections{m: map[string]string{}})
	ctx.Provide("codemode", codemode.New(5*time.Second))
	rows := []kernel.Row{
		{ID: "history", Plugin: "history", Config: map[string]any{"file": filepath.Join(home, ".bough", "history", "s1.jsonl")}},
		{ID: "scratchpad", Plugin: "scratchpad", Config: map[string]any{"dir": filepath.Join(home, "scratch")}},
		{ID: "orb", Plugin: "orb", Config: map[string]any{"runtime": "fake"}},
	}
	return ctx, ctx.Mount(rows)
}

// `bough --headless --project` run by a person or script must not run the
// turn in a session whose orb failed: it exits non-zero with the reason.
// serve's children (origin web) stay up so the web can show the failure.
func TestHeadlessOrbFailureFailsMount(t *testing.T) {
	home := t.TempDir()
	brokenProject(t, home, "broken")
	userHome = func() (string, error) { return home, nil }
	chdir = func(string) error { return nil }

	ctx, err := mountBroken(t, home, "headless", "headless")
	if err == nil {
		ctx.Unmount()
		t.Fatal("a headless orb failure mounted")
	}
	if !strings.Contains(err.Error(), "broken") || !strings.Contains(err.Error(), "bough project show broken") {
		t.Errorf("err = %v", err)
	}
	ctx, err = mountBroken(t, home, "headless", "web")
	if err != nil {
		t.Fatalf("a serve child died on an orb failure: %v", err)
	}
	ctx.Unmount()
}

// Under the tui the failure is a notice the ui renders, never a raw
// stderr line painted over the alt screen.
func TestTUIOrbFailureIsNotice(t *testing.T) {
	home := t.TempDir()
	brokenProject(t, home, "broken")
	userHome = func() (string, error) { return home, nil }
	chdir = func(string) error { return nil }

	r, w, _ := os.Pipe()
	stderr := os.Stderr
	os.Stderr = w
	ctx, err := mountBroken(t, home, "tui", "tui")
	os.Stderr = stderr
	w.Close()
	var buf strings.Builder
	io.Copy(&buf, r)
	if err != nil {
		t.Fatal(err)
	}
	defer ctx.Unmount()
	if strings.Contains(buf.String(), "bough: orb") {
		t.Errorf("stderr under the tui: %q", buf.String())
	}
	h, err := kernel.Get[*handle](ctx, "orb")
	if err != nil {
		t.Fatal(err)
	}
	h.Ready(context.Background())
	waitFor(t, "the orb notice", func() bool { n, _ := kernel.Get[string](ctx, "orb-notice"); return n != "" })
	n, err := kernel.Get[string](ctx, "orb-notice")
	if err != nil || !strings.Contains(n, "broken") || !strings.Contains(n, "bough project show broken") {
		t.Errorf("orb-notice = %q, %v", n, err)
	}
}

// resume.sh failing leaves the container running but the project broken
// (no deps, no dev server): a headless run must not answer from it either.
func TestHeadlessResumeFailureFailsMount(t *testing.T) {
	home := t.TempDir()
	repo := t.TempDir()
	for _, args := range [][]string{{"init", "-q"}, {"-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"}} {
		if out, err := exec.Command("git", append([]string{"-C", repo}, args...)...).CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	if _, err := projectdef.Create(home, "broken"); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(projectdef.Root(home), "broken", projectdef.FileYAML), []byte("repos:\n  - path: "+repo+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(projectdef.Root(home), "broken", projectdef.FileResume), []byte("#!/bin/sh\necho 'npm ERR! missing script: dev' >&2\nexit 3\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	userHome = func() (string, error) { return home, nil }
	chdir = func(string) error { return nil }

	ctx, err := mountBroken(t, home, "headless", "headless")
	if err == nil {
		ctx.Unmount()
		t.Fatal("a headless resume.sh failure mounted")
	}
	if !strings.Contains(err.Error(), "npm ERR! missing script: dev") || !strings.Contains(err.Error(), "resume.sh") {
		t.Errorf("err = %v", err)
	}
}
