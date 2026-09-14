package orb

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	_ "github.com/andreylukin/bough/plugins/history"
	_ "github.com/andreylukin/bough/plugins/scratch"
)

func TestLocalSessionMountsNothing(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	if _, err := kernel.Get[any](ctx, "orb"); err == nil {
		t.Error("local session provides orb")
	}
}

type fakeSections struct{ m map[string]string }

func (f *fakeSections) Set(name, text string) { f.m[name] = text }

func TestLocalSessionSetsReadOnlySection(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	secs := &fakeSections{m: map[string]string{}}
	ctx.Provide("prompt-sections", secs)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	if secs.m["mode"] != iorb.LocalPromptSection {
		t.Errorf("mode section = %q", secs.m["mode"])
	}
}

// The row mounts after the real scratchpad row (Inject keys, not row
// order), opens the orb with the fake runtime, chdirs into the primary
// worktree and writes state.json.
func TestProjectRowOpensOrb(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	repo := filepath.Join(t.TempDir(), "app")
	git := func(args ...string) {
		t.Helper()
		c := exec.Command("git", args...)
		c.Dir = repo
		c.Env = append(os.Environ(), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t")
		if out, err := c.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v\n%s", args, err, out)
		}
	}
	if err := os.MkdirAll(repo, 0o755); err != nil {
		t.Fatal(err)
	}
	git("init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(repo, "README"), []byte("hi\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	git("add", ".")
	git("commit", "-qm", "init")
	if _, err := projectdef.Create(home, "demo"); err != nil {
		t.Fatal(err)
	}
	if err := projectdef.WriteFile(home, "demo", projectdef.FileYAML, "repos:\n  - path: "+repo+"\n    branch: main\n"); err != nil {
		t.Fatal(err)
	}

	userHome = func() (string, error) { return home, nil }
	var dirs []string
	chdir = func(d string) error { dirs = append(dirs, d); return nil }

	ctx := kernel.NewContext()
	ctx.Provide("session-mode", "project")
	ctx.Provide("session-project", "demo")
	hist := filepath.Join(home, ".bough", "history", "sess1.jsonl")
	// orb is listed FIRST: only its Inject keys can make it wait.
	rows := []kernel.Row{
		{ID: "orb", Plugin: "orb", Config: map[string]any{"runtime": "fake"}},
		{ID: "history", Plugin: "history", Config: map[string]any{"file": hist}},
		{ID: "scratchpad", Plugin: "scratchpad", Config: map[string]any{"dir": filepath.Join(home, "scratch")}},
	}
	ctx.Provide("codemode", codemode.New(5*time.Second))
	if err := ctx.Mount(rows); err != nil {
		t.Fatal(err)
	}
	defer ctx.Unmount()
	o, err := kernel.Get[interface{ State() iorb.State }](ctx, "orb-state")
	if err != nil {
		for _, r := range ctx.Rows() {
			t.Logf("%s %s %v %v", r.ID, r.State, r.Missing, r.Err)
		}
		t.Fatal(err)
	}
	st := o.State()
	if st.Primary == "" || len(dirs) != 1 || dirs[0] != st.Primary {
		t.Errorf("chdir = %v, primary %q", dirs, st.Primary)
	}
	if !strings.HasPrefix(st.Primary, iorb.Dir(home, "sess1")) {
		t.Errorf("primary %q not under the orb dir", st.Primary)
	}
	disk, err := iorb.ReadState(home, "sess1")
	if err != nil || disk.Project != "demo" || disk.Status == "" {
		t.Errorf("state.json = %+v, %v", disk, err)
	}
}
