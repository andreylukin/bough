package serve

import (
	"context"
	"encoding/json"
	"fmt"
	"io/fs"
	"maps"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
	"github.com/andreylukin/bough/internal/projectdef"
	"github.com/andreylukin/bough/plugins/history"
)

func seedSession(t *testing.T, f *apiFixture, id string) {
	t.Helper()
	now := time.Now()
	f.seed(t, id,
		history.Entry{Seq: 1, At: now, Kind: "meta", Data: map[string]any{"cwd": "/w"}},
		history.Entry{Seq: 2, At: now, Kind: "input", Data: map[string]any{"text": "hi"}},
		history.Entry{Seq: 3, At: now, Kind: "done", Data: nil},
	)
}

// mkProject creates a project and returns its slug, which is its key
// everywhere: the routes, the sessions that belong to it, and the
// directory under ~/.bough/projects.
func mkProject(t *testing.T, f *apiFixture, name string) string {
	t.Helper()
	code, body := f.do(t, "POST", "/api/projects", `{"name":"`+name+`"}`)
	if code != http.StatusOK {
		t.Fatalf("create %q = %d %v", name, code, body)
	}
	p, _ := body["project"].(map[string]any)
	slug, _ := p["slug"].(string)
	if slug == "" {
		t.Fatalf("created project has no slug: %v", body)
	}
	if _, err := projectdef.Load(f.sup.Home(), slug); err != nil {
		t.Fatalf("create %q left no definition: %v", name, err)
	}
	return slug
}

func TestProjectsCreateListRename(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Platform infra")
	if slug != "platform-infra" {
		t.Fatalf("slug = %q, want it named after the project", slug)
	}

	_, body := f.do(t, "GET", "/api/projects", "")
	ps, _ := body["projects"].([]any)
	if len(ps) != 1 {
		t.Fatalf("projects = %v, want one", body)
	}
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/rename", `{"name":"Observability"}`); code != http.StatusOK {
		t.Fatalf("rename = %d", code)
	}
	_, body = f.do(t, "GET", "/api/projects", "")
	ps, _ = body["projects"].([]any)
	first, _ := ps[0].(map[string]any)
	// The name moves, the slug does not: orbs, images and every past
	// session name the directory.
	if first["name"] != "Observability" || first["slug"] != slug {
		t.Errorf("after rename = %v", first)
	}
	if def, err := projectdef.Load(f.sup.Home(), slug); err != nil || def.Def.Name != "Observability" {
		t.Errorf("project.yml after rename = %+v (%v)", def.Def, err)
	}
	// A second project of the same name has nowhere to live.
	if code, _ := f.do(t, "POST", "/api/projects", `{"name":"platform infra"}`); code != http.StatusConflict {
		t.Errorf("duplicate name = %d, want 409", code)
	}
}

// A name with no letter or digit cannot name a directory, and the
// directory is the project.
func TestProjectNameMustMakeASlug(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	if code, _ := f.do(t, "POST", "/api/projects", `{"name":"🚀🚀"}`); code != http.StatusBadRequest {
		t.Error("a name with nothing to slugify was accepted")
	}
}

// A link from before projects were keyed by slug says so, rather than
// reading as "the project is gone": a UUIDv7 passes the slug pattern.
func TestOldLabelIDLinksAreLegible(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	old := "0199b2c6-2b64-7c71-9b2a-6a1a2f3c4d5e"
	for _, c := range []struct{ method, path, body string }{
		{"POST", "/api/projects/" + old + "/rename", `{"name":"x"}`},
		{"DELETE", "/api/projects/" + old, ""},
		{"GET", "/api/projects/" + old + "/orb", ""},
	} {
		code, body := f.do(t, c.method, c.path, c.body)
		if code != http.StatusBadRequest || !strings.Contains(fmt.Sprint(body["error"]), "older control room") {
			t.Errorf("%s %s = %d %v", c.method, c.path, code, body)
		}
	}
	seedSession(t, f, "s1")
	code, body := f.do(t, "POST", "/api/sessions/s1/project", `{"project":"`+old+`"}`)
	if code != http.StatusBadRequest || !strings.Contains(fmt.Sprint(body["error"]), "older control room") {
		t.Errorf("assign to an old id = %d %v", code, body)
	}
	// A slug-shaped name that is simply not there is still a 404.
	if code, _ := f.do(t, "DELETE", "/api/projects/nope", ""); code != http.StatusNotFound {
		t.Errorf("unknown slug = %d, want 404", code)
	}
}

// A project session's project is in its history and nothing re-files it.
func TestAProjectSessionCannotBeMoved(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	mkProject(t, f, "here")
	other := mkProject(t, f, "there")
	seedModeSession(t, f, "ps", map[string]any{"cwd": "/w", "mode": "project", "project": "here"})
	code, body := f.do(t, "POST", "/api/sessions/ps/project", `{"project":"`+other+`"}`)
	if code != http.StatusConflict || !strings.Contains(fmt.Sprint(body["error"]), "start a thread in the other project") {
		t.Fatalf("move = %d %v", code, body)
	}
	_, body = f.do(t, "GET", "/api/sessions/ps", "")
	if sess, _ := body["session"].(map[string]any); sess["project"] != "here" {
		t.Errorf("row project = %v, want the slug its history records", sess["project"])
	}
}

func TestProjectNeedsAName(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	if code, _ := f.do(t, "POST", "/api/projects", `{"name":"   "}`); code == http.StatusOK {
		t.Error("a blank project name was accepted")
	}
}

func TestAssignAndUnassignASession(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedSession(t, f, "s1")
	slug := mkProject(t, f, "Infra")

	if code, body := f.do(t, "POST", "/api/sessions/s1/project", `{"project":"`+slug+`"}`); code != http.StatusOK {
		t.Fatalf("assign = %d %v", code, body)
	}
	_, body := f.do(t, "GET", "/api/sessions/s1", "")
	sess, _ := body["session"].(map[string]any)
	if sess["project"] != slug {
		t.Fatalf("session not in the project: %v", sess)
	}
	// "" takes it back out.
	if code, _ := f.do(t, "POST", "/api/sessions/s1/project", `{"project":""}`); code != http.StatusOK {
		t.Fatal("unassign failed")
	}
	_, body = f.do(t, "GET", "/api/sessions/s1", "")
	sess, _ = body["session"].(map[string]any)
	if _, ok := sess["project"]; ok {
		t.Errorf("still grouped after unassign: %v", sess)
	}
}

func TestAssignToAnUnknownProjectIsRefused(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedSession(t, f, "s1")
	if code, _ := f.do(t, "POST", "/api/sessions/s1/project", `{"project":"nope"}`); code == http.StatusOK {
		t.Error("assigned a session to a project that does not exist")
	}
}

// Deleting a project must not take the conversations with it — the
// directory is disposable, the work is not. What it DOES take is the
// slug-keyed state a project re-created under the same name would
// otherwise inherit.
func TestDeletingAProjectKeepsItsSessions(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	seedSession(t, f, "s1")
	slug := mkProject(t, f, "Temp")
	f.do(t, "POST", "/api/sessions/s1/project", `{"project":"`+slug+`"}`)
	home := f.sup.Home()
	keyed := []string{
		filepath.Join(home, ".bough", "orbs", "images", slug),
		filepath.Join(home, ".bough", "orbs", "cache", slug),
		filepath.Join(home, ".bough", "cache", slug),
	}
	for _, d := range keyed {
		if err := os.MkdirAll(d, 0o755); err != nil {
			t.Fatal(err)
		}
	}

	if code, _ := f.do(t, "DELETE", "/api/projects/"+slug, ""); code != http.StatusOK {
		t.Fatal("delete failed")
	}
	if _, err := os.Stat(filepath.Join(projectdef.Root(home), slug)); !os.IsNotExist(err) {
		t.Errorf("definition kept: %v", err)
	}
	for _, d := range keyed {
		if _, err := os.Stat(d); !os.IsNotExist(err) {
			t.Errorf("%s kept: a project of the same name would inherit it", d)
		}
	}
	code, body := f.do(t, "GET", "/api/sessions/s1", "")
	if code != http.StatusOK {
		t.Fatalf("the session died with its project: %d", code)
	}
	sess, _ := body["session"].(map[string]any)
	if _, ok := sess["project"]; ok {
		t.Errorf("session still points at a deleted project: %v", sess)
	}
	if code, _ := f.do(t, "DELETE", "/api/projects/"+slug, ""); code == http.StatusOK {
		t.Error("deleting a gone project reported success")
	}
}

// Two tabs opening the same project at the same moment must not each
// mint a main thread, and the per-slug lock must not deadlock against
// the supervisor's own mutex (Create blocks for up to createTimeout).
func TestMainIsIdempotentUnderConcurrentOpens(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Concurrent")

	ids := make([]string, 8)
	errs := make([]error, 8)
	var wg sync.WaitGroup
	for i := range ids {
		wg.Add(1)
		go func() {
			defer wg.Done()
			ids[i], errs[i] = f.sup.Main(slug)
		}()
	}
	wg.Wait()
	for i := range ids {
		if errs[i] != nil {
			t.Fatalf("Main %d: %v", i, errs[i])
		}
		if ids[i] == "" || ids[i] != ids[0] {
			t.Fatalf("Main %d = %q, want the one main %q", i, ids[i], ids[0])
		}
	}
	if n := f.startCount(t); n != 1 {
		t.Errorf("started %d children for one main thread", n)
	}
	// Persisted, so the next serve finds it rather than starting a second.
	if got := f.sup.MainID(slug); got != ids[0] {
		t.Errorf("MainID = %q, want %q", got, ids[0])
	}
	b, err := os.ReadFile(f.sup.opt.MetaPath)
	if err != nil {
		t.Fatal(err)
	}
	var on struct {
		Mains map[string]string `json:"mains"`
	}
	if json.Unmarshal(b, &on) != nil || on.Mains[slug] != ids[0] {
		t.Errorf("meta.json mains = %v, want %s -> %s", on.Mains, slug, ids[0])
	}
	// Main is nobody's child: a SpawnedBy would make ErrDepth refuse
	// every thread it tries to start.
	if sb := f.sup.Meta(ids[0]).SpawnedBy; sb != "" {
		t.Errorf("main spawnedBy = %q, want it parentless", sb)
	}
}

// Messaging the project is messaging main, and the first message is
// what creates it.
func TestMessageProjectStartsAndFeedsMain(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Talkative")
	if got := f.sup.MainID(slug); got != "" {
		t.Fatalf("a project that was never messaged already has a main: %q", got)
	}
	if code, body := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"hello"}`); code != http.StatusOK {
		t.Fatalf("message = %d %v", code, body)
	}
	main := f.sup.MainID(slug)
	if main == "" {
		t.Fatal("messaging the project started no main thread")
	}
	waitFor(t, "main to answer", func() bool { return hasKind(f.sup.Recent(main), "done") })
	var echoed bool
	for _, e := range f.sup.Recent(main) {
		if e.Kind == "assistant" && e.Text == "echo hello" {
			echoed = true
		}
	}
	if !echoed {
		t.Errorf("the message never reached main: %v", kinds(f.sup.Recent(main)))
	}
	// A second message goes to the same session.
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"again"}`); code != http.StatusOK {
		t.Fatal("second message failed")
	}
	if got := f.sup.MainID(slug); got != main {
		t.Errorf("second message started a second main: %q then %q", main, got)
	}
	if code, _ := f.do(t, "POST", "/api/projects/nope/message", `{"text":"x"}`); code != http.StatusNotFound {
		t.Error("messaged a project that does not exist")
	}
}

// An armed ask eats the next line, so the page has to be told to answer
// main's question rather than have its message swallowed.
func TestMessageProjectRefusesOnAPendingAsk(t *testing.T) {
	t.Parallel()
	f := newAPI(t, envAsk+"=1")
	slug := mkProject(t, f, "Curious")
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"hello"}`); code != http.StatusOK {
		t.Fatal("first message failed")
	}
	main := f.sup.MainID(slug)
	waitFor(t, "the ask", func() bool { return f.sup.PendingAsk(main) != nil })
	code, body := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"second"}`)
	if code != http.StatusConflict || !strings.Contains(fmt.Sprint(body["error"]), "pick one") {
		t.Fatalf("message during an ask = %d %v, want the question back", code, body)
	}
}

// The page must show a project session nothing parented: one started
// with `bough --project <slug>`, or one from before there was a main.
func TestProjectDetailIncludesAParentlessSession(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Old work")
	seedModeSession(t, f, "pre-migration", map[string]any{"cwd": "/w", "mode": "project", "project": slug})
	seedSession(t, f, "filed")
	if code, _ := f.do(t, "POST", "/api/sessions/filed/project", `{"project":"`+slug+`"}`); code != http.StatusOK {
		t.Fatal("assign failed")
	}
	// And one thread of the project's own main.
	if code, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+slug+`"}`); code != http.StatusCreated {
		t.Fatalf("create thread = %d %v", code, body)
	}
	main := f.sup.MainID(slug)

	code, body := f.do(t, "GET", "/api/projects/"+slug, "")
	if code != http.StatusOK {
		t.Fatalf("detail = %d %v", code, body)
	}
	if body["slug"] != slug || body["name"] != "Old work" || body["main"] != main {
		t.Errorf("detail = %v", body)
	}
	threads, _ := body["threads"].([]any)
	got := map[string]bool{}
	for _, raw := range threads {
		row, _ := raw.(map[string]any)
		id, _ := row["id"].(string)
		got[id] = true
		if id == main {
			t.Error("main is listed among its own threads")
		}
	}
	if !got["pre-migration"] || !got["filed"] {
		t.Errorf("threads = %v, want the parentless sessions too", got)
	}
	if len(threads) != 3 {
		t.Errorf("threads = %d, want main's thread and the two parentless ones", len(threads))
	}
	// Reading the page must not start anything.
	other := mkProject(t, f, "Never opened")
	f.do(t, "GET", "/api/projects/"+other, "")
	if got := f.sup.MainID(other); got != "" {
		t.Errorf("reading the page started a main thread: %q", got)
	}
}

// Archive stops the project: main, every thread, and the container each
// of them runs. Orbs are per session, so N threads are N+1 containers.
func TestArchiveProjectStopsMainAndEveryThread(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Winding down")
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"HANG"}`); code != http.StatusOK {
		t.Fatal("message failed")
	}
	main := f.sup.MainID(slug)
	_, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+slug+`"}`)
	thread, _ := rowOf(t, body)["id"].(string)
	if thread == "" {
		t.Fatalf("no thread: %v", body)
	}
	waitFor(t, "both sessions to be running", func() bool {
		if !f.sup.Live(main) || !f.sup.Live(thread) {
			return false
		}
		_, a := f.api.info(main)
		_, b := f.api.info(thread)
		return a && b
	})

	ctx := context.Background()
	f.rt.Build(ctx, container.BuildSpec{Tag: "img"}, nil)
	for _, id := range []string{main, thread} {
		if err := f.rt.Start(ctx, container.RunSpec{Name: container.OrbName(id), Image: "img"}); err != nil {
			t.Fatal(err)
		}
		writeState(t, f.home, orb.State{Session: id, Project: slug, Status: orb.StatusRunning, PID: os.Getpid(), UpdatedAt: time.Now()})
	}

	if code, body := f.do(t, "POST", "/api/projects/"+slug+"/archive", ""); code != http.StatusOK {
		t.Fatalf("archive = %d %v", code, body)
	}
	for _, id := range []string{main, thread} {
		if f.sup.Live(id) {
			t.Errorf("%s is still running after archive", id)
		}
		if st, _ := f.rt.Inspect(ctx, container.OrbName(id)); st != container.StateStopped {
			t.Errorf("container of %s = %s, want stopped", id, st)
		}
		if !f.sup.Meta(id).Archived {
			t.Errorf("%s is not archived", id)
		}
	}
	// The project itself stays: archive is "stop showing me this", not
	// a delete, and a message starts it again.
	if _, ok := f.sup.Project(slug); !ok {
		t.Error("archive deleted the project")
	}
}

// Deleting a project stops what it was running first: a thread left
// alive would rebuild its orb from a definition that no longer exists.
func TestDeleteProjectStopsItsSessions(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Doomed")
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"HANG"}`); code != http.StatusOK {
		t.Fatal("message failed")
	}
	main := f.sup.MainID(slug)
	waitFor(t, "main live", func() bool { return f.sup.Live(main) })
	if code, body := f.do(t, "DELETE", "/api/projects/"+slug, ""); code != http.StatusOK {
		t.Fatalf("delete = %d %v", code, body)
	}
	if f.sup.Live(main) {
		t.Error("the main thread outlived its project")
	}
	if got := f.sup.MainID(slug); got != "" {
		t.Errorf("a deleted project still claims main %q", got)
	}
	// The conversation is kept: only the directory goes.
	if code, _ := f.do(t, "GET", "/api/sessions/"+main, ""); code != http.StatusOK {
		t.Error("delete took the main thread's history with it")
	}
}

// The version-1 label table folded into ~/.bough/projects, against a
// real meta.json of the shape that shipped (testdata/meta-v1.json).
//
// This is the test the run cannot do without: a project is its
// directory now, and a migration that loses a display name, collides
// two labels onto one directory or leaves a session pointing at a slug
// that does not exist is not something a person can repair by hand.
func TestMigrateFromMetaV1(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	meta := filepath.Join(home, ".bough", "serve", "meta.json")
	if err := os.MkdirAll(filepath.Dir(meta), 0o755); err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(filepath.Join("testdata", "meta-v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(meta, b, 0o644); err != nil {
		t.Fatal(err)
	}
	// The one label that was attached to a definition. Hand-written, the
	// way a person's is: comments above and below the keys, a real repo,
	// and no name — the display name lived only in the label table.
	const handWritten = `# bough project definition. Lives outside every repo; never committed.
# The web app, the one with the flaky e2e suite.
repos:
  - path: ~/repos/webapp
    branch: main
checks:
  fast: npm test
# caches: [/root/.npm]
`
	dir := filepath.Join(projectdef.Root(home), "my-web-app")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, projectdef.FileYAML), []byte(handWritten), 0o644); err != nil {
		t.Fatal(err)
	}

	opt := Options{Exe: "/bin/true", HistDir: filepath.Join(home, ".bough", "history"), MetaPath: meta, Runtime: container.NewFake()}
	sup, err := NewSupervisor(opt)
	if err != nil {
		t.Fatal(err)
	}
	defer sup.Close()

	// Every label is a directory, keyed by slug, named what it was called.
	want := map[string]string{
		"my-web-app":     "My Web App",
		"platform-infra": "Platform infra",
		"my-web-app-2":   "My web app",
		"project-1":      "🚀 ロケット",
	}
	got := map[string]string{}
	for _, p := range sup.Projects() {
		if p.Error != "" {
			t.Errorf("project %s did not parse after the migration: %s", p.Slug, p.Error)
		}
		got[p.Slug] = p.Name
		if err := projectdef.ValidSlug(p.Slug); err != nil {
			t.Errorf("migration made a slug nothing can use: %v", err)
		}
	}
	if len(got) != len(want) {
		t.Fatalf("projects = %v, want %v", got, want)
	}
	for slug, name := range want {
		if got[slug] != name {
			t.Errorf("project %s = %q, want %q", slug, got[slug], name)
		}
	}

	// The attached label: its name landed in project.yml, and the file it
	// landed in is still the file the person wrote.
	text := readDef(t, home, "my-web-app")
	for _, keep := range []string{
		"# The web app, the one with the flaky e2e suite.",
		"  - path: ~/repos/webapp",
		"  fast: npm test",
		"# caches: [/root/.npm]",
	} {
		if !strings.Contains(text, keep) {
			t.Errorf("migration rewrote the definition; %q is gone:\n%s", keep, text)
		}
	}
	if n := strings.Count(text, "\nname:"); n != 1 {
		t.Errorf("name: appears %d times:\n%s", n, text)
	}

	// A label-only project is a definition with no repos and no build
	// script: the skeleton's placeholder repo would be refused on the
	// next write, and a setup.sh nobody asked for would be built.
	infra := readDef(t, home, "platform-infra")
	if !strings.Contains(infra, "repos: []") {
		t.Errorf("platform-infra project.yml:\n%s", infra)
	}
	for _, f := range []string{projectdef.FileSetup, projectdef.FileDockerfile, projectdef.FileResume} {
		if _, err := os.Stat(filepath.Join(projectdef.Root(home), "platform-infra", f)); !os.IsNotExist(err) {
			t.Errorf("migration wrote a %s nobody asked for (%v)", f, err)
		}
	}

	// Two labels that slugify the same: the second gets a suffix, and the
	// definition the first one already owned is not touched by it.
	if !strings.Contains(readDef(t, home, "my-web-app-2"), "repos: []") {
		t.Error("the colliding label did not get its own empty definition")
	}
	if text != readDef(t, home, "my-web-app") {
		t.Error("the colliding label wrote into the existing project")
	}

	// Sessions moved from label id to slug, and a session whose label is
	// gone comes back unassigned — never pointing at a slug that is not
	// a directory.
	for id, slug := range map[string]string{
		"0199c0f1-7f00-7a11-9000-000000000001": "my-web-app",
		"0199c0f1-7f00-7a11-9000-000000000002": "platform-infra",
		"0199c0f1-7f00-7a11-9000-000000000003": "my-web-app-2",
		"0199c0f1-7f00-7a11-9000-000000000004": "project-1",
		"0199c0f1-7f00-7a11-9000-000000000005": "",
		"0199c0f1-7f00-7a11-9000-000000000006": "",
	} {
		if m := sup.Meta(id); m.Project != slug {
			t.Errorf("session %s project = %q, want %q", id, m.Project, slug)
		}
	}
	// Everything else meta.json held survived the rewrite.
	if m := sup.Meta("0199c0f1-7f00-7a11-9000-000000000002"); !m.Archived {
		t.Error("the archive flag was lost")
	}
	if m := sup.Meta("0199c0f1-7f00-7a11-9000-000000000005"); m.Ack != 12 {
		t.Errorf("ack lost: %+v", m)
	}
	if m := sup.Meta("0199c0f1-7f00-7a11-9000-000000000006"); m.Title != "Nothing to do with a project" || m.Model != "anthropic/claude-opus-4" {
		t.Errorf("session metadata lost in the migration: %+v", m)
	}

	// The table is gone and the file says which schema it is at.
	after, err := os.ReadFile(meta)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(after), `"projects"`) || !strings.Contains(string(after), `"version": 2`) {
		t.Fatalf("meta.json after the migration: %s", after)
	}

	// Running it again changes nothing — not the file, not a directory.
	// A migration that is not idempotent re-mints project-2, project-3…
	// on every boot.
	tree := projectTree(t, home)
	if err := sup.migrateProjects(); err != nil {
		t.Fatal(err)
	}
	sup2, err := NewSupervisor(opt)
	if err != nil {
		t.Fatal(err)
	}
	defer sup2.Close()
	if b, _ := os.ReadFile(meta); string(b) != string(after) {
		t.Errorf("a second migration rewrote meta.json:\n%s\n%s", after, b)
	}
	if now := projectTree(t, home); !maps.Equal(now, tree) {
		t.Errorf("a second migration changed ~/.bough/projects:\n%v\n%v", tree, now)
	}
	if len(sup2.Projects()) != len(want) {
		t.Errorf("second boot projects = %+v", sup2.Projects())
	}
}

func readDef(t *testing.T, home, slug string) string {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(projectdef.Root(home), slug, projectdef.FileYAML))
	if err != nil {
		t.Fatalf("read %s: %v", slug, err)
	}
	return string(b)
}

// projectTree is every file under ~/.bough/projects by relative path.
func projectTree(t *testing.T, home string) map[string]string {
	t.Helper()
	out := map[string]string{}
	root := projectdef.Root(home)
	err := filepath.WalkDir(root, func(p string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		b, err := os.ReadFile(p)
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(root, p)
		out[rel] = string(b)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return out
}

// Archiving is "stop showing me this", and archiveProject says a
// message starts the project again. It could not: Send refuses an
// archived session, nothing on the project page unarchives one, and the
// only way back was finding main in the sidebar's archived list.
func TestMessagingAnArchivedProjectReopensIt(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Back from the dead")
	if code, _ := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"HANG"}`); code != http.StatusOK {
		t.Fatal("first message failed")
	}
	main := f.sup.MainID(slug)
	waitFor(t, "main live", func() bool { return f.sup.Live(main) })
	if code, body := f.do(t, "POST", "/api/projects/"+slug+"/archive", ""); code != http.StatusOK {
		t.Fatalf("archive = %d %v", code, body)
	}
	if !f.sup.Meta(main).Archived {
		t.Fatal("archive left main unarchived")
	}
	if code, body := f.do(t, "POST", "/api/projects/"+slug+"/message", `{"text":"hello again"}`); code != http.StatusOK {
		t.Fatalf("message after archive = %d %v", code, body)
	}
	if f.sup.Meta(main).Archived {
		t.Error("the project is still archived after a message to it")
	}
}

// A thread archived from its own conversation — or by "Archive
// project", which archives every one of them — stops being listed on
// the project page, the way archived sessions are hidden everywhere
// else. Listing them again in Idle made archiving look like a no-op.
func TestProjectDetailHidesArchivedThreads(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Tidy")
	seedSession(t, f, "thread-1")
	if err := f.sup.AssignProject("thread-1", slug); err != nil {
		t.Fatal(err)
	}
	if got := len(detailThreads(t, f, slug)); got != 1 {
		t.Fatalf("threads before archiving = %d, want 1", got)
	}
	if code, body := f.do(t, "POST", "/api/sessions/thread-1/archive", ""); code != http.StatusOK {
		t.Fatalf("archive thread = %d %v", code, body)
	}
	if got := detailThreads(t, f, slug); len(got) != 0 {
		t.Errorf("threads after archiving = %v, want none", got)
	}
}

// detailThreads is the ids GET /api/projects/{slug} lists as threads.
func detailThreads(t *testing.T, f *apiFixture, slug string) []string {
	t.Helper()
	code, body := f.do(t, "GET", "/api/projects/"+slug, "")
	if code != http.StatusOK {
		t.Fatalf("GET project = %d %v", code, body)
	}
	list, _ := body["threads"].([]any)
	var out []string
	for _, row := range list {
		m, _ := row.(map[string]any)
		id, _ := m["id"].(string)
		out = append(out, id)
	}
	return out
}

// migrateV1 boots a supervisor over a hand-written version-1 meta.json.
func migrateV1(t *testing.T, home, metaJSON string) *Supervisor {
	t.Helper()
	meta := filepath.Join(home, ".bough", "serve", "meta.json")
	if err := os.MkdirAll(filepath.Dir(meta), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(meta, []byte(metaJSON), 0o644); err != nil {
		t.Fatal(err)
	}
	sup, err := NewSupervisor(Options{
		Exe: "/bin/true", HistDir: filepath.Join(home, ".bough", "history"),
		MetaPath: meta, Runtime: container.NewFake(),
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { sup.Close() })
	return sup
}

const labelOnlyBough = `{"sessions":{"s1":{"project":"p-label"}},
 "projects":{"p-label":{"id":"p-label","name":"Bough"}}}`

// A definition directory nobody labelled — `bough project create`, or
// an agent writing it, after the last boot of the old binary — is not
// the migration's to take. A label that slugifies onto it used to adopt
// it: rename it, and file its sessions into someone else's repos, image
// and orb.
func TestMigrateLeavesAnUnlabelledDefinitionAlone(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	const mine = `name: The real bough
repos:
  - path: ~/repos/bough
    branch: main
`
	dir := filepath.Join(projectdef.Root(home), "bough")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, projectdef.FileYAML), []byte(mine), 0o644); err != nil {
		t.Fatal(err)
	}
	sup := migrateV1(t, home, labelOnlyBough)

	names := map[string]string{}
	for _, p := range sup.Projects() {
		names[p.Slug] = p.Name
	}
	if names["bough"] != "The real bough" {
		t.Errorf("the unlabelled project is now %q, want %q", names["bough"], "The real bough")
	}
	if names["bough-2"] != "Bough" {
		t.Errorf("the label landed as %v, want bough-2 = Bough", names)
	}
	if got := sup.Meta("s1").Project; got != "bough-2" {
		t.Errorf("the label's session was filed under %q, want bough-2", got)
	}
	if text := readDef(t, home, "bough"); !strings.Contains(text, "~/repos/bough") {
		t.Errorf("the unlabelled definition was rewritten:\n%s", text)
	}
}

// The other half: a directory an earlier run of this same migration
// left behind — this label's name, no repos, nothing else — is finished
// rather than duplicated, or a crash mid-migration would leave an empty
// project beside every real one.
func TestMigrateAdoptsItsOwnLeftover(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	if _, err := projectdef.CreateEmpty(home, "bough", "Bough"); err != nil {
		t.Fatal(err)
	}
	sup := migrateV1(t, home, labelOnlyBough)
	if got := sup.Projects(); len(got) != 1 || got[0].Slug != "bough" || got[0].Name != "Bough" {
		t.Fatalf("projects = %v, want the one leftover adopted", got)
	}
	if got := sup.Meta("s1").Project; got != "bough" {
		t.Errorf("session filed under %q, want bough", got)
	}
}

// The page and the sidebar list one set: a project's threads are the
// /api/sessions rows filed under it. Being main's child is not enough —
// a local child of main nests under main in the sidebar, and a child
// that died before writing history is in no list — and neither may
// show up on the page as a phantom running thread.
func TestProjectDetailMatchesTheSessionList(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	slug := mkProject(t, f, "Omni")
	if code, body := f.do(t, "POST", "/api/sessions", `{"mode":"project","project":"`+slug+`"}`); code != http.StatusCreated {
		t.Fatalf("create thread = %d %v", code, body)
	}
	main := f.sup.MainID(slug)
	seedSession(t, f, "local-kid")
	f.sup.mu.Lock()
	f.sup.meta["local-kid"] = SessionMeta{SpawnedBy: main}
	f.sup.meta["died-early"] = SessionMeta{SpawnedBy: main, Project: slug}
	f.sup.mu.Unlock()

	_, list := f.do(t, "GET", "/api/sessions", "")
	want := map[string]bool{}
	for _, raw := range list["sessions"].([]any) {
		row := raw.(map[string]any)
		id, _ := row["id"].(string)
		if row["project"] == slug && id != main {
			want[id] = true
		}
	}
	_, detail := f.do(t, "GET", "/api/projects/"+slug, "")
	got := map[string]bool{}
	for _, raw := range detail["threads"].([]any) {
		got[raw.(map[string]any)["id"].(string)] = true
	}
	if fmt.Sprint(got) != fmt.Sprint(want) || len(got) != 1 {
		t.Errorf("page threads = %v, sidebar's project rows = %v; want the one real thread in both", got, want)
	}
}
