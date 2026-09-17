package orb

import (
	"io"
	"os"
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
	if _, err := kernel.Get[any](ctx, "orb"); err == nil {
		t.Error("a failed open still provided an orb")
	}
	text := secs.m["orb"]
	if !strings.Contains(text, "failed to start") || !strings.Contains(text, "bough project write broken setup.sh") {
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
	n, err := kernel.Get[string](ctx, "orb-notice")
	if err != nil || !strings.Contains(n, "broken") || !strings.Contains(n, "bough project show broken") {
		t.Errorf("orb-notice = %q, %v", n, err)
	}
}
