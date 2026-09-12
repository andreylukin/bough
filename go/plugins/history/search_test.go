package history

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// write builds a session file whose entries are the given kind/text
// pairs, and returns its path.
func writeSession(t *testing.T, dir, id string, meta map[string]any, kv ...[2]string) string {
	t.Helper()
	p := filepath.Join(dir, id+".jsonl")
	s, err := Open(p)
	if err != nil {
		t.Fatal(err)
	}
	if meta != nil {
		s.Append("meta", meta)
	}
	for _, e := range kv {
		s.Append(e[0], map[string]any{"text": e[1]})
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestParseQueryPrefixesAndTerms(t *testing.T) {
	t.Parallel()
	q, err := ParseQuery("Postgres MIGRATION repo:bough branch:feat-x")
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(q.Terms, ",") != "postgres,migration" {
		t.Errorf("terms = %v, want lowercased free text only", q.Terms)
	}
	if q.Repo != "bough" || q.Branch != "feat-x" {
		t.Errorf("repo=%q branch=%q", q.Repo, q.Branch)
	}
	// An unknown prefix is a word: people search for "TODO:" and "fix:".
	q, err = ParseQuery("TODO: note:")
	if err != nil {
		t.Fatal(err)
	}
	if len(q.Terms) != 2 {
		t.Errorf("unknown prefixes should stay terms, got %v", q.Terms)
	}
}

func TestParseQuerySince(t *testing.T) {
	t.Parallel()
	for _, v := range []string{"7d", "24h", "2w", "2026-09-01"} {
		q, err := ParseQuery("since:" + v)
		if err != nil {
			t.Fatalf("since:%s: %v", v, err)
		}
		if q.Since.IsZero() {
			t.Errorf("since:%s produced no bound", v)
		}
	}
	if _, err := ParseQuery("since:soon"); err == nil {
		t.Error("an unparseable since must be an error, not a silent no-op")
	}
}

func TestSearchFindsTermAnywhereInTheCorpus(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", map[string]any{"cwd": "/x"}, [2]string{"input", "fix the parser"})
	writeSession(t, dir, "b", map[string]any{"cwd": "/x"}, [2]string{"assistant", "the postgres migration is reverted"})

	got, err := Search(dir, mustQuery(t, "postgres"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].ID != "b" {
		t.Fatalf("search = %+v, want only session b", got)
	}
	if got[0].Hits != 1 || len(got[0].Lines) != 1 || !strings.Contains(got[0].Lines[0].Text, "postgres") {
		t.Errorf("match should carry the line that matched: %+v", got[0])
	}
	// The match is on the assistant's words, which a title-only filter
	// would never see: the session's title is the first input.
	if strings.Contains(strings.ToLower(got[0].Title), "postgres") {
		t.Fatal("precondition: the term must not be in the title")
	}
}

func TestSearchRequiresEveryTerm(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", nil, [2]string{"input", "postgres"}, [2]string{"assistant", "migration"})
	writeSession(t, dir, "b", nil, [2]string{"input", "postgres only"})

	got, err := Search(dir, mustQuery(t, "postgres migration"), 0)
	if err != nil {
		t.Fatal(err)
	}
	// Terms may land in different entries of the same session, but a
	// session missing one of them is not a match.
	if len(got) != 1 || got[0].ID != "a" {
		t.Fatalf("search = %v, want only a", ids(got))
	}
}

func TestSearchOrdersByRecencyAndObeysLimit(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	old := writeSession(t, dir, "old", nil, [2]string{"input", "shared term"})
	mid := writeSession(t, dir, "mid", nil, [2]string{"input", "shared term"})
	writeSession(t, dir, "new", nil, [2]string{"input", "shared term"})
	now := time.Now()
	os.Chtimes(old, now.Add(-48*time.Hour), now.Add(-48*time.Hour))
	os.Chtimes(mid, now.Add(-24*time.Hour), now.Add(-24*time.Hour))

	got, err := Search(dir, mustQuery(t, "shared"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if ids(got) != "new,mid,old" {
		t.Errorf("order = %s, want newest first", ids(got))
	}
	if got, _ := Search(dir, mustQuery(t, "shared"), 2); len(got) != 2 {
		t.Errorf("limit ignored, got %d", len(got))
	}
}

func TestSearchSinceBoundsByLastActivity(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	old := writeSession(t, dir, "old", nil, [2]string{"input", "term"})
	writeSession(t, dir, "new", nil, [2]string{"input", "term"})
	stale := time.Now().AddDate(0, 0, -30)
	os.Chtimes(old, stale, stale)

	got, err := Search(dir, mustQuery(t, "term since:7d"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if ids(got) != "new" {
		t.Errorf("since:7d = %s, want only the recent session", ids(got))
	}
}

func TestSearchRepoMatchesMetadataThenFilesTouched(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "meta", map[string]any{"cwd": "/tmp", "repo": "/Users/me/repos/bough"}, [2]string{"input", "term"})
	writeSession(t, dir, "cwd", map[string]any{"cwd": "/Users/me/repos/other"}, [2]string{"input", "term"})

	// A session with neither recorded still matches on what it wrote,
	// which is the only signal older sessions have.
	p := filepath.Join(dir, "files.jsonl")
	s, err := Open(p)
	if err != nil {
		t.Fatal(err)
	}
	s.Append("meta", map[string]any{"cwd": "/home"})
	s.Append("input", map[string]any{"text": "term"})
	s.Append("done", map[string]any{"files": []any{"/Users/me/repos/bough/go/main.go"}})
	s.Close()

	got, err := Search(dir, mustQuery(t, "term repo:bough"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 {
		t.Fatalf("repo:bough matched %s, want the meta and files sessions", ids(got))
	}
	for _, m := range got {
		if m.ID == "cwd" {
			t.Error("repo:bough must not match an unrelated checkout")
		}
	}
}

func TestSearchFilterOnlyQueryMatchesWithoutPreview(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", map[string]any{"cwd": "/Users/me/repos/bough"}, [2]string{"input", "anything"})

	got, err := Search(dir, mustQuery(t, "repo:bough"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || len(got[0].Lines) != 0 {
		t.Errorf("a filter-only query matches with nothing to preview, got %+v", got)
	}
}

func mustQuery(t *testing.T, s string) Query {
	t.Helper()
	q, err := ParseQuery(s)
	if err != nil {
		t.Fatal(err)
	}
	return q
}

func ids(ms []Match) string {
	var out []string
	for _, m := range ms {
		out = append(out, m.ID)
	}
	return strings.Join(out, ",")
}

// The preview must point at the match. Showing an entry's first line
// instead surfaced paths and JSON blobs while the matching text sat
// further down the same result.
func TestPreviewShowsTheMatchingLineNotTheFirst(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", nil,
		[2]string{"result", "/Users/me/repos/bough\nsome noise\nthe rate limiter drops the third retry"})

	got, err := Search(dir, mustQuery(t, "limiter"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || len(got[0].Lines) != 1 {
		t.Fatalf("matches = %+v", got)
	}
	if line := got[0].Lines[0].Text; !strings.Contains(line, "limiter") {
		t.Errorf("preview = %q, want the line carrying the term", line)
	}
}

// Tool output is worth searching but makes a poor preview, so what was
// said wins the limited slots.
func TestPreviewPrefersWhatWasSaidOverToolOutput(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", nil,
		[2]string{"result", "postgres in tool output one"},
		[2]string{"result", "postgres in tool output two"},
		[2]string{"result", "postgres in tool output three"},
		[2]string{"result", "postgres in tool output four"},
		[2]string{"assistant", "postgres is what I concluded"},
	)

	got, err := Search(dir, mustQuery(t, "postgres"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || len(got[0].Lines) != linesPerSession {
		t.Fatalf("match = %+v", got)
	}
	said := false
	for _, l := range got[0].Lines {
		if l.Kind == "assistant" {
			said = true
		}
	}
	if !said {
		t.Errorf("the assistant's line must survive the cut: %+v", got[0].Lines)
	}
}

func TestPreviewLinesStayInTranscriptOrder(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", nil,
		[2]string{"assistant", "postgres first"},
		[2]string{"assistant", "postgres second"},
		[2]string{"assistant", "postgres third"},
	)

	got, err := Search(dir, mustQuery(t, "postgres"), 0)
	if err != nil {
		t.Fatal(err)
	}
	lines := got[0].Lines
	for i := 1; i < len(lines); i++ {
		if lines[i].Seq <= lines[i-1].Seq {
			t.Fatalf("preview out of transcript order: %+v", lines)
		}
	}
}

// A line carrying more of the query wins a scarce preview slot. It is
// not promoted to the front: selection ranks by terms, display stays
// in transcript order, and the two must not be confused.
func TestPreviewKeepsTheLineWithMoreTermsWhenSlotsAreScarce(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	writeSession(t, dir, "a", nil,
		[2]string{"assistant", "rate alone one"},
		[2]string{"assistant", "rate alone two"},
		[2]string{"assistant", "rate alone three"},
		[2]string{"assistant", "rate alone four"},
		[2]string{"assistant", "the rate limiter together"},
	)

	got, err := Search(dir, mustQuery(t, "rate limiter"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || len(got[0].Lines) != linesPerSession {
		t.Fatalf("match = %+v", got)
	}
	kept := false
	for _, l := range got[0].Lines {
		if strings.Contains(l.Text, "limiter") {
			kept = true
		}
	}
	if !kept {
		t.Errorf("the line carrying both terms must survive the cut: %+v", got[0].Lines)
	}
	// Transcript order, not rank order.
	for i := 1; i < len(got[0].Lines); i++ {
		if got[0].Lines[i].Seq <= got[0].Lines[i-1].Seq {
			t.Errorf("preview out of transcript order: %+v", got[0].Lines)
		}
	}
}
