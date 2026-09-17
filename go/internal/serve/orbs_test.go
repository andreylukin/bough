package serve

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
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

func writeState(t *testing.T, home string, st orb.State) {
	t.Helper()
	dir := orb.Dir(home, st.Session)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	b, _ := json.Marshal(st)
	if err := os.WriteFile(filepath.Join(dir, "state.json"), b, 0o644); err != nil {
		t.Fatal(err)
	}
}

func projectByID(t *testing.T, f *apiFixture, id string) map[string]any {
	t.Helper()
	_, body := f.do(t, "GET", "/api/projects", "")
	ps, _ := body["projects"].([]any)
	for _, p := range ps {
		if m, _ := p.(map[string]any); m["id"] == id {
			return m
		}
	}
	t.Fatalf("project %s not listed: %v", id, body)
	return nil
}

// An old meta.json with label-only projects must load, list and stay
// byte-for-byte the same when no definition exists.
func TestLabelOnlyProjectsUnchanged(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	meta := filepath.Join(home, ".bough", "serve", "meta.json")
	os.MkdirAll(filepath.Dir(meta), 0o755)
	golden := `{"sessions":{"s1":{"project":"p1"}},"projects":{"p1":{"id":"p1","name":"Infra"}}}`
	os.WriteFile(meta, []byte(golden), 0o644)
	sup, err := NewSupervisor(Options{Exe: "/bin/true", HistDir: filepath.Join(home, ".bough", "history"), MetaPath: meta, Runtime: container.NewFake()})
	if err != nil {
		t.Fatal(err)
	}
	defer sup.Close()
	if b, _ := os.ReadFile(meta); string(b) != golden {
		t.Fatalf("meta.json rewritten: %s", b)
	}
	ps := sup.Projects()
	if len(ps) != 1 || ps[0] != (Project{ID: "p1", Name: "Infra"}) {
		t.Fatalf("projects = %+v", ps)
	}
	b, _ := json.Marshal(ps[0])
	if string(b) != `{"id":"p1","name":"Infra"}` {
		t.Fatalf("label-only project json = %s", b)
	}
}

func TestDefinitionOnDiskGetsALabel(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := projectdef.Create(home, "made-by-agent"); err != nil {
		t.Fatal(err)
	}
	sup, err := NewSupervisor(Options{Exe: "/bin/true", HistDir: filepath.Join(home, ".bough", "history"), MetaPath: filepath.Join(home, ".bough", "serve", "meta.json"), Runtime: container.NewFake()})
	if err != nil {
		t.Fatal(err)
	}
	defer sup.Close()
	ps := sup.Projects()
	if len(ps) != 1 || ps[0].Slug != "made-by-agent" || ps[0].Name != "made-by-agent" {
		t.Fatalf("projects = %+v", ps)
	}
}

func TestAttachDetachOrb(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "My App")
	other := mkProject(t, f, "Other")

	if p := projectByID(t, f, id); p["orb"] != nil || p["slug"] != nil {
		t.Fatalf("label-only project carries orb fields: %v", p)
	}
	code, body := f.do(t, "POST", "/api/projects/"+id+"/orb", `{}`)
	if code != http.StatusOK {
		t.Fatalf("attach = %d %v", code, body)
	}
	if p, _ := body["project"].(map[string]any); p["slug"] != "my-app" {
		t.Fatalf("attach project = %v", body)
	}
	if _, err := os.Stat(filepath.Join(projectdef.Root(f.home), "my-app", projectdef.FileYAML)); err != nil {
		t.Fatalf("skeleton not created: %v", err)
	}
	if o, _ := projectByID(t, f, id)["orb"].(map[string]any); o["slug"] != "my-app" || !strings.HasPrefix(o["image"].(string), "bough-orb/my-app:") {
		t.Fatalf("listed orb = %v", o)
	}
	if code, _ := f.do(t, "POST", "/api/projects/"+other+"/orb", `{"slug":"my-app"}`); code != http.StatusConflict {
		t.Errorf("second label on one slug = %d, want 409", code)
	}
	if code, _ := f.do(t, "POST", "/api/projects/"+other+"/orb", `{"slug":"Bad Slug"}`); code != http.StatusBadRequest {
		t.Errorf("bad slug = %d, want 400", code)
	}
	if code, body := f.do(t, "GET", "/api/projects/"+id+"/orb", ""); code != http.StatusOK {
		t.Fatalf("detail = %d %v", code, body)
	} else if files, _ := body["files"].(map[string]any); len(files) != 4 || files["project.yml"] == "" {
		t.Errorf("detail files = %v", body["files"])
	} else if rt, _ := body["runtime"].(map[string]any); rt["name"] != "fake" || rt["available"] != true {
		t.Errorf("detail runtime = %v", body["runtime"])
	}

	if code, _ := f.do(t, "DELETE", "/api/projects/"+id+"/orb", ""); code != http.StatusOK {
		t.Fatalf("detach = %d", code)
	}
	if p := projectByID(t, f, id); p["orb"] != nil || p["slug"] != nil {
		t.Errorf("still attached: %v", p)
	}
	if _, err := os.Stat(filepath.Join(projectdef.Root(f.home), "my-app")); err != nil {
		t.Errorf("detach deleted the definition: %v", err)
	}
	if code, _ := f.do(t, "GET", "/api/projects/"+id+"/orb", ""); code != http.StatusNotFound {
		t.Errorf("detail after detach = %d, want 404", code)
	}
	// Re-attaching finds the existing directory instead of failing on it.
	if code, body := f.do(t, "POST", "/api/projects/"+id+"/orb", ``); code != http.StatusOK {
		t.Errorf("re-attach = %d %v", code, body)
	}
}

func TestPutOrbFile(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "web")
	f.do(t, "POST", "/api/projects/"+id+"/orb", `{}`)
	base := "/api/projects/" + id + "/orb/files/"
	if code, _ := f.do(t, "PUT", base+"project.yml", `{"text":"repos: [\n"}`); code != http.StatusBadRequest {
		t.Errorf("bad yaml = %d, want 400", code)
	}
	// A5: the skeleton's placeholder repo is refused in words the editor shows as they are.
	if code, body := f.do(t, "PUT", base+"project.yml", `{"text":"repos:\n  - path: ~/repos/example\nmemory: lots\n"}`); code != http.StatusBadRequest ||
		!strings.HasPrefix(fmt.Sprint(body["error"]), "project.yml has 2 problems:\n  repos[0].path: ~/repos/example is the template placeholder") {
		t.Errorf("host check = %d %v", code, body)
	}
	if code, _ := f.do(t, "PUT", base+"evil.sh", `{"text":"x"}`); code != http.StatusBadRequest {
		t.Errorf("bad name = %d, want 400", code)
	}
	code, body := f.do(t, "PUT", base+"resume.sh", `{"text":"echo hi\n"}`)
	if code != http.StatusOK || body["orb"] == nil {
		t.Fatalf("save = %d %v", code, body)
	}
	if got, _ := projectdef.ReadFile(f.home, "web", "resume.sh"); got != "echo hi\n" {
		t.Errorf("resume.sh = %q", got)
	}
}

func TestBuildOrbPollsToOK(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "built")
	f.do(t, "POST", "/api/projects/"+id+"/orb", `{}`)

	code, body := f.do(t, "POST", "/api/projects/"+id+"/orb/build", `{}`)
	if code != http.StatusAccepted {
		t.Fatalf("build = %d %v", code, body)
	}
	var text string
	offset := 0.0
	waitFor(t, "build ok", func() bool {
		_, lb := f.do(t, "GET", "/api/projects/"+id+"/orb/build/log?offset="+jsonNum(offset), "")
		text += lb["text"].(string)
		offset = lb["offset"].(float64)
		return lb["state"] == "ok"
	})
	_, lb := f.do(t, "GET", "/api/projects/"+id+"/orb/build/log?offset="+jsonNum(offset), "")
	text += lb["text"].(string)
	if !strings.Contains(text, "fake commit bough-orb/built:") {
		t.Errorf("log = %q", text)
	}
	if o, _ := projectByID(t, f, id)["orb"].(map[string]any); o["built"] != true {
		t.Errorf("not built after ok: %v", o)
	}

	// A build this serve is running refuses a second one.
	f.sup.mu.Lock()
	f.sup.building["built"] = true
	f.sup.mu.Unlock()
	if code, _ := f.do(t, "POST", "/api/projects/"+id+"/orb/build", `{}`); code != http.StatusConflict {
		t.Errorf("build while building = %d, want 409", code)
	}
}

func jsonNum(f float64) string { b, _ := json.Marshal(int64(f)); return string(b) }

func seedModeSession(t *testing.T, f *apiFixture, id string, data map[string]any) {
	t.Helper()
	now := time.Now()
	f.seed(t, id,
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: data},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "hi"}},
	)
}

func TestSessionRowsCarryModeAndOrb(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedModeSession(t, f, "old", map[string]any{"cwd": "/w"})
	seedModeSession(t, f, "live", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	seedModeSession(t, f, "dead", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	seedModeSession(t, f, "reused", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	now := time.Now()
	writeState(t, f.home, orb.State{Session: "live", Project: "app", Status: orb.StatusRunning, PID: os.Getpid(), UpdatedAt: now})
	// A pid far past any real one: kill(pid, 0) fails.
	writeState(t, f.home, orb.State{Session: "dead", Project: "app", Status: orb.StatusRunning, PID: 1 << 30, UpdatedAt: now})
	// Alive pid, but written before this serve started and not its child.
	writeState(t, f.home, orb.State{Session: "reused", Project: "app", Status: orb.StatusRunning, PID: os.Getpid(), UpdatedAt: now.Add(-time.Hour)})

	want := map[string]string{"live": "running", "dead": "stopped", "reused": "stopped"}
	for id, status := range want {
		_, body := f.do(t, "GET", "/api/sessions/"+id, "")
		row := rowOf(t, body)
		o, _ := row["orb"].(map[string]any)
		if row["mode"] != "project" || o["project"] != "app" || o["status"] != status {
			t.Errorf("%s row = mode %v orb %v, want project app %s", id, row["mode"], o, status)
		}
	}
	_, body := f.do(t, "GET", "/api/sessions/old", "")
	if row := rowOf(t, body); row["mode"] != "local" || row["orb"] != nil {
		t.Errorf("old session = mode %v orb %v, want local and no orb", row["mode"], row["orb"])
	}
	_, body = f.do(t, "GET", "/api/sessions/old/orb", "")
	if v, ok := body["orb"]; !ok || v != nil {
		t.Errorf("local session orb = %v, want null", body)
	}
}

func TestStopOrb(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := "stoppy"
	seedModeSession(t, f, id, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	ctx := context.Background()
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName(id), Image: "img"}); err != nil {
		t.Fatal(err)
	}
	writeState(t, f.home, orb.State{Session: id, Project: "app", Status: orb.StatusRunning, PID: os.Getpid(), UpdatedAt: time.Now()})
	_, body := f.do(t, "GET", "/api/sessions/"+id+"/orb", "")
	if o, _ := body["orb"].(map[string]any); o["status"] != "running" {
		t.Fatalf("before stop = %v", body)
	}
	if code, body := f.do(t, "POST", "/api/sessions/"+id+"/orb/stop", ""); code != http.StatusOK {
		t.Fatalf("stop = %d %v", code, body)
	}
	if st, _ := f.rt.Inspect(ctx, container.OrbName(id)); st != container.StateStopped {
		t.Errorf("container = %s", st)
	}
	_, body = f.do(t, "GET", "/api/sessions/"+id, "")
	if o, _ := rowOf(t, body)["orb"].(map[string]any); o["status"] != "stopped" {
		t.Errorf("row after stop = %v", o)
	}
	// state.json says so too: anything reading the file sees the stop.
	if st, _ := orb.ReadState(f.home, id); st.Status != orb.StatusStopped {
		t.Errorf("state.json after stop = %+v", st)
	}
}

// A failed setup leaves the container running: the row and the orb
// detail say it is up, so the web can still offer Stop.
func TestFailedSetupOrbIsUp(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	ctx := context.Background()
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	for _, id := range []string{"upfail", "downfail"} {
		seedModeSession(t, f, id, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
		writeState(t, f.home, orb.State{Session: id, Project: "app", Status: orb.StatusFailed, Error: "resume.sh: exit 3", PID: os.Getpid(), UpdatedAt: time.Now()})
	}
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName("upfail"), Image: "img"}); err != nil {
		t.Fatal(err)
	}
	for id, want := range map[string]any{"upfail": true, "downfail": nil} {
		_, body := f.do(t, "GET", "/api/sessions/"+id, "")
		if o, _ := rowOf(t, body)["orb"].(map[string]any); o["up"] != want {
			t.Errorf("%s row orb = %v, want up %v", id, o, want)
		}
		_, body = f.do(t, "GET", "/api/sessions/"+id+"/orb", "")
		if o, _ := body["orb"].(map[string]any); o["up"] != want {
			t.Errorf("%s session orb = %v, want up %v", id, o, want)
		}
	}
}

func TestCreateProjectSession(t *testing.T) {
	t.Parallel()
	// The serve process itself carries a mode: it must not leak.
	f := newAPI(t, envNewID+"=sess-proj", "BOUGH_MODE=project", "BOUGH_PROJECT=leak")
	label := mkProject(t, f, "App")
	if code, _ := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+label+`"}`); code != http.StatusBadRequest {
		t.Errorf("project mode on a label-only project = %d, want 400", code)
	}
	f.do(t, "POST", "/api/projects/"+label+"/orb", `{}`)
	code, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+label+`","cwd":"/nowhere"}`)
	if code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, body)
	}
	row := rowOf(t, body)
	if row["project"] != label || row["mode"] != "project" {
		t.Errorf("row = project %v mode %v", row["project"], row["mode"])
	}
	es, _ := history.Read(filepath.Join(f.hist, "sess-proj.jsonl"))
	if len(es) == 0 || es[0].Data["mode"] != "project" || es[0].Data["project"] != "app" || !sameResolvedDir(es[0].Data["cwd"], f.home) {
		t.Fatalf("child meta = %+v", es)
	}
	if code, _ := f.do(t, "POST", "/api/sessions", `{"mode":"cloud","cwd":"/"}`); code != http.StatusBadRequest {
		t.Errorf("unknown mode = %d, want 400", code)
	}
}

func TestLocalChildDropsInheritedMode(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envNewID+"=sess-local", "BOUGH_MODE=project", "BOUGH_PROJECT=leak")
	if code, body := f.do(t, "POST", "/api/sessions", `{"cwd":"`+f.home+`"}`); code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, body)
	}
	es, _ := history.Read(filepath.Join(f.hist, "sess-local.jsonl"))
	if len(es) == 0 || es[0].Data["mode"] != "" || es[0].Data["project"] != "" {
		t.Fatalf("local child inherited mode: %+v", es)
	}
}

// sameResolvedDir compares through symlinks: macOS TempDir lives under /var,
// which the child's getwd reports as /private/var.
func sameResolvedDir(got any, want string) bool {
	g, _ := got.(string)
	a, _ := filepath.EvalSymlinks(g)
	b, _ := filepath.EvalSymlinks(want)
	return a != "" && a == b
}

// Build clones a remote repo before hashing, as orb.Open does, and a
// failure before build.json is written still reaches the page.
func TestBuildOrbClonesRemoteAndReportsEarlyFailure(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "remote")
	f.do(t, "POST", "/api/projects/"+id+"/orb", `{}`)
	base := "/api/projects/" + id + "/orb/files/"
	missing := filepath.Join(t.TempDir(), "nope.git")
	if code, body := f.do(t, "PUT", base+"project.yml", `{"text":"repos:\n  - remote: `+missing+`\n    name: app\n"}`); code != http.StatusOK {
		t.Fatalf("save = %d %v", code, body)
	}
	if code, body := f.do(t, "POST", "/api/projects/"+id+"/orb/build", `{}`); code != http.StatusAccepted {
		t.Fatalf("build = %d %v", code, body)
	}
	var lb map[string]any
	waitFor(t, "build failed", func() bool {
		_, lb = f.do(t, "GET", "/api/projects/"+id+"/orb/build/log?offset=0", "")
		return lb["state"] == "failed"
	})
	if msg, _ := lb["error"].(string); msg == "" {
		t.Errorf("failed build has no error: %v", lb)
	}
	_, d := f.do(t, "GET", "/api/projects/"+id+"/orb", "")
	if b, _ := d["build"].(map[string]any); b["state"] != "failed" || b["error"] == "" {
		t.Errorf("detail build = %v", d["build"])
	}
	if o, _ := projectByID(t, f, id)["orb"].(map[string]any); o["build"] != "failed" {
		t.Errorf("summary = %v", o)
	}
}

// While this serve builds, the detail's build says building (the page
// decides from it whether to poll the log), and a failed build of an
// older tag never contradicts the current image being built.
func TestOrbDetailBuildState(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "bs")
	f.do(t, "POST", "/api/projects/"+id+"/orb", `{}`)
	f.do(t, "POST", "/api/projects/"+id+"/orb/build", `{}`)
	waitFor(t, "build ok", func() bool {
		_, lb := f.do(t, "GET", "/api/projects/"+id+"/orb/build/log?offset=0", "")
		return lb["state"] == "ok"
	})

	f.sup.mu.Lock()
	f.sup.building["bs"] = true
	f.sup.mu.Unlock()
	_, d := f.do(t, "GET", "/api/projects/"+id+"/orb", "")
	if b, _ := d["build"].(map[string]any); b["state"] != "building" {
		t.Errorf("detail build while building = %v", b)
	}
	f.sup.mu.Lock()
	delete(f.sup.building, "bs")
	f.sup.mu.Unlock()

	// An older tag's failure: the current image exists, so it is built.
	home := f.sup.Home()
	b, _ := json.Marshal(orb.Build{Tag: "bough-orb/bs:old", State: "failed", Error: "boom"})
	os.WriteFile(filepath.Join(home, ".bough", "orbs", "images", "bs", "build.json"), b, 0o644)
	_, d = f.do(t, "GET", "/api/projects/"+id+"/orb", "")
	sum, _ := d["orb"].(map[string]any)
	if sum["built"] != true || sum["build"] == "failed" {
		t.Errorf("summary contradicts itself: %v", sum)
	}
}

// A session's orb carries its start phases.
func TestSessionOrbPhases(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedModeSession(t, f, "ph1", map[string]any{"mode": "project", "project": "x"})
	now := time.Now().UTC()
	writeState(t, f.sup.Home(), orb.State{Session: "ph1", Project: "x", Status: orb.StatusBuilding, Phase: orb.PhaseBuild, PID: os.Getpid(), UpdatedAt: now,
		Phases: []orb.Phase{{Name: orb.PhaseSync, StartedAt: now, EndedAt: now}, {Name: orb.PhaseBuild, StartedAt: now}}})
	_, body := f.do(t, "GET", "/api/sessions/ph1/orb", "")
	o, _ := body["orb"].(map[string]any)
	if ph, _ := o["phases"].([]any); len(ph) != 2 || o["phase"] != "build" {
		t.Fatalf("orb = %v", o)
	}
}

// B1: Remove orb shows its plan first, then deletes container and orb
// dir; an orb another live process owns is refused.
func TestRemoveOrb(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	ctx := context.Background()
	id := "failedstart"
	seedModeSession(t, f, id, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName(id), Image: "img"}); err != nil {
		t.Fatal(err)
	}
	writeState(t, f.home, orb.State{Session: id, Project: "app", Status: orb.StatusFailed, Error: "resume.sh: exit 1", UpdatedAt: time.Now()})
	code, body := f.do(t, "GET", "/api/sessions/"+id+"/orb/remove?branches=1", "")
	plan, _ := body["plan"].(map[string]any)
	if code != http.StatusOK || plan["container"] != container.OrbName(id) {
		t.Fatalf("plan = %d %v", code, body)
	}
	if code, body := f.do(t, "DELETE", "/api/sessions/"+id+"/orb", ""); code != http.StatusOK {
		t.Fatalf("remove = %d %v", code, body)
	}
	if st, _ := f.rt.Inspect(ctx, container.OrbName(id)); st != container.StateMissing {
		t.Errorf("container = %s", st)
	}
	if _, err := os.Stat(orb.Dir(f.home, id)); !os.IsNotExist(err) {
		t.Errorf("orb dir kept: %v", err)
	}

	busy := "owned"
	seedModeSession(t, f, busy, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	writeState(t, f.home, orb.State{Session: busy, Project: "app", Status: orb.StatusRunning, PID: os.Getpid(), UpdatedAt: time.Now()})
	if code, _ := f.do(t, "DELETE", "/api/sessions/"+busy+"/orb", ""); code != http.StatusConflict {
		t.Errorf("remove of a live owner's orb = %d, want 409", code)
	}
	if _, err := os.Stat(orb.Dir(f.home, busy)); err != nil {
		t.Errorf("live orb removed: %v", err)
	}
}

// B4: the orb detail carries the preflight, runtime first.
func TestOrbDetailPreflight(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	id := mkProject(t, f, "pf")
	f.do(t, "POST", "/api/projects/"+id+"/orb", `{}`)
	_, d := f.do(t, "GET", "/api/projects/"+id+"/orb", "")
	pf, _ := d["preflight"].([]any)
	if len(pf) == 0 {
		t.Fatalf("no preflight in %v", d)
	}
	if c, _ := pf[0].(map[string]any); c["kind"] != "runtime" || c["status"] != "ok" {
		t.Errorf("first check = %v", c)
	}
}
