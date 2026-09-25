package serve

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
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

func projectBySlug(t *testing.T, f *apiFixture, slug string) map[string]any {
	t.Helper()
	_, body := f.do(t, "GET", "/api/projects", "")
	ps, _ := body["projects"].([]any)
	for _, p := range ps {
		if m, _ := p.(map[string]any); m["slug"] == slug {
			return m
		}
	}
	t.Fatalf("project %s not listed: %v", slug, body)
	return nil
}

// A version-1 meta.json migrates on boot: the label table becomes
// directories, the display names survive into project.yml, and every
// session's membership is rewritten to a slug.
func TestMigrateLabelTable(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	meta := filepath.Join(home, ".bough", "serve", "meta.json")
	os.MkdirAll(filepath.Dir(meta), 0o755)
	// p1 label only; p2 already attached to a definition whose directory
	// carries the slugified name; p3 a name nothing can be slugified from.
	if _, err := projectdef.Create(home, "my-web-app"); err != nil {
		t.Fatal(err)
	}
	os.WriteFile(meta, []byte(`{"sessions":{`+
		`"s1":{"project":"p1"},"s2":{"project":"p2"},"s3":{"project":"p3"},"s4":{"project":"gone"},"s5":{"title":"keep me"}},`+
		`"projects":{`+
		`"p1":{"id":"p1","name":"Platform infra"},`+
		`"p2":{"id":"p2","name":"My Web App","slug":"my-web-app"},`+
		`"p3":{"id":"p3","name":"🚀"}}}`), 0o644)

	sup, err := NewSupervisor(Options{Exe: "/bin/true", HistDir: filepath.Join(home, ".bough", "history"), MetaPath: meta, Runtime: container.NewFake()})
	if err != nil {
		t.Fatal(err)
	}
	defer sup.Close()

	want := map[string]string{"platform-infra": "Platform infra", "my-web-app": "My Web App", "project-1": "🚀"}
	got := map[string]string{}
	for _, p := range sup.Projects() {
		got[p.Slug] = p.Name
	}
	for slug, name := range want {
		if got[slug] != name {
			t.Errorf("project %s = %q, want %q (all: %v)", slug, got[slug], name, got)
		}
	}
	if len(got) != len(want) {
		t.Errorf("projects = %v, want %v", got, want)
	}
	for sid, slug := range map[string]string{"s1": "platform-infra", "s2": "my-web-app", "s3": "project-1", "s4": "", "s5": ""} {
		if m := sup.Meta(sid); m.Project != slug {
			t.Errorf("%s project = %q, want %q", sid, m.Project, slug)
		}
	}
	if m := sup.Meta("s5"); m.Title != "keep me" {
		t.Errorf("session metadata lost in the migration: %+v", m)
	}
	// The table is gone and the file says so, so the next boot skips it.
	b, _ := os.ReadFile(meta)
	if strings.Contains(string(b), `"projects"`) || !strings.Contains(string(b), `"version": 2`) {
		t.Fatalf("meta.json after migration: %s", b)
	}

	// Booting again changes nothing: the version stamp short-circuits it.
	before := string(b)
	sup2, err := NewSupervisor(Options{Exe: "/bin/true", HistDir: filepath.Join(home, ".bough", "history"), MetaPath: meta, Runtime: container.NewFake()})
	if err != nil {
		t.Fatal(err)
	}
	defer sup2.Close()
	if b, _ := os.ReadFile(meta); string(b) != before {
		t.Errorf("second boot rewrote meta.json:\n%s\n%s", before, b)
	}
	if len(sup2.Projects()) != len(want) {
		t.Errorf("second boot projects = %+v", sup2.Projects())
	}
}

// A definition the agent wrote with the file tools is a project: nothing
// has to be told about it.
func TestDefinitionOnDiskIsAProject(t *testing.T) {
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
	if len(ps) != 1 || ps[0] != (Project{Slug: "made-by-agent", Name: "made-by-agent"}) {
		t.Fatalf("projects = %+v", ps)
	}
	// Broken yaml is still a project: the editor that fixes it is on its page.
	os.WriteFile(filepath.Join(projectdef.Root(home), "made-by-agent", projectdef.FileYAML), []byte("repos: [\n"), 0o644)
	ps = sup.Projects()
	if len(ps) != 1 || ps[0].Error == "" {
		t.Fatalf("broken definition = %+v", ps)
	}
}

// The orb page re-reads its detail only when the listed summary moves.
// An identity entry leaves the image hash alone, so a project.yml written
// outside the editor (a guest through the mounted project dir) must move
// the summary by itself, or the editor keeps showing the old file.
func TestOrbSummaryMovesOnOutsideEdit(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Lend")
	before, _ := projectBySlug(t, f, slug)["orb"].(map[string]any)
	path := filepath.Join(projectdef.Root(f.home), slug, projectdef.FileYAML)
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	yml := strings.Replace(string(b), "identity: [gh]", "identity: [gh, .aws]", 1)
	if yml == string(b) {
		t.Fatalf("no identity line to extend in %q", b)
	}
	if err := os.WriteFile(path, []byte(yml), 0o644); err != nil {
		t.Fatal(err)
	}
	after, _ := projectBySlug(t, f, slug)["orb"].(map[string]any)
	if fmt.Sprint(after) == fmt.Sprint(before) {
		t.Fatalf("summary unchanged by a project.yml edit: %v", after)
	}
}

// Every project has an orb surface: the directory IS the definition, so
// there is nothing to attach.
func TestOrbDetail(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "My App")
	if slug != "my-app" {
		t.Fatalf("slug = %q", slug)
	}
	if o, _ := projectBySlug(t, f, slug)["orb"].(map[string]any); o["slug"] != "my-app" || !strings.HasPrefix(o["image"].(string), "bough-orb/my-app:") {
		t.Fatalf("listed orb = %v", o)
	}
	if code, body := f.do(t, "GET", "/api/projects/"+slug+"/orb", ""); code != http.StatusOK {
		t.Fatalf("detail = %d %v", code, body)
	} else if files, _ := body["files"].(map[string]any); len(files) != len(projectdef.EditableFiles) {
		t.Errorf("detail files = %v", body["files"])
	} else if rt, _ := body["runtime"].(map[string]any); rt["name"] != "fake" || rt["available"] != true {
		t.Errorf("detail runtime = %v", body["runtime"])
	} else if p, _ := body["project"].(map[string]any); p["slug"] != "my-app" || p["name"] != "My App" {
		t.Errorf("detail project = %v", body["project"])
	}
	if code, _ := f.do(t, "GET", "/api/projects/not-a-project/orb", ""); code != http.StatusNotFound {
		t.Errorf("detail of nothing = %d, want 404", code)
	}
}

func TestPutOrbFile(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "web")
	base := "/api/projects/" + slug + "/orb/files/"
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
	slug := mkProject(t, f, "built")

	code, body := f.do(t, "POST", "/api/projects/"+slug+"/orb/build", `{}`)
	if code != http.StatusAccepted {
		t.Fatalf("build = %d %v", code, body)
	}
	var text string
	offset := 0.0
	waitFor(t, "build ok", func() bool {
		_, lb := f.do(t, "GET", "/api/projects/"+slug+"/orb/build/log?offset="+jsonNum(offset), "")
		text += lb["text"].(string)
		offset = lb["offset"].(float64)
		return lb["state"] == "ok"
	})
	_, lb := f.do(t, "GET", "/api/projects/"+slug+"/orb/build/log?offset="+jsonNum(offset), "")
	text += lb["text"].(string)
	if !strings.Contains(text, "fake commit bough-orb/built:") {
		t.Errorf("log = %q", text)
	}
	if o, _ := projectBySlug(t, f, slug)["orb"].(map[string]any); o["built"] != true {
		t.Errorf("not built after ok: %v", o)
	}

	// A build this serve is running refuses a second one.
	f.sup.mu.Lock()
	f.sup.building["built"] = true
	f.sup.mu.Unlock()
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/orb/build", `{}`); code != http.StatusConflict {
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
// A stop serve makes outside the Stop orb handler — Kill, archive, the
// reaper's own — must not leave the running snapshot saying the
// container runs: the session's next start would show up (and offer
// Stop) for a stopped container for up to runningTTL. Found by the orb
// lifecycle model walk (go/tests/model): Kill, then a restart.
func TestKillDropsRunningSnapshot(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	ctx := context.Background()
	id := "killed"
	seedModeSession(t, f, id, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName(id), Image: "img"}); err != nil {
		t.Fatal(err)
	}
	writeState(t, f.home, orb.State{Session: id, Project: "app", Status: orb.StatusStarting, PID: os.Getpid(), UpdatedAt: time.Now()})
	_, body := f.do(t, "GET", "/api/sessions/"+id, "")
	if o, _ := rowOf(t, body)["orb"].(map[string]any); o["up"] != true {
		t.Fatalf("before the stop: row orb = %v, want up", o)
	}
	// What Kill's stopKilledOrb does for a killed child.
	if err := f.sup.stopOrb(ctx, id); err != nil {
		t.Fatal(err)
	}
	// The session starts again; the container is still stopped.
	time.Sleep(2 * time.Millisecond)
	writeState(t, f.home, orb.State{Session: id, Project: "app", Status: orb.StatusStarting, PID: os.Getpid(), UpdatedAt: time.Now()})
	_, body = f.do(t, "GET", "/api/sessions/"+id, "")
	if o, _ := rowOf(t, body)["orb"].(map[string]any); o["up"] != nil {
		t.Errorf("after serve stopped it: row orb = %v, want not up", o)
	}
}

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
	f := newAPI(t, "BOUGH_MODE=project", "BOUGH_PROJECT=leak")
	slug := mkProject(t, f, "App")
	if code, _ := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"no-such-project"}`); code != http.StatusBadRequest {
		t.Errorf("project mode on a project that does not exist = %d, want 400", code)
	}
	code, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+slug+`","cwd":"/nowhere"}`)
	if code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, body)
	}
	row := rowOf(t, body)
	id, _ := row["id"].(string)
	if row["project"] != slug || row["mode"] != "project" {
		t.Errorf("row = project %v mode %v", row["project"], row["mode"])
	}
	// The session is a thread of the project's main thread, so its
	// finish reaches a conversation a person reads.
	main := f.sup.MainID(slug)
	if main == "" || id == main || row["spawnedBy"] != main {
		t.Errorf("row %q spawnedBy = %v, want main %q", id, row["spawnedBy"], main)
	}
	waitFor(t, "the thread's history", func() bool {
		es, _ := history.Read(filepath.Join(f.hist, id+".jsonl"))
		return len(es) > 0
	})
	es, _ := history.Read(filepath.Join(f.hist, id+".jsonl"))
	if len(es) == 0 || es[0].Data["mode"] != "project" || es[0].Data["project"] != "app" || !sameResolvedDir(es[0].Data["cwd"], f.home) {
		t.Fatalf("child meta = %+v", es)
	}
	// Main runs in the same project's orb, and is nobody's child: a
	// SpawnedBy on it would make the depth rule refuse its spawns.
	mes, _ := history.Read(filepath.Join(f.hist, main+".jsonl"))
	if len(mes) == 0 || mes[0].Data["mode"] != "project" || mes[0].Data["project"] != "app" || mes[0].Data["spawned_by"] != "" {
		t.Fatalf("main meta = %+v", mes)
	}
	if code, _ := f.do(t, "POST", "/api/sessions", `{"mode":"cloud","cwd":"/"}`); code != http.StatusBadRequest {
		t.Errorf("unknown mode = %d, want 400", code)
	}
}

func TestLocalChildDropsInheritedMode(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envNewID+"=sess-local", "BOUGH_MODE=project", "BOUGH_PROJECT=leak")
	code, body := f.do(t, "POST", "/api/sessions", `{"cwd":"`+f.home+`"}`)
	if code != http.StatusCreated {
		t.Fatalf("create = %d %v", code, body)
	}
	id, _ := rowOf(t, body)["id"].(string)
	es, _ := history.Read(filepath.Join(f.hist, id+".jsonl"))
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
	slug := mkProject(t, f, "remote")
	base := "/api/projects/" + slug + "/orb/files/"
	missing := filepath.Join(t.TempDir(), "nope.git")
	if code, body := f.do(t, "PUT", base+"project.yml", `{"text":"repos:\n  - remote: `+missing+`\n    name: app\n"}`); code != http.StatusOK {
		t.Fatalf("save = %d %v", code, body)
	}
	if code, body := f.do(t, "POST", "/api/projects/"+slug+"/orb/build", `{}`); code != http.StatusAccepted {
		t.Fatalf("build = %d %v", code, body)
	}
	var lb map[string]any
	waitFor(t, "build failed", func() bool {
		_, lb = f.do(t, "GET", "/api/projects/"+slug+"/orb/build/log?offset=0", "")
		return lb["state"] == "failed"
	})
	if msg, _ := lb["error"].(string); msg == "" {
		t.Errorf("failed build has no error: %v", lb)
	}
	_, d := f.do(t, "GET", "/api/projects/"+slug+"/orb", "")
	if b, _ := d["build"].(map[string]any); b["state"] != "failed" || b["error"] == "" {
		t.Errorf("detail build = %v", d["build"])
	}
	if o, _ := projectBySlug(t, f, slug)["orb"].(map[string]any); o["build"] != "failed" {
		t.Errorf("summary = %v", o)
	}
}

// While this serve builds, the detail's build says building (the page
// decides from it whether to poll the log), and a failed build of an
// older tag never contradicts the current image being built.
func TestOrbDetailBuildState(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "bs")
	f.do(t, "POST", "/api/projects/"+slug+"/orb/build", `{}`)
	waitFor(t, "build ok", func() bool {
		_, lb := f.do(t, "GET", "/api/projects/"+slug+"/orb/build/log?offset=0", "")
		return lb["state"] == "ok"
	})

	f.sup.mu.Lock()
	f.sup.building["bs"] = true
	f.sup.mu.Unlock()
	_, d := f.do(t, "GET", "/api/projects/"+slug+"/orb", "")
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
	_, d = f.do(t, "GET", "/api/projects/"+slug+"/orb", "")
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
	slug := mkProject(t, f, "pf")
	_, d := f.do(t, "GET", "/api/projects/"+slug+"/orb", "")
	pf, _ := d["preflight"].([]any)
	if len(pf) == 0 {
		t.Fatalf("no preflight in %v", d)
	}
	if c, _ := pf[0].(map[string]any); c["kind"] != "runtime" || c["status"] != "ok" {
		t.Errorf("first check = %v", c)
	}
}

// A7: the orb list names each session by its title even once archived
// (the web only holds live rows), and the summary names the repos the
// definition declares, so the page never reads "0 repos" for them.
func TestOrbDetailTitlesAndRepos(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "tr")
	seedModeSession(t, f, "gone", map[string]any{"cwd": "/w", "mode": "project", "project": "tr"})
	if err := f.sup.SetTitle("gone", "Debug the server"); err != nil {
		t.Fatal(err)
	}
	if code, body := f.do(t, "POST", "/api/sessions/gone/archive", ""); code != http.StatusOK {
		t.Fatalf("archive = %d %v", code, body)
	}
	writeState(t, f.home, orb.State{Session: "gone", Project: "tr", Status: orb.StatusStopped, UpdatedAt: time.Now()})
	// A new project declares no repos; this one works on app.
	if code, body := f.do(t, "PUT", "/api/projects/"+slug+"/orb/files/project.yml",
		`{"text":"repos:\n  - remote: `+filepath.Join(t.TempDir(), "app.git")+`\n    name: app\n"}`); code != http.StatusOK {
		t.Fatalf("save = %d %v", code, body)
	}
	_, d := f.do(t, "GET", "/api/projects/"+slug+"/orb", "")
	os_, _ := d["orbs"].([]any)
	if len(os_) != 1 {
		t.Fatalf("orbs = %v", d["orbs"])
	}
	if o, _ := os_[0].(map[string]any); o["title"] != "Debug the server" {
		t.Errorf("orb row title = %v", o["title"])
	}
	sum, _ := d["orb"].(map[string]any)
	if rs, _ := sum["repos"].([]any); len(rs) == 0 {
		t.Errorf("summary repos = %v", sum)
	}
}

// A project with no repos — what a label-only project migrates into —
// has a page: the detail is a 200 with a preflight the orb strip can
// render, not a 500 and a blank panel.
func TestOrbDetailNoRepos(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "empty area")
	code, d := f.do(t, "GET", "/api/projects/"+slug+"/orb", "")
	if code != http.StatusOK {
		t.Fatalf("detail = %d %v", code, d)
	}
	pf, _ := d["preflight"].([]any)
	if len(pf) == 0 {
		t.Fatalf("no preflight in %v", d)
	}
	if c, _ := pf[0].(map[string]any); c["kind"] != "runtime" {
		t.Errorf("first check = %v", c)
	}
	sum, _ := d["orb"].(map[string]any)
	if rs, _ := sum["repos"].([]any); len(rs) != 0 {
		t.Errorf("summary repos = %v, want none", rs)
	}
	if sum["error"] != nil {
		t.Errorf("summary error = %v", sum["error"])
	}
	if files, _ := d["files"].(map[string]any); files[projectdef.FileMemory] == nil {
		t.Errorf("files = %v, want one entry per editable file", files)
	}
}

// A work area created from a repo group is a definition directory from
// the first click: no repos in it, but a slug, so the project page has
// an orb to configure instead of an "attach one" prompt.
func TestProjectFromRepoMakesAnEmptyDefinition(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	now := time.Now()
	f.seed(t, "01a00000-0000-7000-8000-0000000000f1",
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": f.home}},
		codeEntry(2, `tools.bash("cd repos/lone-repo && ls")`),
	)
	code, body := f.do(t, "POST", "/api/projects/from-repo", `{"repos":["lone-repo"],"name":"Ship it"}`)
	if code != http.StatusOK {
		t.Fatalf("from-repo = %d %v", code, body)
	}
	p, _ := body["project"].(map[string]any)
	if p["slug"] != "ship-it" {
		t.Fatalf("project = %v, want the slug of a new definition", p)
	}
	def, err := projectdef.Load(f.sup.Home(), "ship-it")
	if err != nil {
		t.Fatal(err)
	}
	if len(def.Def.Repos) != 0 {
		t.Errorf("repos = %v, want none: the repos it works on are chosen by hand", def.Def.Repos)
	}
	if def.Def.Name != "Ship it" {
		t.Errorf("name = %q, want the name the caller gave", def.Def.Name)
	}
}

// Sixty failed orbs used to mean sixty `container inspect` execs per
// list. The runtime is asked once for what runs, and the answer serves
// every row for a while; a stop this serve performs is seen at once.
func TestContainerUpIsOneListPerSnapshot(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	ctx := context.Background()
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	for _, id := range []string{"f1", "f2", "f3"} {
		seedModeSession(t, f, id, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
		writeState(t, f.home, orb.State{Session: id, Project: "app", Status: orb.StatusFailed, UpdatedAt: time.Now()})
	}
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName("f2"), Image: "img"}); err != nil {
		t.Fatal(err)
	}
	before := len(f.rt.CallList())
	_, body := f.do(t, "GET", "/api/sessions", "")
	rows, _ := body["sessions"].([]any)
	up := map[string]bool{}
	for _, r := range rows {
		row := r.(map[string]any)
		o, _ := row["orb"].(map[string]any)
		up[row["id"].(string)] = o["up"] == true
	}
	if !up["f2"] || up["f1"] || up["f3"] {
		t.Fatalf("up = %v", up)
	}
	calls := f.rt.CallList()[before:]
	running, inspects := 0, 0
	for _, c := range calls {
		if c == "running" {
			running++
		}
		if strings.HasPrefix(c, "inspect") {
			inspects++
		}
	}
	if running != 1 || inspects != 0 {
		t.Fatalf("runtime calls for one list = %v (want one running, no inspect)", calls)
	}
	// A second list inside the TTL asks nothing.
	before = len(f.rt.CallList())
	f.do(t, "GET", "/api/sessions", "")
	if n := len(f.rt.CallList()) - before; n != 0 {
		t.Fatalf("second list made %d runtime calls", n)
	}
	// Stopped through this serve: the next list sees it down without waiting out the TTL.
	if code, _ := f.do(t, "POST", "/api/sessions/f2/orb/stop", ""); code != http.StatusOK {
		t.Fatalf("stop = %d", code)
	}
	_, body = f.do(t, "GET", "/api/sessions/f2", "")
	if o, _ := rowOf(t, body)["orb"].(map[string]any); o["up"] == true {
		t.Fatalf("f2 still up after stop: %v", o)
	}
}

// A session quiet for longer than the idle limit loses its container;
// one that wrote history recently, or whose orb changed recently, keeps
// it. The stop is the same as a Stop orb click: state.json says stopped.
func TestReaperStopsQuietOrbsOnly(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	ctx := context.Background()
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	now := time.Now()
	old := now.Add(-6 * time.Hour)
	for _, id := range []string{"quiet", "busy", "fresh-orb", "stopped"} {
		seedModeSession(t, f, id, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
		if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName(id), Image: "img"}); err != nil {
			t.Fatal(err)
		}
		// The history file's mtime is the session's last word.
		if err := os.Chtimes(filepath.Join(f.home, ".bough", "history", id+".jsonl"), old, old); err != nil {
			t.Fatal(err)
		}
	}
	writeState(t, f.home, orb.State{Session: "quiet", Project: "app", Status: orb.StatusRunning, UpdatedAt: old})
	writeState(t, f.home, orb.State{Session: "busy", Project: "app", Status: orb.StatusRunning, UpdatedAt: old})
	writeState(t, f.home, orb.State{Session: "fresh-orb", Project: "app", Status: orb.StatusRunning, UpdatedAt: now.Add(-time.Minute)})
	writeState(t, f.home, orb.State{Session: "stopped", Project: "app", Status: orb.StatusStopped, UpdatedAt: old})
	// busy wrote history a minute ago.
	if err := os.Chtimes(filepath.Join(f.home, ".bough", "history", "busy.jsonl"), now, now.Add(-time.Minute)); err != nil {
		t.Fatal(err)
	}

	stopped := f.api.reapIdleOrbs(ctx, 4*time.Hour, now)
	if len(stopped) != 1 || stopped[0] != "quiet" {
		t.Fatalf("stopped = %v, want [quiet]", stopped)
	}
	for id, want := range map[string]container.State{"quiet": container.StateStopped, "busy": container.StateRunning, "fresh-orb": container.StateRunning} {
		if st, _ := f.rt.Inspect(ctx, container.OrbName(id)); st != want {
			t.Errorf("%s container = %s, want %s", id, st, want)
		}
	}
	if st, _ := orb.ReadState(f.home, "quiet"); st.Status != orb.StatusStopped {
		t.Errorf("quiet state = %s, want stopped", st.Status)
	}
	// A second pass finds nothing left to do.
	if again := f.api.reapIdleOrbs(ctx, 4*time.Hour, now); len(again) != 0 {
		t.Fatalf("second pass stopped %v", again)
	}

	// A start that failed after the VM came up leaves it running under a
	// "failed" state: idle is idle, so it is stopped like any other.
	seedModeSession(t, f, "half-up", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName("half-up"), Image: "img"}); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(filepath.Join(f.home, ".bough", "history", "half-up.jsonl"), old, old); err != nil {
		t.Fatal(err)
	}
	writeState(t, f.home, orb.State{Session: "half-up", Project: "app", Status: orb.StatusFailed, Error: "resume.sh: exit status 1", UpdatedAt: old})
	// And a ghost: state says running, the container is long gone. It is
	// stopped in the record, once, instead of failing every pass forever.
	seedModeSession(t, f, "ghost", map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	if err := os.Chtimes(filepath.Join(f.home, ".bough", "history", "ghost.jsonl"), old, old); err != nil {
		t.Fatal(err)
	}
	writeState(t, f.home, orb.State{Session: "ghost", Project: "app", Status: orb.StatusRunning, UpdatedAt: old})
	got := f.api.reapIdleOrbs(ctx, 4*time.Hour, now)
	slices.Sort(got)
	if !slices.Equal(got, []string{"ghost", "half-up"}) {
		t.Fatalf("stopped = %v, want [ghost half-up]", got)
	}
	if st, _ := f.rt.Inspect(ctx, container.OrbName("half-up")); st != container.StateStopped {
		t.Errorf("half-up container = %s, want stopped", st)
	}
	for _, id := range []string{"half-up", "ghost"} {
		if st, _ := orb.ReadState(f.home, id); st.Status != orb.StatusStopped {
			t.Errorf("%s state = %s, want stopped", id, st.Status)
		}
	}
	if again := f.api.reapIdleOrbs(ctx, 4*time.Hour, now); len(again) != 0 {
		t.Fatalf("third pass stopped %v", again)
	}
	// Off is off.
	for in, want := range map[string]time.Duration{"": DefaultOrbIdle, "0": 0, "off": 0, "90m": 90 * time.Minute} {
		if got, err := OrbIdleFromEnv(in); err != nil || got != want {
			t.Errorf("OrbIdleFromEnv(%q) = %v %v", in, got, err)
		}
	}
	if _, err := OrbIdleFromEnv("soon"); err == nil {
		t.Error("a bad duration should be an error")
	}
}

// Restart orb asks a live session through its request file (202); a
// session nobody runs needs nothing, since its next start applies the
// definition (200, not scheduled); no orb is a 404.
func TestRestartOrb(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	live := "restarty"
	seedModeSession(t, f, live, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	writeState(t, f.home, orb.State{Session: live, Project: "app", Status: orb.StatusRunning, PID: os.Getpid(), UpdatedAt: time.Now()})
	code, body := f.do(t, "POST", "/api/sessions/"+live+"/orb/restart", `{"fresh":true}`)
	if code != http.StatusAccepted || body["scheduled"] != true {
		t.Fatalf("restart live = %d %v", code, body)
	}
	if r, ok := orb.TakeRestart(f.home, live); !ok || r.By != "web" || !r.Fresh {
		t.Fatalf("request %+v %v", r, ok)
	}
	idle := "restart-idle"
	seedModeSession(t, f, idle, map[string]any{"cwd": "/w", "mode": "project", "project": "app"})
	writeState(t, f.home, orb.State{Session: idle, Project: "app", Status: orb.StatusStopped, UpdatedAt: time.Now()})
	code, body = f.do(t, "POST", "/api/sessions/"+idle+"/orb/restart", "")
	if code != http.StatusOK || body["scheduled"] != false {
		t.Fatalf("restart idle = %d %v", code, body)
	}
	if _, ok := orb.TakeRestart(f.home, idle); ok {
		t.Fatal("request written for a session nobody runs")
	}
	if code, _ := f.do(t, "POST", "/api/sessions/nobody/orb/restart", ""); code != http.StatusNotFound {
		t.Fatalf("restart without an orb = %d", code)
	}
}
