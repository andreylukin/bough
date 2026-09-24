package orb

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	iorb "github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
	"github.com/andreylukin/bough/plugins/commands"
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

type fakeSections struct {
	mu sync.Mutex
	m  map[string]string
}

func (f *fakeSections) Set(name, text string) { f.mu.Lock(); defer f.mu.Unlock(); f.m[name] = text }
func (f *fakeSections) get(name string) string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.m[name]
}

// waitFor polls cond: the start settles on its own goroutine, and the
// row's watcher runs after it.
func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for !cond() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func TestLocalSessionSetsReadOnlySection(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	secs := &fakeSections{m: map[string]string{}}
	ctx.Provide("prompt-sections", secs)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	if secs.get("mode") != iorb.LocalPromptSection {
		t.Errorf("mode section = %q", secs.get("mode"))
	}
}

// The row mounts after the real scratchpad row (Inject keys, not row
// order) and returns at once, with the orb still starting behind its
// handle; a command waits for it. Once the fake runtime lets the start
// through, the row has chdir'd into the primary worktree and written
// state.json.
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
	gate := make(chan struct{})
	newFake = func() container.Runtime { f := container.NewFake(); f.StartGate = gate; return f }

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
	reg := commands.NewRegistry()
	// The /orb setup skill is already registered by the skills row.
	reg.Register(commands.CommandInfo{Name: "orb", Kind: "skill"}, func(string) (string, error) { return "", nil })
	ctx.Provide("commands", reg)
	if err := ctx.Mount(rows); err != nil {
		t.Fatal(err)
	}
	defer ctx.Unmount()
	o, err := kernel.Get[*handle](ctx, "orb-state")
	if err != nil {
		for _, r := range ctx.Rows() {
			t.Logf("%s %s %v %v", r.ID, r.State, r.Missing, r.Err)
		}
		t.Fatal(err)
	}
	// Mounted, not open: the worktree is there and the process is in it,
	// the start is held at the container start.
	if !o.Starting() || len(dirs) != 1 {
		t.Fatalf("row waited for the start: starting=%v chdir=%v", o.Starting(), dirs)
	}
	if line := o.Line(); !strings.HasPrefix(line, "orb demo · ") {
		t.Errorf("Line while starting = %q", line)
	}
	// A command bounded by its own context gives up with the reason.
	short, cancelShort := context.WithTimeout(context.Background(), 20*time.Millisecond)
	if c := o.Command(short, "true"); c.Err == nil || !strings.Contains(c.Err.Error(), "waiting for the container") {
		t.Errorf("Command while starting: err = %v", c.Err)
	}
	cancelShort()
	close(gate)
	if err := o.Ready(context.Background()); err != nil {
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

	// A reload (a late service, a Remount) keeps the same running orb:
	// no stop, no second Open.
	opens := func() int {
		b, _ := os.ReadFile(filepath.Join(iorb.Dir(home, "sess1"), "state.json"))
		return strings.Count(string(b), "running")
	}
	if err := ctx.Remount("orb"); err != nil {
		t.Fatal(err)
	}
	o2, err := kernel.Get[*handle](ctx, "orb-state")
	if err != nil || o2 != o {
		t.Fatalf("reload reopened the orb: %v", err)
	}
	if s, _ := iorb.ReadState(home, "sess1"); s.Status != iorb.StatusRunning || opens() != 1 {
		t.Fatalf("reload stopped the orb: %+v", s)
	}

	// /orb status names every phase with its time; logs shows resume.log;
	// anything else is still the setup skill; stop stops the container.
	os.WriteFile(filepath.Join(iorb.Dir(home, "sess1"), "resume.log"), []byte("== resume.sh start x\n"), 0o644)
	if out, err := reg.Run("orb", "status"); err != nil || !strings.Contains(out, "orb demo · running") || !strings.Contains(out, "build image") || !strings.Contains(out, "resume.sh") {
		t.Errorf("/orb status = %q, %v", out, err)
	}
	if out, err := reg.Run("orb", "logs"); err != nil || !strings.Contains(out, "== resume.sh start") {
		t.Errorf("/orb logs = %q, %v", out, err)
	}
	if _, err := reg.Run("orb", "demo"); err == nil || !strings.Contains(err.Error(), "/orb demo") {
		t.Errorf("/orb demo = %v, want the skill submit", err)
	}
	if out, err := reg.Run("orb", "stop"); err != nil || !strings.Contains(out, "stopped") {
		t.Errorf("/orb stop = %q, %v", out, err)
	}
	if s, _ := iorb.ReadState(home, "sess1"); s.Status != iorb.StatusStopped {
		t.Errorf("after /orb stop %s", s.Status)
	}
	// /orb restart applies the definition in this session: scheduled at
	// once, and with no turn running the orb comes back swapped.
	if out, err := reg.Run("orb", "restart"); err != nil || !strings.Contains(out, "scheduled") {
		t.Errorf("/orb restart = %q, %v", out, err)
	}
	waitFor(t, "the restarted orb", func() bool {
		s, _ := iorb.ReadState(home, "sess1")
		return s.Status == iorb.StatusRunning && s.Restart == ""
	})

	// A chdir failing leaves no container behind: the start settles
	// failed before one is started.
	ctx.Unmount()
	chdir = func(string) error { return os.ErrNotExist }
	ctx2 := kernel.NewContext()
	ctx2.Provide("session-mode", "project")
	ctx2.Provide("session-project", "demo")
	ctx2.Provide("codemode", codemode.New(5*time.Second))
	ctx2.Provide("commands", commands.NewRegistry())
	if err := ctx2.Mount(rows); err != nil {
		t.Fatal(err)
	}
	defer ctx2.Unmount()
	o3, err := kernel.Get[*handle](ctx2, "orb-state")
	if err != nil {
		t.Fatal(err)
	}
	if err := o3.Ready(context.Background()); err == nil || !strings.Contains(err.Error(), "chdir") {
		t.Fatalf("Ready after a failed chdir = %v", err)
	}
	if s, _ := iorb.ReadState(home, "sess1"); s.Status != iorb.StatusFailed || !strings.Contains(s.Error, "chdir") {
		t.Fatalf("failed chdir left the orb %s (%q)", s.Status, s.Error)
	}
}
