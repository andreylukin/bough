package history

// RecentPrompts: what the composer's Up arrow reaches across sessions.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// session writes one session file with a cwd and a list of entries,
// then stamps its mtime so ordering is deterministic.
func session(t *testing.T, dir, name, cwd string, age time.Duration, lines ...string) {
	t.Helper()
	body := jsonLine(t, map[string]any{"seq": 1, "kind": "meta", "data": map[string]any{"cwd": cwd}}) + "\n"
	for _, l := range lines {
		body += l + "\n"
	}
	p := filepath.Join(dir, name+".jsonl")
	if err := os.WriteFile(p, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	when := time.Now().Add(-age)
	if err := os.Chtimes(p, when, when); err != nil {
		t.Fatal(err)
	}
}

func input(text string) string {
	return `{"seq":2,"kind":"input","data":{"text":"` + text + `"}}`
}

func TestRecentPromptsNewestFirstAcrossSessions(t *testing.T) {
	dir := t.TempDir()
	session(t, dir, "old", "/w", 2*time.Hour, input("first"), input("second"))
	session(t, dir, "new", "/w", time.Minute, input("third"))
	got := RecentPrompts(dir, "/w", 10)
	want := []string{"third", "second", "first"}
	if len(got) != len(want) {
		t.Fatalf("got %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("got %v, want %v", got, want)
		}
	}
}

// Sessions from another directory are noise in this one's composer.
func TestRecentPromptsSkipsOtherDirectories(t *testing.T) {
	dir := t.TempDir()
	session(t, dir, "here", "/w", time.Minute, input("mine"))
	session(t, dir, "there", "/other", time.Minute, input("theirs"))
	got := RecentPrompts(dir, "/w", 10)
	if len(got) != 1 || got[0] != "mine" {
		t.Fatalf("got %v, want just [mine]", got)
	}
}

// The message sent to the model carries @file expansions and injected
// skills; the composer must recall the line that was typed.
func TestRecentPromptsPrefersTypedLine(t *testing.T) {
	dir := t.TempDir()
	session(t, dir, "s", "/w", time.Minute,
		`{"seq":2,"kind":"input","data":{"text":"look at @a.go\n\n<file>...500 lines...</file>","typed":"look at @a.go"}}`)
	got := RecentPrompts(dir, "/w", 10)
	if len(got) != 1 || got[0] != "look at @a.go" {
		t.Fatalf("got %v, want the typed line", got)
	}
}

// A background job waking an idle agent is written as an input, but
// nobody typed it.
func TestRecentPromptsSkipsBackgroundWakeups(t *testing.T) {
	dir := t.TempDir()
	session(t, dir, "s", "/w", time.Minute,
		input("[background job] A command you started in the background has finished"),
		input("real prompt"))
	got := RecentPrompts(dir, "/w", 10)
	if len(got) != 1 || got[0] != "real prompt" {
		t.Fatalf("got %v, want just the typed prompt", got)
	}
}

func TestRecentPromptsStopsAtLimit(t *testing.T) {
	dir := t.TempDir()
	session(t, dir, "s", "/w", time.Minute, input("a"), input("b"), input("c"))
	if got := RecentPrompts(dir, "/w", 2); len(got) != 2 {
		t.Fatalf("limit ignored: %v", got)
	}
}

func TestRecentPromptsSqueezesRepeats(t *testing.T) {
	dir := t.TempDir()
	session(t, dir, "s", "/w", time.Minute, input("same"), input("same"), input("other"))
	got := RecentPrompts(dir, "/w", 10)
	if len(got) != 2 {
		t.Fatalf("consecutive duplicates should squeeze, got %v", got)
	}
}

func TestRecentPromptsEmptyDir(t *testing.T) {
	if got := RecentPrompts(t.TempDir(), "/w", 10); got != nil {
		t.Fatalf("empty dir should yield nothing, got %v", got)
	}
}

// jsonLine encodes one history line. Building JSON by concatenation
// breaks on Windows, where a cwd is C:\\Users\\… and the backslashes
// are read as escapes.
func jsonLine(t *testing.T, v any) string {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}
