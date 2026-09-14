package tools

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	iorb "github.com/andreylukin/bough/internal/orb"
)

// The wiki ingest runs as a local session with BOUGH_WRITE_ROOTS set to
// the wiki: write and patch work there and nowhere else, including
// through a symlink that leaves the root.
func TestLocalWriteRootsConfineWrite(t *testing.T) {
	wiki := t.TempDir()
	outside := t.TempDir()
	if err := os.Symlink(outside, filepath.Join(wiki, "escape")); err != nil {
		t.Fatal(err)
	}
	t.Setenv(iorb.WriteRootsEnv, wiki+string(os.PathListSeparator)+"relative/ignored")
	roots := iorb.LocalWriteRoots()
	if len(roots) != 1 || roots[0] != filepath.Clean(wiki) {
		t.Fatalf("roots = %v, want only the absolute wiki dir", roots)
	}
	s := &Stats{writeRoots: roots}
	if _, err := s.write(filepath.Join(wiki, "topics", "a.md"), "hello\n"); err != nil {
		t.Fatalf("write inside the wiki: %v", err)
	}
	if _, err := s.patch(filepath.Join(wiki, "topics", "a.md"), "hello", "bye"); err != nil {
		t.Fatalf("patch inside the wiki: %v", err)
	}
	for _, p := range []string{filepath.Join(outside, "x.md"), filepath.Join(wiki, "escape", "x.md"), filepath.Join(wiki, "..", "x.md")} {
		if _, err := s.write(p, "no"); err == nil || !strings.Contains(err.Error(), "outside this session's writable") {
			t.Errorf("write %s = %v, want refused", p, err)
		}
	}
	if !strings.Contains(iorb.LocalPromptSectionFor(roots), wiki) || iorb.LocalPromptSectionFor(nil) != iorb.LocalPromptSection {
		t.Error("prompt section does not name the write roots, or changed for a plain local session")
	}
}
