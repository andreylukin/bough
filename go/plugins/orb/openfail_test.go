package orb

import (
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
