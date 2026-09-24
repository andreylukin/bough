package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
)

// startedWith is the BOUGH_PROJECT_DIR of each start the child has
// recorded, in order: it writes one meta entry per process start. The
// entry lands after the start is counted, so callers wait on this.
func startedWith(t *testing.T, f *fixture, id string) []string {
	t.Helper()
	es, err := history.Read(filepath.Join(f.hist, id+".jsonl"))
	if err != nil {
		return nil // the child has not written its first entry yet
	}
	var dirs []string
	for _, e := range es {
		if e.Kind == "meta" {
			if _, ok := e.Data["project_dir"]; ok {
				v, _ := e.Data["project_dir"].(string)
				dirs = append(dirs, v)
			}
		}
	}
	return dirs
}

// The membership lives in meta.json, not in the spawn arguments: a
// session assigned to a project long after it was created — or after a
// serve restart, which empties spawnArgs — still has to start with the
// project directory, or its MEMORY.md is never injected.
func TestProjectEnvAtEveryStart(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	if _, err := projectdef.CreateEmpty(f.home, "web", "Web"); err != nil {
		t.Fatal(err)
	}
	f.seed(t, "sess-env", history.Entry{
		Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": f.home},
	})

	if err := f.sup.Adopt("sess-env"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the child's first start", func() bool { return len(startedWith(t, f, "sess-env")) == 1 })
	if got := startedWith(t, f, "sess-env"); got[0] != "" {
		t.Fatalf("unassigned session started with BOUGH_PROJECT_DIR=%q", got[0])
	}
	if err := f.sup.AssignProject("sess-env", "web"); err != nil {
		t.Fatalf("AssignProject: %v", err)
	}
	// Assignment does not restart anything: the running child keeps the
	// environment it booted with, and the next start is what picks it up.
	if err := f.sup.Kill("sess-env"); err != nil {
		t.Fatalf("Kill: %v", err)
	}
	if err := f.sup.Adopt("sess-env"); err != nil {
		t.Fatalf("re-Adopt: %v", err)
	}
	waitFor(t, "the replacement child's start", func() bool { return len(startedWith(t, f, "sess-env")) == 2 })
	want := filepath.Join(projectdef.Root(f.home), "web")
	if got := startedWith(t, f, "sess-env"); got[1] != want {
		t.Errorf("BOUGH_PROJECT_DIR = %q, want %q", got[1], want)
	}
}

// A project whose directory is gone, and a slug that was never a
// directory name, are both "no project": pointing a child at a path
// that is not there would make every turn's context read fail.
func TestProjectEnvSkipsMissingDirectory(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	if _, err := projectdef.CreateEmpty(f.home, "web", "Web"); err != nil {
		t.Fatal(err)
	}
	f.seed(t, "s1")
	if err := f.sup.AssignProject("s1", "web"); err != nil {
		t.Fatalf("AssignProject: %v", err)
	}
	if len(f.sup.projectEnv("s1")) != 1 {
		t.Fatalf("projectEnv = %v, want the directory", f.sup.projectEnv("s1"))
	}
	if err := os.RemoveAll(filepath.Join(projectdef.Root(f.home), "web")); err != nil {
		t.Fatal(err)
	}
	if got := f.sup.projectEnv("s1"); got != nil {
		t.Errorf("projectEnv after the directory went = %v, want none", got)
	}
	if got := f.sup.projectEnv(""); got != nil {
		t.Errorf("projectEnv(no id) = %v, want none", got)
	}
}

// The first start of main is told it is main too, not only a restart:
// projectEnv has no id to go on in a Create, so the main thread's first
// process ran as an ordinary project session (the model test's boot
// hold saw it announce itself as "session").
func TestMainsFirstStartKnowsItIsMain(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "App")
	code, body := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"hello"}`)
	if code != http.StatusOK {
		t.Fatalf("message = %d %v", code, body)
	}
	main, _ := body["main"].(string)
	es, err := history.Read(filepath.Join(f.hist, main+".jsonl"))
	if err != nil || len(es) == 0 {
		t.Fatalf("main's history: %v %v", es, err)
	}
	if got := es[0].Data["project_main"]; got != "1" {
		t.Errorf("main's first process started with BOUGH_PROJECT_MAIN=%q, want 1", got)
	}
}

// The main thread is told it is the main thread, and a thread of the
// same project is not. Derived at every start for the same reason as
// the directory: which session is main lives in meta.json, not in the
// spawn arguments, so a restarted serve still says so.
func TestProjectEnvMarksTheMainThread(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	if _, err := projectdef.CreateEmpty(f.home, "web", "Web"); err != nil {
		t.Fatal(err)
	}
	f.seed(t, "s-main")
	f.seed(t, "s-thread")
	for _, id := range []string{"s-main", "s-thread"} {
		if err := f.sup.AssignProject(id, "web"); err != nil {
			t.Fatalf("AssignProject %s: %v", id, err)
		}
	}
	f.sup.mu.Lock()
	f.sup.mains["web"] = "s-main"
	f.sup.mu.Unlock()

	if got := f.sup.projectEnv("s-main"); !slices.Contains(got, "BOUGH_PROJECT_MAIN=1") {
		t.Errorf("the main thread starts with %v, want BOUGH_PROJECT_MAIN", got)
	}
	if got := f.sup.projectEnv("s-thread"); slices.Contains(got, "BOUGH_PROJECT_MAIN=1") {
		t.Errorf("a thread starts with %v, which claims it is main", got)
	}
}

// A running child keeps the project directory it started with, so the
// row says which one that was: a person who files a live session sees
// its MEMORY.md is not in force until the child starts again, instead of
// expecting it on the next turn. Nothing while no child runs.
func TestRowSaysWhichProjectTheChildStartedWith(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	if _, err := projectdef.CreateEmpty(f.home, "web", "Web"); err != nil {
		t.Fatal(err)
	}
	f.seed(t, "sess-brief", history.Entry{
		Seq: 1, At: time.Now(), Kind: "meta", Data: map[string]any{"cwd": f.home},
	})
	a := NewAPI(f.sup)
	row := func() Row {
		t.Helper()
		in, ok := a.info("sess-brief")
		if !ok {
			t.Fatal("no session sess-brief")
		}
		return a.row(in)
	}
	if err := f.sup.AssignProject("sess-brief", "web"); err != nil {
		t.Fatalf("AssignProject: %v", err)
	}
	if r := row(); r.Live || r.StartedIn != "" {
		t.Fatalf("no child: live %v, startedIn %q; want false, \"\"", r.Live, r.StartedIn)
	}
	if err := f.sup.Adopt("sess-brief"); err != nil {
		t.Fatalf("Adopt: %v", err)
	}
	waitFor(t, "the child's start", func() bool { return len(startedWith(t, f, "sess-brief")) == 1 })
	if r := row(); !r.Live || r.StartedIn != "web" {
		t.Fatalf("started filed in web: live %v, startedIn %q; want true, \"web\"", r.Live, r.StartedIn)
	}
	// Taken out while it runs: the child still has web's directory.
	if err := f.sup.AssignProject("sess-brief", ""); err != nil {
		t.Fatalf("AssignProject out: %v", err)
	}
	if r := row(); r.Project != "" || r.StartedIn != "web" {
		t.Fatalf("filed out while running: project %q, startedIn %q; want \"\", \"web\"", r.Project, r.StartedIn)
	}
	if err := f.sup.Kill("sess-brief"); err != nil {
		t.Fatalf("Kill: %v", err)
	}
	waitFor(t, "the child to be gone", func() bool { return !f.sup.Live("sess-brief") })
	if r := row(); r.StartedIn != "" {
		t.Fatalf("after the child went: startedIn %q, want \"\"", r.StartedIn)
	}
}
