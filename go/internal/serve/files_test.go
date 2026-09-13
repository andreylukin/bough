package serve

import (
	"net/http"
	"os"
	"path/filepath"
	"testing"
)

func touchFile(t *testing.T, path string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
}

// The @ picker exists so you can stop typing a path early.
func TestFilesMatchScatteredAndRankName(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	touchFile(t, filepath.Join(base, "app", "palette.tsx"))
	touchFile(t, filepath.Join(base, "deep", "nested", "other.txt"))
	touchFile(t, filepath.Join(base, "palette.md"))

	got := findFiles(base, "palette")
	if len(got) < 2 {
		t.Fatalf("palette matched %d paths, want both (%v)", len(got), got)
	}
	// A match on the file's own name outranks one buried in directories,
	// and the shorter path wins the tie.
	if got[0].Path != "palette.md" {
		t.Errorf("first = %q, want palette.md", got[0].Path)
	}

	// Scattered, in order.
	if s := findFiles(base, "aptsx"); len(s) == 0 || s[0].Path != "app/palette.tsx" {
		t.Errorf("aptsx = %v, want app/palette.tsx", s)
	}
}

// A home directory holds a thousand repos; the walk must never descend
// into the directories that make it unbearable.
func TestFilesSkipsMachineDirectories(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	touchFile(t, filepath.Join(base, "node_modules", "pkg", "target.js"))
	touchFile(t, filepath.Join(base, ".git", "target.js"))
	touchFile(t, filepath.Join(base, "src", "target.js"))

	got := findFiles(base, "target")
	if len(got) != 1 || got[0].Path != "src/target.js" {
		t.Errorf("got %v, want only src/target.js", got)
	}
}

// A query is required: listing a home directory does not answer
// "which file", and the walk is not free.
func TestFilesNeedsAQuery(t *testing.T) {
	t.Parallel()
	f := newAPI(t)
	f.api.home = f.home
	code, body := f.do(t, "GET", "/api/files?q=", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/files = %d", code)
	}
	list, ok := body["files"].([]any)
	if !ok || len(list) != 0 {
		t.Errorf("files = %#v, want an empty list", body["files"])
	}
}
