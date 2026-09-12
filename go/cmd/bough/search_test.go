package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/plugins/history"
)

// writeSearchSession stores a session whose entries carry the given
// kind/text pairs, so a test can put words somewhere other than the
// title (which is all `bough sessions` can match on).
func writeSearchSession(t *testing.T, id string, mtime time.Time, meta map[string]any, kv ...[2]string) {
	t.Helper()
	dir := sessionsDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	var lines string
	seq := 1
	if meta != nil {
		lines += mustJSON(t, map[string]any{"seq": seq, "kind": "meta", "data": meta}) + "\n"
		seq++
	}
	for _, e := range kv {
		lines += mustJSON(t, map[string]any{"seq": seq, "kind": e[0], "data": map[string]any{"text": e[1]}}) + "\n"
		seq++
	}
	p := filepath.Join(dir, id+".jsonl")
	if err := os.WriteFile(p, []byte(lines), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(p, mtime, mtime); err != nil {
		t.Fatal(err)
	}
}

// The point of the command: a word said in the middle of a session
// finds it, and the matching line is shown as the reason.
func TestSearchFindsSessionsByTranscriptText(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	now := time.Now()
	writeSearchSession(t, "hit", now, map[string]any{"cwd": "/w"},
		[2]string{"input", "look at the deploy"},
		[2]string{"assistant", "the rate limiter drops the third retry"})
	writeSearchSession(t, "miss", now, map[string]any{"cwd": "/w"},
		[2]string{"input", "something else entirely"})

	matches, err := history.Search(sessionsDir(), mustParse(t, "rate limiter"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) != 1 || matches[0].ID != "hit" {
		t.Fatalf("matches = %+v, want only hit", matches)
	}

	var b bytes.Buffer
	printMatches(&b, matches, now)
	out := b.String()
	if !strings.Contains(out, "hit") || !strings.Contains(out, "rate limiter drops") {
		t.Errorf("output must name the session and why it matched:\n%s", out)
	}
	if !strings.Contains(out, "today") {
		t.Errorf("results are grouped by recency:\n%s", out)
	}
}

func TestSearchOutputGroupsSessionsByAge(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	now := time.Now()
	writeSearchSession(t, "fresh", now, nil, [2]string{"input", "shared"})
	writeSearchSession(t, "old", now.AddDate(0, 0, -40), nil, [2]string{"input", "shared"})

	matches, err := history.Search(sessionsDir(), mustParse(t, "shared"), 0)
	if err != nil {
		t.Fatal(err)
	}
	var b bytes.Buffer
	printMatches(&b, matches, now)
	out := b.String()
	if !strings.Contains(out, "today") || !strings.Contains(out, "older") {
		t.Errorf("want both a today and an older heading:\n%s", out)
	}
	if strings.Index(out, "today") > strings.Index(out, "older") {
		t.Errorf("newest group first:\n%s", out)
	}
}

// A session matching many times is one result, and says what was left
// out rather than looking arbitrarily trimmed.
func TestSearchPreviewCapsLinesAndSaysHowManyMore(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	now := time.Now()
	var kv [][2]string
	for range 6 {
		kv = append(kv, [2]string{"assistant", "postgres again"})
	}
	writeSearchSession(t, "many", now, nil, kv...)

	matches, err := history.Search(sessionsDir(), mustParse(t, "postgres"), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) != 1 || matches[0].Hits != 6 || len(matches[0].Lines) != 3 {
		t.Fatalf("match = %+v, want 6 hits previewed by 3", matches[0])
	}
	var b bytes.Buffer
	printMatches(&b, matches, now)
	if !strings.Contains(b.String(), "3 more in this session") {
		t.Errorf("missing the elision note:\n%s", b.String())
	}
}

func TestDateGroupBuckets(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 11, 12, 0, 0, 0, time.Local)
	for _, c := range []struct {
		age  time.Duration
		want string
	}{
		{0, "today"},
		{26 * time.Hour, "yesterday"},
		{5 * 24 * time.Hour, "last 7 days"},
		{20 * 24 * time.Hour, "this month"},
		{90 * 24 * time.Hour, "older"},
	} {
		if got := dateGroup(now.Add(-c.age), now); got != c.want {
			t.Errorf("%v ago = %q, want %q", c.age, got, c.want)
		}
	}
}

// Sessions predating repo capture still say where they ran.
func TestPlaceFallsBackToCwdThenUnknown(t *testing.T) {
	t.Parallel()
	home := "/Users/me"
	withRepo := history.Match{SessionInfo: history.SessionInfo{Repo: "/Users/me/repos/bough", Branch: "feat-x"}}
	if got := place(withRepo, home); got != "~/repos/bough feat-x" {
		t.Errorf("place = %q", got)
	}
	withCwd := history.Match{SessionInfo: history.SessionInfo{Cwd: "/tmp/x"}}
	if got := place(withCwd, home); got != "/tmp/x" {
		t.Errorf("place = %q", got)
	}
	if got := place(history.Match{}, home); got != "?" {
		t.Errorf("place = %q, want ?", got)
	}
}

func mustParse(t *testing.T, s string) history.Query {
	t.Helper()
	q, err := history.ParseQuery(s)
	if err != nil {
		t.Fatal(err)
	}
	return q
}
