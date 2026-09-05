package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"encoding/json"
	"github.com/andreylukin/bough/plugins/history"
)

// writeSession stores one session JSONL under $HOME/.bough/history with
// the given cwd meta entry ("" = old file without one) and mtime.
func writeSession(t *testing.T, id, cwd string, mtime time.Time) string {
	t.Helper()
	dir := sessionsDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	p := filepath.Join(dir, id+".jsonl")
	var lines string
	// Marshalled, not concatenated: on Windows a cwd is C:\Users\…, and
	// pasting that between quotes produces `\U`, which is not JSON. The
	// file was then skipped as corrupt and the test failed for a reason
	// that had nothing to do with what it was testing.
	if cwd != "" {
		lines += mustJSON(t, map[string]any{"seq": 1, "kind": "meta", "data": map[string]any{"cwd": cwd}}) + "\n"
	}
	lines += mustJSON(t, map[string]any{"seq": 2, "kind": "input", "data": map[string]any{"text": "prompt in " + id}}) + "\n"
	if err := os.WriteFile(p, []byte(lines), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(p, mtime, mtime); err != nil {
		t.Fatal(err)
	}
	return p
}

// -c resumes the newest session recorded in THIS directory, not the
// globally newest one from another project.
func TestContinuePrefersThisDirectory(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	here := cwd()
	now := time.Now()
	writeSession(t, "old-here", here, now.Add(-2*time.Hour))
	elsewhere := writeSession(t, "new-elsewhere", "/some/other/project", now)
	want := writeSession(t, "new-here", here, now.Add(-time.Hour))

	got, picker := resolveSession(true, false, "", "tui")
	if picker || got != want {
		t.Fatalf("-c = %q (picker %v), want %q", got, picker, want)
	}

	// Only foreign sessions: fall back to the newest anywhere.
	os.Remove(want)
	os.Remove(filepath.Join(sessionsDir(), "old-here.jsonl"))
	if got, _ := resolveSession(true, false, "", "tui"); got != elsewhere {
		t.Fatalf("-c with no local session = %q, want fallback %q", got, elsewhere)
	}
}

func TestLatestSessionInPrefersCwd(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	now := time.Now()
	writeSession(t, "elsewhere", "/other", now)
	want := writeSession(t, "here", "/here", now.Add(-time.Hour))
	if got, err := latestSessionIn(sessionsDir(), "/here"); err != nil || got != want {
		t.Fatalf("latest for /here = %q, %v; want %q", got, err, want)
	}
	if _, err := latestSessionIn(filepath.Join(t.TempDir(), "empty"), "/here"); err == nil ||
		!strings.Contains(err.Error(), "no sessions") {
		t.Fatalf("empty dir: err = %v, want 'no sessions'", err)
	}
}

func TestPrintSessionsCwdColumn(t *testing.T) {
	infos := []history.SessionInfo{
		{ID: "a", Title: "t", Cwd: "/proj"},
		{ID: "b", Title: "t"},
	}
	var buf bytes.Buffer
	printSessions(&buf, infos, true)
	out := buf.String()
	if !strings.Contains(out, "/proj") || !strings.Contains(out, "?") {
		t.Fatalf("cwd column missing:\n%s", out)
	}
	buf.Reset()
	printSessions(&buf, infos, false)
	if strings.Contains(buf.String(), "/proj") {
		t.Fatalf("cwd column shown without --all:\n%s", buf.String())
	}
}

// mustJSON encodes one history line.
func mustJSON(t *testing.T, v any) string {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}
