package graph

import (
	"context"
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/dop251/goja"

	"github.com/andreylukin/bough/kernel"
)

func openTemp(t *testing.T) *Store {
	t.Helper()
	st, err := Open(filepath.Join(t.TempDir(), "graph.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { st.Close() })
	return st
}

func TestKeysAndRefs(t *testing.T) {
	if got := Tickets("andrey/NME-1673-fix SHA-256 sums FOMS-12 NME-1673"); strings.Join(got, ",") != "NME-1673,SHA-256,FOMS-12" {
		t.Fatalf("tickets: %v", got)
	}
	if got := PRs("see https://github.com/acme/uni-nas-event-log/pull/50 and bough#7"); strings.Join(got, ",") != "uni-nas-event-log#50,bough#7" {
		t.Fatalf("prs: %v", got)
	}
	for in, want := range map[string]string{
		"git@github.com:andreylukin/bough.git": "github.com/andreylukin/bough",
		"https://github.com/andreylukin/bough": "github.com/andreylukin/bough",
		"https://GitHub.com/Andrey/Repo.git/":  "github.com/Andrey/Repo",
		"/Users/andrey/repos":                  "/Users/andrey/repos",
	} {
		if got := RepoKey(in); got != want {
			t.Errorf("RepoKey(%q) = %q, want %q", in, got, want)
		}
	}
	for in, want := range map[string]Ref{
		"NME-1673":                             {"ticket", "NME-1673"},
		"uni-nas-event-log#50":                 {"pr", "uni-nas-event-log#50"},
		"https://github.com/a/b/pull/3":        {"pr", "b#3"},
		"Jane.Doe@Example.com":                 {"person", "jane.doe@example.com"},
		"git@github.com:andreylukin/bough.git": {"repo", "github.com/andreylukin/bough"},
		"andrey/NME-9-thing":                   {"ticket", "NME-9"},
		"gitops:promotion":                     {"concept", "gitops:promotion"},
	} {
		got, ok := ParseRef(in)
		if !ok || got != want {
			t.Errorf("ParseRef(%q) = %+v %v, want %+v", in, got, ok, want)
		}
	}
	if _, ok := ParseRef("two words here"); ok {
		t.Error("prose is not a reference")
	}
}

func TestAssertDedupesInvalidateClosesAndTimeTravels(t *testing.T) {
	st := openTemp(t)
	now := time.Date(2026, 9, 2, 12, 0, 0, 0, time.UTC)
	st.now = func() time.Time { return now }
	ep, _ := st.Episode("test", "t")
	pr, _ := st.Upsert("pr", "bough#50", "add graph memory", "")
	tk, _ := st.Upsert("ticket", "NME-1673", "graph memory", "")
	e1, err := st.Assert(pr, "implements", tk, ep, "collector", AssertOpts{})
	if err != nil {
		t.Fatal(err)
	}
	e2, _ := st.Assert(pr, "implements", tk, ep, "collector", AssertOpts{})
	if e1.ID != e2.ID {
		t.Fatalf("asserting twice made two edges: %d %d", e1.ID, e2.ID)
	}
	if _, err := st.Assert(pr, "", tk, ep, "collector", AssertOpts{}); err == nil {
		t.Fatal("an edge needs a rel")
	}
	if _, err := st.Assert(pr, "implements", tk, 0, "collector", AssertOpts{}); err == nil {
		t.Fatal("an edge needs an episode: no orphan facts")
	}

	// The world changed: the PR now implements a different ticket.
	later := now.Add(24 * time.Hour)
	st.now = func() time.Time { return later }
	if err := st.Invalidate(e1.ID, "retargeted to NME-1700", "human", 0); err != nil {
		t.Fatal(err)
	}
	if err := st.Invalidate(e1.ID, "again", "human", 0); err == nil {
		t.Fatal("closing a closed window must fail loudly")
	}
	tk2, _ := st.Upsert("ticket", "NME-1700", "", "")
	if _, err := st.Assert(pr, "implements", tk2, ep, "human", AssertOpts{}); err != nil {
		t.Fatal(err)
	}
	open, _ := st.Neighbors(pr, 1, "", 0)
	if len(open) != 1 || open[0].Dst.Key != "NME-1700" {
		t.Fatalf("now: %+v", open)
	}
	then, _ := st.Neighbors(pr, 1, "", now.Unix()+60)
	if len(then) != 1 || then[0].Dst.Key != "NME-1673" {
		t.Fatalf("point-in-time (yesterday): %+v", then)
	}
	tl, _ := st.Timeline(pr)
	if len(tl) != 2 || tl[0].ValidTo != nil || tl[1].ValidTo == nil {
		t.Fatalf("timeline keeps the closed window: %+v", tl)
	}
	s, _ := st.Stats()
	if s.Edges != 2 || s.OpenEdges != 1 || s.Episodes != 2 {
		t.Fatalf("stats: %+v", s)
	}
}

func TestNeighborsHopsAndRelFilter(t *testing.T) {
	st := openTemp(t)
	ep, _ := st.Episode("test", "t")
	sess, _ := st.Upsert("session", "s1", "", "")
	repo, _ := st.Upsert("repo", "github.com/a/bough", "bough", "")
	pr, _ := st.Upsert("pr", "bough#50", "", "")
	tk, _ := st.Upsert("ticket", "NME-1", "", "")
	who, _ := st.Upsert("person", "jane@x.io", "Jane", "")
	st.Assert(sess, "touches", repo, ep, "session", AssertOpts{})
	st.Assert(pr, "implements", tk, ep, "collector", AssertOpts{})
	st.Assert(who, "reviews", pr, ep, "collector", AssertOpts{})
	st.Assert(sess, "touches", pr, ep, "session", AssertOpts{})
	one, _ := st.Neighbors(tk, 1, "", 0)
	if len(one) != 1 {
		t.Fatalf("1 hop from the ticket: %+v", one)
	}
	two, _ := st.Neighbors(tk, 2, "", 0)
	if len(two) != 3 { // implements, reviews, session touches pr
		t.Fatalf("2 hops from the ticket: %d edges", len(two))
	}
	rev, _ := st.Neighbors(pr, 1, "reviews", 0)
	if len(rev) != 1 || rev[0].Src.Key != "jane@x.io" {
		t.Fatalf("rel filter: %+v", rev)
	}
}

// fakeEmbedder maps text to a 3-vector by keyword, so cosine ranks
// "graph" texts together without a network.
type fakeEmbedder struct{}

func (fakeEmbedder) Model() string { return "fake" }
func (fakeEmbedder) Embed(_ context.Context, text string) ([]float32, error) {
	v := []float32{0.01, 0.01, 0.01}
	t := strings.ToLower(text)
	if strings.Contains(t, "graph") || strings.Contains(t, "memory") {
		v[0] = 1
	}
	if strings.Contains(t, "deploy") || strings.Contains(t, "argocd") {
		v[1] = 1
	}
	return v, nil
}

func TestSearchFusesLexicalAndSemantic(t *testing.T) {
	st := openTemp(t)
	st.SetEmbedder(fakeEmbedder{})
	ep, _ := st.Episode("test", "t")
	a, _ := st.Upsert("concept", "graph-memory", "long-term memory as a graph", "")
	b, _ := st.Upsert("concept", "gitops:promotion", "argocd deploy promotion", "")
	c, _ := st.Upsert("ticket", "NME-1673", "NME-1673", "")
	st.Assert(a, "relates", c, ep, "cheap", AssertOpts{Claim: "the memory graph is tracked under NME-1673"})
	st.Assert(b, "relates", c, ep, "cheap", AssertOpts{Claim: "deploy promotion is unrelated"})

	hits, err := st.Search(t.Context(), "NME-1673", 5)
	if err != nil {
		t.Fatal(err)
	}
	if len(hits) == 0 || hits[0].Entity == nil || hits[0].Entity.Key != "NME-1673" {
		t.Fatalf("a key with punctuation must match lexically first: %+v", hits)
	}
	hits, _ = st.Search(t.Context(), "memory", 5)
	var keys []string
	for _, h := range hits {
		if h.Entity != nil {
			keys = append(keys, h.Entity.Key)
		} else {
			keys = append(keys, "edge:"+h.Edge.Claim)
		}
	}
	joined := strings.Join(keys, "|")
	if !strings.Contains(joined, "graph-memory") || !strings.Contains(joined, "edge:the memory graph") {
		t.Fatalf("lexical+semantic hits: %v", keys)
	}
	if strings.HasPrefix(joined, "gitops") {
		t.Fatalf("the deploy concept must not outrank memory: %v", keys)
	}
	s, _ := st.Stats()
	if s.Embeddings != 5 {
		t.Fatalf("every entity and claim embedded at write time: %+v", s)
	}
	if hits, _ := st.Search(t.Context(), "   ", 5); hits != nil {
		t.Fatal("empty query: nothing")
	}
}

func TestBackfillFromOldDBAndHistory(t *testing.T) {
	dir := t.TempDir()
	old := filepath.Join(dir, "bough.db")
	db, err := sql.Open("sqlite", old)
	if err != nil {
		t.Fatal(err)
	}
	must := func(q string, args ...any) {
		t.Helper()
		if _, err := db.Exec(q, args...); err != nil {
			t.Fatal(err)
		}
	}
	must(`CREATE TABLE notes(id INTEGER PRIMARY KEY, path TEXT, title TEXT, created_at INTEGER)`)
	must(`CREATE TABLE note_sections(id INTEGER PRIMARY KEY, note_id INTEGER)`)
	must(`CREATE TABLE section_citations(section_id INTEGER, kind TEXT, ref TEXT, at INTEGER)`)
	must(`CREATE TABLE command_history(id INTEGER PRIMARY KEY, session_id TEXT, ts INTEGER, repo TEXT, cmd TEXT, tags TEXT, exit_code INTEGER)`)
	must(`INSERT INTO notes VALUES(1,'nased','NASED service',1700000000)`)
	must(`INSERT INTO note_sections VALUES(10,1)`)
	must(`INSERT INTO section_citations VALUES(10,'url','https://example.com/doc',1700000001)`)
	must(`INSERT INTO command_history VALUES(1,'sess-a',1700000002,'git@github.com:acme/nased.git','git checkout -b andrey/NME-77-fix','git:checkout',0)`)
	must(`INSERT INTO command_history VALUES(2,'sess-a',1700000003,'git@github.com:acme/nased.git','make test','make',1)`)
	db.Close()

	hist := filepath.Join(dir, "history")
	os.MkdirAll(hist, 0o755)
	lines := []string{
		`{"seq":1,"at":"2026-09-01T22:32:34Z","kind":"meta","data":{"cwd":"/nowhere/special"}}`,
		`{"seq":2,"at":"2026-09-01T22:32:35Z","kind":"input","data":{"text":"fix NME-77 per https://github.com/acme/nased/pull/9"}}`,
		`{"seq":3,"at":"2026-09-01T22:32:36Z","kind":"done","data":{"text":""}}`,
	}
	os.WriteFile(filepath.Join(hist, "2026-09-01T22:32:34Z-1.jsonl"), []byte(strings.Join(lines, "\n")+"\n"), 0o644)

	st := openTemp(t)
	r, err := st.Backfill(old, hist)
	if err != nil {
		t.Fatal(err)
	}
	if r.Concepts != 1 || r.Cites != 1 || r.Commands != 2 || r.Repos != 1 || r.Sessions != 2 {
		t.Fatalf("report: %+v", r)
	}
	tk, err := st.Get("ticket", "NME-77")
	if err != nil {
		t.Fatal("the ticket typed into a checkout and named in a prompt must exist")
	}
	edges, _ := st.Neighbors(tk, 1, "touches", 0)
	if len(edges) != 2 {
		t.Fatalf("both sessions touch NME-77: %+v", edges)
	}
	if _, err := st.Get("pr", "nased#9"); err != nil {
		t.Fatal("the PR url in the prompt becomes a pr entity")
	}
	if _, err := st.Get("repo", "github.com/acme/nased"); err != nil {
		t.Fatal("the origin is normalized to a repo key")
	}
	if _, err := st.Get("url", "https://example.com/doc"); err != nil {
		t.Fatal("a url citation is a url entity")
	}
	// Idempotent: same counts, no duplicate edges.
	before, _ := st.Stats()
	if _, err := st.Backfill(old, hist); err != nil {
		t.Fatal(err)
	}
	after, _ := st.Stats()
	if after.Edges != before.Edges || after.Entities != before.Entities {
		t.Fatalf("backfill twice grew the graph: %+v → %+v", before, after)
	}
	r, _ = st.Backfill(filepath.Join(dir, "missing.db"), "")
	if len(r.Skipped) != 1 {
		t.Fatalf("a missing source is reported, not fatal: %+v", r)
	}
}

func TestPromptSectionAndVerbs(t *testing.T) {
	st := openTemp(t)
	ep, _ := st.Episode("session", "s-now")
	sess, _ := st.Upsert("session", "s-now", "", "")
	svc := &Service{Store: st, session: sess, episode: ep}
	ws := WorkspaceInfo{Repo: "github.com/acme/nased", Branch: "andrey/NME-77-fix", Tickets: []string{"NME-77"}}
	if s := svc.PromptSection(ws, 1, 40); s != "" {
		t.Fatalf("an empty graph adds no prompt bytes: %q", s)
	}
	svc.recordWorkspace(ws)
	pr, _ := st.Upsert("pr", "nased#9", "fix the thing", "")
	tk, _ := st.Get("ticket", "NME-77")
	st.Assert(pr, "implements", tk, ep, "collector", AssertOpts{})
	sec := svc.PromptSection(ws, 1, 40)
	if !strings.HasPrefix(sec, "## memory") || !strings.Contains(sec, "pr:nased#9") || !strings.Contains(sec, "implements") {
		t.Fatalf("section:\n%s", sec)
	}
	if strings.Contains(sec, "session:") {
		t.Fatalf("this session's own edges are not memory:\n%s", sec)
	}

	// The verbs through goja, the way the model calls them.
	vm := goja.New()
	tools := vm.NewObject()
	vm.Set("tools", tools)
	tools.Set("graph", svc.jsObject(vm))
	v, err := vm.RunString(`
		var e = tools.graph.assert("NME-77", "relates", "concept:nased", "the ticket is about the NASED service");
		var n = tools.graph.neighbors("NME-77", 1, "");
		var r = tools.graph.resolve("andrey/NME-77-fix");
		var s = tools.graph.search("NASED", 5);
		JSON.stringify({edge: e.rel, dst: e.dst.key, author: e.author, n: n.length, r: r.key, s: s.length})
	`)
	if err != nil {
		t.Fatal(err)
	}
	var got struct {
		Edge, Dst, Author, R string
		N, S                 int
	}
	json.Unmarshal([]byte(v.String()), &got)
	if got.Edge != "relates" || got.Dst != "nased" || got.Author != "session" || got.N != 3 || got.R != "NME-77" || got.S == 0 {
		t.Fatalf("verbs: %+v", got)
	}
	if _, err := vm.RunString(`tools.graph.invalidate(9999, "nope")`); err == nil {
		t.Fatal("invalidating a missing edge must throw")
	}
	if _, err := vm.RunString(`tools.graph.resolve("no such thing here")`); err == nil {
		t.Fatal("an unreadable reference must throw")
	}
}

func TestParseConfigAndMount(t *testing.T) {
	if _, err := parseConfig(map[string]any{"hops": 9}); err == nil {
		t.Fatal("hops out of range")
	}
	if _, err := parseConfig(map[string]any{"nope": 1}); err == nil {
		t.Fatal("unknown key")
	}
	c, err := parseConfig(map[string]any{"path": "/x/g.db", "embed": false, "max_rows": 5})
	if err != nil || c.Path != "/x/g.db" || c.Embed || c.MaxRows != 5 || c.Hops != 1 {
		t.Fatalf("config: %+v %v", c, err)
	}
	ctx := kernel.NewContext()
	path := filepath.Join(t.TempDir(), "g.db")
	if err := (plugin{}).Apply(ctx, map[string]any{"path": path, "embed": false}); err != nil {
		t.Fatal(err)
	}
	svc, err := kernel.Get[*Service](ctx, "graph")
	if err != nil {
		t.Fatal(err)
	}
	if svc.session.ID == 0 || svc.episode == 0 {
		t.Fatalf("a mount records its session: %+v", svc)
	}
	ctx.Unmount()
}

// bough.db keeps epoch milliseconds; the graph works in seconds. Every
// edge the old-database backfill wrote was dated in the year 58000,
// where no time-bounded query could reach it — 1,076 of 1,162 edges in
// a real graph.
func TestBackfillTimestampsAreSeconds(t *testing.T) {
	ms := int64(1787355955996) // 2026-08-21 in milliseconds
	if got := secs(ms); got != 1787355955 {
		t.Fatalf("secs(%d) = %d", ms, got)
	}
	// A value already in seconds is left alone.
	for _, s := range []int64{0, 1, 1787355955} {
		if got := secs(s); got != s {
			t.Fatalf("secs(%d) = %d, want it unchanged", s, got)
		}
	}
	// The point of the conversion: an edge dated from it is visible now.
	if secs(ms) > time.Now().Unix() {
		t.Fatal("a 2026 timestamp still lands in the future")
	}
}

func TestVocabularyIsClosed(t *testing.T) {
	st := openTemp(t)
	ep, _ := st.Episode("test", "t")
	a, _ := st.Upsert("concept", "a", "", "")
	b, _ := st.Upsert("concept", "b", "", "")
	if _, err := st.Assert(a, "depends_on", b, ep, "session", AssertOpts{}); err == nil {
		t.Fatal("an unlisted rel must be refused at the store")
	}
	for in, want := range map[string]string{"Requires": "requires", "depends on": "requires", "supersedes": "replaces", "lives_in": "relates"} {
		got, _ := NormalizeRel(in)
		if got != want {
			t.Errorf("NormalizeRel(%q) = %q, want %q", in, got, want)
		}
	}
	svc := &Service{Store: st, episode: ep}
	e, err := svc.AssertAs("cheap", "concept:a", "lives_in", "concept:b", "a is under b")
	if err != nil {
		t.Fatal(err)
	}
	if e.Rel != "relates" || e.Author != "cheap" || !strings.HasPrefix(e.Claim, "(lives_in)") {
		t.Fatalf("free verb folds to relates and keeps the wording, signed cheap: %+v", e)
	}
}

func TestSetStateAndWorld(t *testing.T) {
	st := openTemp(t)
	now := time.Date(2026, 9, 6, 12, 0, 0, 0, time.UTC)
	st.now = func() time.Time { return now }
	ep, _ := st.Episode("collector:github", "t")
	me, _ := st.Upsert("person", "andrey@example.com", "Andrey", "")
	bradley, _ := st.Upsert("person", "bradley@example.com", "Bradley", "")
	pr, _ := st.Upsert("pr", "uni-nas-event-log#46", "fix sharding", "")
	pr, _ = st.SetLink(pr, Link{URL: "https://github.com/x/uni-nas-event-log/pull/46", Summary: "2 unresolved Devin threads", UpdatedAt: now.Unix()})
	if err := st.SetState(pr, "open", ep, "collector", now.Unix()); err != nil {
		t.Fatal(err)
	}
	st.Assert(me, "authored", pr, ep, "collector", AssertOpts{})
	st.Assert(pr, "awaits", bradley, ep, "collector", AssertOpts{})
	other, _ := st.Upsert("pr", "uni-orb#142", "alert backtesting", "")
	st.Assert(other, "awaits", me, ep, "collector", AssertOpts{})

	w, err := st.WorldOf(me)
	if err != nil {
		t.Fatal(err)
	}
	out := w.Render()
	for _, want := range []string{"Waiting on me:", "pr:uni-orb#142", "Mine, open:", "pr:uni-nas-event-log#46", "[open]", "awaits Bradley", "pull/46", "Devin threads", "collected 2026-09-06"} {
		if !strings.Contains(out, want) {
			t.Errorf("world lacks %q:\n%s", want, out)
		}
	}

	// Merged: the state edge closes, a new one opens, and the PR leaves the world.
	later := now.Add(time.Hour)
	if err := st.SetState(pr, "merged", ep, "collector", later.Unix()); err != nil {
		t.Fatal(err)
	}
	tl, _ := st.Timeline(pr)
	var states []string
	for _, e := range tl {
		if e.Rel == "has_state" {
			states = append(states, e.Dst.Key)
		}
	}
	if len(states) != 2 {
		t.Fatalf("timeline states: %v", states)
	}
	w, _ = st.WorldOf(me)
	if len(w.Mine) != 0 {
		t.Fatalf("a merged PR is not open: %+v", w.Mine)
	}
	if err := st.SetState(pr, "merged", ep, "collector", 0); err != nil {
		t.Fatal(err)
	}
	if tl, _ = st.Timeline(pr); len(tl) != 4 {
		t.Fatalf("an unchanged state adds nothing: %d edges", len(tl))
	}
}
