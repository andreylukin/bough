package wiki

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// storeFixture is a home with one history session and a two-page wiki
// that has one claim in every state.
func storeFixture(t *testing.T) (*Store, string) {
	t.Helper()
	home := t.TempDir()
	s := Open(home)
	at := time.Date(2026, 9, 10, 12, 0, 0, 0, time.UTC)
	writeSession(t, s.p.hist, "s1", "/repo", at, []entry{
		{Seq: 2, Kind: "input", Data: map[string]any{"text": "why is the ghost test flaky"}},
		{Seq: 3, Kind: "thinking", Data: map[string]any{"text": "hmm"}},
		{Seq: 4, Kind: "result", Data: map[string]any{"text": "--- FAIL: TestGhost\nwant 3 frames, got 2"}},
		{Seq: 5, Kind: "assistant", Data: map[string]any{"text": "The first read is empty."}},
	})
	write(t, filepath.Join(s.p.wiki, "index.md"), "# Wiki index\n\n## go-testing\n\n- [Ghost tests](topics/go-testing/ghost.md) — flaky under -race\n")
	write(t, filepath.Join(s.p.wiki, "topics", "go-testing", "ghost.md"), strings.Join([]string{
		"# Ghost tests",
		"",
		"The suite drops a frame under load.",
		"",
		"## What happens",
		"",
		"The harness counts an empty read as a frame `s1#4`.",
		"",
		"- The fix is a wait `s1#99`.",
		"- tmux is unaffected.",
		"- *Inference:* the macOS flakes are the same race.",
		"- **Outdated** (superseded by `s1#5`): -parallel 1 is the only workaround `s1#4`.",
		"",
		"## See also",
		"- [Gate](../go-testing/gate.md)",
		"",
		"Updated: 2026-09-11 · Sessions: `s1#4`",
		"",
	}, "\n"))
	write(t, filepath.Join(s.p.wiki, "topics", "go-testing", "gate.md"), "# Gate\n\nRead the gate `s1#4`.\n\n## See also\n- [Ghost](ghost.md)\n")
	return s, home
}

func write(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func writeSession(t *testing.T, hist, id, cwd string, at time.Time, es []entry) {
	t.Helper()
	var b strings.Builder
	all := append([]entry{{Seq: 1, Kind: "meta", Data: map[string]any{"cwd": cwd}}}, es...)
	for i, e := range all {
		e.At = at.Add(time.Duration(i) * time.Second)
		line, _ := json.Marshal(e)
		b.Write(line)
		b.WriteByte('\n')
	}
	write(t, filepath.Join(hist, id+".jsonl"), b.String())
	old := at.Add(-24 * time.Hour)
	_ = os.Chtimes(filepath.Join(hist, id+".jsonl"), old, old)
}

func TestPageStates(t *testing.T) {
	s, _ := storeFixture(t)
	pg, err := s.Page("topics/go-testing/ghost.md")
	if err != nil {
		t.Fatal(err)
	}
	want := Counts{Cited: 1, Inferred: 1, Uncited: 1, Unsupported: 1, Superseded: 1}
	if pg.Counts != want {
		t.Fatalf("counts = %+v, want %+v", pg.Counts, want)
	}
	if pg.Summary != "The suite drops a frame under load." || pg.Updated != "2026-09-11" {
		t.Fatalf("summary %q updated %q", pg.Summary, pg.Updated)
	}
	var claims []Block
	for _, b := range pg.Blocks {
		if b.Kind == "claim" {
			claims = append(claims, b)
		}
	}
	if len(claims) != 5 {
		t.Fatalf("%d claims, want 5: %+v", len(claims), pg.Blocks)
	}
	cited := claims[0]
	if cited.Text != "The harness counts an empty read as a frame." || cited.Line != 7 {
		t.Fatalf("cited claim text %q line %d", cited.Text, cited.Line)
	}
	if c := cited.Cites[0]; c.Label != "output" || !strings.Contains(c.Excerpt, "want 3 frames") {
		t.Fatalf("cite excerpt = %+v", c)
	}
	if claims[1].State != "unsupported" || !strings.Contains(claims[1].Cites[0].Problem, "#99") {
		t.Fatalf("broken cite not unsupported: %+v", claims[1])
	}
	if claims[3].Text != "the macOS flakes are the same race." {
		t.Fatalf("inference label left in text: %q", claims[3].Text)
	}
	sup := claims[4]
	if sup.State != "superseded" || sup.SupersededBy == nil || sup.SupersededBy.Seq != 5 || sup.Text != "-parallel 1 is the only workaround." {
		t.Fatalf("superseded = %+v", sup)
	}
	if len(pg.LinkedFrom) != 1 || pg.LinkedFrom[0].Path != "topics/go-testing/gate.md" {
		t.Fatalf("linkedFrom = %+v", pg.LinkedFrom)
	}
}

func TestIndexListsUnindexedPagesAndHealth(t *testing.T) {
	s, _ := storeFixture(t)
	ix := s.Index(time.Now())
	if !ix.Exists || len(ix.Topics) != 1 || len(ix.Topics[0].Pages) != 2 {
		t.Fatalf("topics = %+v", ix.Topics)
	}
	if ix.Topics[0].Pages[0].Summary != "flaky under -race" {
		t.Fatalf("index summary not used: %+v", ix.Topics[0].Pages[0])
	}
	h := ix.Health
	if h.Unsupported != 1 || h.Superseded != 1 || h.Uncited != 1 {
		t.Fatalf("health = %+v", h)
	}
	if ix.Thin != 1 { // gate.md cites one entry
		t.Fatalf("thin = %d", ix.Thin)
	}
}

func TestIndexLineColonSeparator(t *testing.T) {
	m := indexLineRE.FindStringSubmatch("- [Sentinels](db/sentinels.md): PostgreSQL sentinels, retries")
	if m == nil || m[3] != "PostgreSQL sentinels, retries" {
		t.Fatalf("match = %q", m)
	}
}

func TestShortDuration(t *testing.T) {
	for d, want := range map[time.Duration]string{5 * time.Minute: "5m", time.Hour: "1h", 90 * time.Second: "1m30s"} {
		if got := shortDuration(d); got != want {
			t.Fatalf("shortDuration(%v) = %q, want %q", d, got, want)
		}
	}
}

func TestFocusStartsAtTheClaimedSpan(t *testing.T) {
	text := "Looked at the build first.\nNothing there.\nThe scheduler retries each ingest twice before giving up."
	got := focus(text, "Ingest retries twice `abc#3`.")
	if got != "… The scheduler retries each ingest twice before giving up." {
		t.Fatalf("focus = %q", got)
	}
	if focus(text, "unrelated words entirely") != text {
		t.Fatal("no overlap should keep the text whole")
	}
}

func TestIndexWithoutWiki(t *testing.T) {
	ix := Open(t.TempDir()).Index(time.Now())
	if ix.Exists || ix.Topics == nil {
		t.Fatalf("index = %+v", ix)
	}
}

func TestReview(t *testing.T) {
	s, _ := storeFixture(t)
	rv := s.Review(time.Now())
	kinds := map[string]int{}
	for _, f := range rv.Flags {
		kinds[f.Kind]++
	}
	// gate.md is not in the index: a page-level problem. The broken
	// citation is already its claim's flag and is not listed twice.
	want := map[string]int{"unsupported": 1, "superseded": 1, "uncited": 1, "problem": 1}
	for k, n := range want {
		if kinds[k] != n {
			t.Fatalf("flags = %+v", rv.Flags)
		}
	}
	for _, f := range rv.Flags {
		if f.Kind == "superseded" && !strings.Contains(f.Evidence, "first read is empty") {
			t.Fatalf("superseded evidence = %q", f.Evidence)
		}
	}
}

func TestEditClaim(t *testing.T) {
	s, _ := storeFixture(t)
	rel := "topics/go-testing/ghost.md"
	find := func(state string) Block {
		pg, _ := s.Page(rel)
		for _, b := range pg.Blocks {
			if b.State == state {
				return b
			}
		}
		t.Fatalf("no %s claim", state)
		return Block{}
	}
	u := find("uncited")
	if err := s.EditClaim(rel, u.Line, u.End, u.Raw+"x", "inference"); !errors.Is(err, ErrStale) {
		t.Fatalf("stale edit err = %v", err)
	}
	if err := s.EditClaim(rel, u.Line, u.End, u.Raw, "inference"); err != nil {
		t.Fatal(err)
	}
	pg, _ := s.Page(rel)
	if pg.Counts.Uncited != 0 || pg.Counts.Inferred != 2 {
		t.Fatalf("after inference counts = %+v", pg.Counts)
	}
	sup := find("superseded")
	if err := s.EditClaim(rel, sup.Line, sup.End, sup.Raw, "drop"); err != nil {
		t.Fatal(err)
	}
	pg, _ = s.Page(rel)
	if pg.Counts.Superseded != 0 || strings.Contains(pg.Body, "Outdated") {
		t.Fatalf("drop left the claim: %q", pg.Body)
	}
}

func TestPagePathRefusesOutsideTopics(t *testing.T) {
	s, home := storeFixture(t)
	write(t, filepath.Join(home, "secret.md"), "no")
	if err := os.Symlink(home, filepath.Join(s.p.wiki, "topics", "out")); err != nil {
		t.Fatal(err)
	}
	for _, rel := range []string{"", "index.md", "log.md", "/etc/x.md", "topics/../index.md", "topics/go-testing/ghost.txt", "topics/out/secret.md"} {
		if _, err := s.pagePath(rel); !errors.Is(err, ErrBadPath) {
			t.Errorf("pagePath(%q) err = %v, want ErrBadPath", rel, err)
		}
	}
	if err := s.WritePage("topics/out/secret.md", "pwned"); err == nil {
		t.Fatal("wrote through a symlink out of the wiki")
	}
}

// The Me page's empty state opens topics/me/profile.md before it
// exists, so the page view must have something to edit and the save
// must create the file. Any other missing page stays not found.
func TestProfileIsWritableBeforeItExists(t *testing.T) {
	t.Parallel()
	s := Open(t.TempDir())
	pg, err := s.Page(ProfilePath)
	if err != nil {
		t.Fatalf("Page(%s) with no profile: %v", ProfilePath, err)
	}
	if !strings.Contains(pg.Body, "## Not mine") {
		t.Fatalf("the missing profile opens without its sections:\n%s", pg.Body)
	}
	if s.Me(time.Now()).HasProfile {
		t.Fatal("reading the missing profile created it")
	}
	if err := s.WritePage(ProfilePath, pg.Body+"\nI own acme/web.\n"); err != nil {
		t.Fatalf("WritePage(%s) with no profile: %v", ProfilePath, err)
	}
	if !s.Me(time.Now()).HasProfile {
		t.Fatal("saving the profile did not create it")
	}
	if err := s.WritePage("topics/me/other.md", "x"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("WritePage of another missing page: %v, want ErrNotFound", err)
	}
	if _, err := s.Page("topics/me/other.md"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("Page of another missing page: %v, want ErrNotFound", err)
	}
}

func TestSource(t *testing.T) {
	s, _ := storeFixture(t)
	src, err := s.Source("s1", 4)
	if err != nil {
		t.Fatal(err)
	}
	var seqs []int64
	for _, l := range src.Lines {
		seqs = append(seqs, l.Seq)
	}
	// thinking (#3) is not something the digest shows, so it is not shown here.
	if len(seqs) != 4 || seqs[0] != 1 || seqs[2] != 4 || src.Total != 5 {
		t.Fatalf("lines %v total %d", seqs, src.Total)
	}
	if len(src.CitedBy) != 2 {
		t.Fatalf("citedBy = %+v", src.CitedBy)
	}
	if _, err := s.Source("s1", 99); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing entry err = %v", err)
	}
	if _, err := s.Source("../s1", 4); !errors.Is(err, ErrNotFound) {
		t.Fatalf("traversal err = %v", err)
	}
}

func TestActivity(t *testing.T) {
	s, _ := storeFixture(t)
	now := time.Date(2026, 9, 10, 18, 0, 0, 0, time.UTC)
	writeSession(t, s.p.hist, "run1", s.p.wiki, now.Add(-time.Hour), []entry{
		{Seq: 2, Kind: "input", Data: map[string]any{"text": "/llm-wiki ingest s1\n\n[skill: llm-wiki]\n---\nname: llm-wiki"}},
		{Seq: 3, Kind: "done", Data: map[string]any{"files": []any{"log.md", "topics/go-testing/ghost.md"},
			"usage": map[string]any{"cost": 0.25}}},
	})
	write(t, s.p.log(), "# Wiki log\n\n## [2026-09-10] ingest | s1#5 | Update | topics/go-testing/ghost.md\n")
	a := s.Activity(now)
	if len(a.Runs) != 1 {
		t.Fatalf("runs = %+v", a.Runs)
	}
	r := a.Runs[0]
	if r.Cost != 0.25 || r.Running || r.Ms != 2000 || len(r.Files) != 2 {
		t.Fatalf("run = %+v", r)
	}
	if len(r.Outcomes) != 1 || r.Outcomes[0].Disposition != "Update" || r.Outcomes[0].Title == "" {
		t.Fatalf("outcomes = %+v", r.Outcomes)
	}
	if a.Today.Runs != 1 || a.Today.Ingested != 1 || a.Spent != 0.25 {
		t.Fatalf("today = %+v spent %v", a.Today, a.Spent)
	}
	// The ingest session itself is not history waiting to be compiled.
	for _, x := range s.Review(now).Pending {
		if x.ID == "run1" {
			t.Fatal("an ingest run is listed as pending")
		}
	}
}

func TestSearch(t *testing.T) {
	s, _ := storeFixture(t)
	hits := s.Search("empty read", 10)
	if len(hits) != 1 || hits[0].Path != "topics/go-testing/ghost.md" || !strings.Contains(hits[0].Excerpt, "empty read") {
		t.Fatalf("hits = %+v", hits)
	}
	if got := s.Search("gate", 10); len(got) != 2 || got[0].Title != "Gate" {
		t.Fatalf("title match not first: %+v", got)
	}
	if len(s.Search("  ", 10)) != 0 {
		t.Fatal("blank query matched")
	}
}

func TestExternalCitationsAreCitedAndLinked(t *testing.T) {
	var b Block
	b.Kind = "claim"
	classify(&b, "Waiting on Priya's review. `gh:asi/uni-nes#7801` `notion:abc-123` `url:https://x.y/z`")
	if b.State != "cited" || len(b.Cites) != 3 {
		t.Fatalf("state=%s cites=%+v", b.State, b.Cites)
	}
	if b.Cites[0].URL != "https://github.com/asi/uni-nes/issues/7801" || b.Cites[1].URL != "https://www.notion.so/abc123" || b.Cites[2].URL != "https://x.y/z" {
		t.Fatalf("urls: %+v", b.Cites)
	}
	if b.Text != "Waiting on Priya's review." {
		t.Fatalf("text = %q", b.Text)
	}
	// Mixed with a session citation, both are kept and neither is confused for the other.
	var m Block
	m.Kind = "claim"
	classify(&m, "Ran the tests. `01a0c046-e48e-716b-9f1c-26c93cbbc95d#12` `gh:asi/uni-nes#7801`")
	if len(m.Cites) != 2 || m.Cites[0].Source != "gh" || m.Cites[1].Session == "" || m.Cites[1].Seq != 12 {
		t.Fatalf("cites = %+v", m.Cites)
	}
}
