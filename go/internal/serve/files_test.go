package serve

import (
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
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

// A bare "@" lists the top of the directory — one level down, never the
// whole tree — so the picker opens with something in it.
func TestFilesBareQueryListsTheTop(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	touchFile(t, filepath.Join(base, "readme.md"))
	touchFile(t, filepath.Join(base, "src", "main.go"))
	touchFile(t, filepath.Join(base, "src", "deep", "buried.go"))

	got := map[string]bool{}
	for _, h := range findFiles(base, "") {
		got[h.Path] = true
	}
	for _, want := range []string{"readme.md", "src", "src/main.go"} {
		if !got[want] {
			t.Errorf("bare query missing %q; got %v", want, got)
		}
	}
	if got["src/deep/buried.go"] {
		t.Errorf("bare query descended past one level: %v", got)
	}

	f := newAPI(t)
	f.api.home = f.home
	code, body := f.do(t, "GET", "/api/files?q=", "")
	if code != http.StatusOK {
		t.Fatalf("GET /api/files = %d", code)
	}
	if _, ok := body["files"].([]any); !ok {
		t.Errorf("files = %#v, want a list", body["files"])
	}
}

// In a repository, what git ignores is not what @ is reaching for: an
// ignored export used to outrank the real source.
func TestFilesRespectGitignore(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not installed")
	}
	base := t.TempDir()
	if out, err := exec.Command("git", "-C", base, "init", "-q").CombinedOutput(); err != nil {
		t.Fatalf("git init: %v %s", err, out)
	}
	if err := os.WriteFile(filepath.Join(base, ".gitignore"), []byte("export/\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	touchFile(t, filepath.Join(base, "export", "App.js"))
	touchFile(t, filepath.Join(base, "web", "src", "app.tsx"))

	got := findFiles(base, "app")
	if len(got) == 0 || got[0].Path != "web/src/app.tsx" {
		t.Fatalf("got %v, want web/src/app.tsx first", got)
	}
	for _, h := range got {
		if strings.HasPrefix(h.Path, "export") {
			t.Errorf("ignored path %q offered", h.Path)
		}
	}
}
