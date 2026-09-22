package tools

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/andreylukin/bough/kernel"
	"github.com/andreylukin/bough/plugins/codemode"
)

// fakeOrb stands in for plugins/orb: Command runs on the host with a
// marker in the env so a test can tell the orb ran it, and records argv.
type fakeOrb struct {
	root string
	mu   sync.Mutex
	argv [][]string
}

func (o *fakeOrb) Command(ctx context.Context, argv ...string) *exec.Cmd {
	o.mu.Lock()
	o.argv = append(o.argv, argv)
	o.mu.Unlock()
	c := exec.CommandContext(ctx, argv[0], argv[1:]...)
	c.Env = append(os.Environ(), "IN_ORB=yes")
	return c
}

func (o *fakeOrb) Root() string { return o.root }

func (o *fakeOrb) calls() int {
	o.mu.Lock()
	defer o.mu.Unlock()
	return len(o.argv)
}

// provideHostProject makes ctx a project session whose orb is the host
// filesystem root, for tests of write/patch themselves.
func provideHostProject(ctx *kernel.Context) {
	ctx.Provide("session-mode", "project")
	ctx.Provide("session-project", "demo")
	ctx.Provide("orb", &fakeOrb{root: "/"})
}

type fakeSections struct {
	mu sync.Mutex
	m  map[string]string
}

func (f *fakeSections) Set(name, text string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.m[name] = text
}

func TestLocalSessionHasNoWriteTools(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	secs := &fakeSections{m: map[string]string{}}
	ctx.Provide("prompt-sections", secs)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	cm, _ := kernel.Get[*codemode.CodeMode](ctx, "codemode")
	out, err := cm.Run(`typeof tools.write + " " + typeof tools.patch + " " + typeof tools.view + " " + typeof tools.bash`)
	if err != nil {
		t.Fatal(err)
	}
	if out != "undefined undefined function function" {
		t.Errorf("local tools = %q", out)
	}
	// The orb row owns the mode section (plugins/orb): tools must not
	// touch prompt-sections, or its reload cascades into the loop.
	if _, ok := secs.m["mode"]; ok {
		t.Errorf("tools set the mode section: %q", secs.m["mode"])
	}
	if strings.Contains(cm.Catalogue(), "tools.write") {
		t.Error("local catalogue still describes tools.write")
	}
}

func TestProjectBashAndJobsRunThroughOrb(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	ctx.Provide("session-mode", "project")
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	// The orb lands after tools mounted: bash must resolve it per call.
	o := &fakeOrb{root: t.TempDir()}
	ctx.Provide("orb", o)

	out, err := st.bash("echo fg=$IN_ORB")
	if err != nil || !strings.Contains(out, "fg=yes") {
		t.Fatalf("foreground = %q, %v", out, err)
	}
	if _, err := st.bash("echo bg=$IN_ORB", 10); err != nil {
		t.Fatal(err)
	}
	if out, err := st.jobs.jobWait(1, 10); err != nil || !strings.Contains(out, "bg=yes") {
		t.Fatalf("job = %q, %v", out, err)
	}
	if n := o.calls(); n != 2 {
		t.Errorf("orb commands = %d, want 2", n)
	}
	if o.argv[0][0] != "sh" {
		t.Errorf("argv = %v", o.argv[0])
	}

	inside := filepath.Join(o.root, "repo", "f.txt")
	if _, err := st.write(inside, "x"); err != nil {
		t.Errorf("write inside root: %v", err)
	}
	outside := filepath.Join(t.TempDir(), "f.txt")
	if _, err := st.write(outside, "x"); err == nil || !strings.Contains(err.Error(), "outside this project session") {
		t.Errorf("write outside root err = %v", err)
	}
	if _, err := os.Stat(outside); err == nil {
		t.Error("refused write still created the file")
	}
	if _, err := st.patch(outside, "", "x"); err == nil {
		t.Error("patch outside root allowed")
	}
}

func TestProjectWithoutOrbNeverRunsOnHost(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	ctx.Provide("session-mode", "project")
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	marker := filepath.Join(t.TempDir(), "ran")
	if _, err := st.bash("touch " + marker); err == nil || !strings.Contains(err.Error(), "orb not ready") {
		t.Errorf("bash err = %v", err)
	}
	if _, err := st.bash("touch "+marker, 10); err == nil {
		t.Error("background bash without an orb started")
	}
	if _, err := os.Stat(marker); err == nil {
		t.Fatal("bash ran on the host with no orb")
	}
	if _, err := st.write(marker, "x"); err == nil || !strings.Contains(err.Error(), "orb not ready") {
		t.Errorf("write err = %v", err)
	}
}

// A hook is the user's own script: its bash runs on the host even in a
// project session, where the agent's own bash never does.
func TestProjectHookBashRunsOnHost(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	cm := codemode.New(5 * time.Second)
	ctx.Provide("codemode", cm)
	ctx.Provide("session-mode", "project")
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	fg, bg := filepath.Join(dir, "fg"), filepath.Join(dir, "bg")
	body := `tools.bash("touch ` + fg + `"); tools.bash("touch ` + bg + `", 10);`
	if _, err := cm.RunHook(context.Background(), body, map[string]any{}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(fg); err != nil {
		t.Fatalf("hook bash did not run on the host: %v", err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		if _, err := os.Stat(bg); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("hook background bash did not run on the host")
		}
		time.Sleep(20 * time.Millisecond)
	}
	// Outside the hook, the agent's bash still refuses to use the host.
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	if _, err := st.bash("true"); err == nil || !strings.Contains(err.Error(), "orb not ready") {
		t.Errorf("agent bash err = %v", err)
	}
}

// guestOrb's commands carry a Cancel standing in for the runtime's guest
// kill, which tools must still call after its own process-group kill.
type guestOrb struct {
	fakeOrb
	kills atomic.Int32
}

func (o *guestOrb) Command(ctx context.Context, argv ...string) *exec.Cmd {
	c := o.fakeOrb.Command(ctx, argv...)
	c.Cancel = func() error { o.kills.Add(1); return nil }
	return c
}

func TestProjectCancelKillsInsideOrb(t *testing.T) {
	t.Parallel()
	ctx := kernel.NewContext()
	ctx.Provide("codemode", codemode.New(5*time.Second))
	ctx.Provide("session-mode", "project")
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	defer ctx.Unmount()
	st, _ := kernel.Get[*Stats](ctx, "turn-stats")
	o := &guestOrb{fakeOrb: fakeOrb{root: t.TempDir()}}
	ctx.Provide("orb", orbExec(o))

	turn, cancel := context.WithCancel(context.Background())
	st.runCtx = func() context.Context { return turn }
	done := make(chan error, 1)
	go func() { _, err := st.bash("sleep 30"); done <- err }()
	time.Sleep(200 * time.Millisecond)
	cancel()
	if err := <-done; err == nil || !strings.Contains(err.Error(), "cancelled") {
		t.Fatalf("bash err = %v", err)
	}
	if n := o.kills.Load(); n != 1 {
		t.Fatalf("guest kills after a cancelled bash = %d, want 1", n)
	}

	st.jobs.grace = 0 // the job itself is under test, not the foreground wait
	if _, err := st.bash("sleep 30", 60); err != nil {
		t.Fatal(err)
	}
	time.Sleep(200 * time.Millisecond)
	st.jobs.Stop()
	deadline := time.Now().Add(5 * time.Second)
	for o.kills.Load() != 2 && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if n := o.kills.Load(); n != 2 {
		t.Fatalf("guest kills after stopping a job = %d, want 2", n)
	}
}
