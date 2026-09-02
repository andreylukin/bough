package loop

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// "@path" words that name real files become attachments, once each,
// in mention order; handles that are not files, directories, and
// parent-escaping paths are ignored; big files are cut with a note.
func TestExpandAt(t *testing.T) {
	root := t.TempDir()
	os.WriteFile(filepath.Join(root, "a.go"), []byte("package a\n"), 0o644)
	os.MkdirAll(filepath.Join(root, "sub"), 0o755)
	os.WriteFile(filepath.Join(root, "sub", "b.txt"), []byte("bee"), 0o644)
	os.WriteFile(filepath.Join(root, "big.log"), []byte(strings.Repeat("x", atMaxBytes+10)), 0o644)
	blocks := ExpandAt("look at @sub/b.txt, then @a.go and @a.go again; ping @someone @sub @../etc/passwd @big.log", root)
	if len(blocks) != 3 {
		t.Fatalf("blocks = %d: %q", len(blocks), blocks)
	}
	if blocks[0] != "[file: sub/b.txt]\nbee" || blocks[1] != "[file: a.go]\npackage a\n" {
		t.Fatalf("blocks = %q", blocks[:2])
	}
	if !strings.HasPrefix(blocks[2], "[file: big.log]\n") || !strings.Contains(blocks[2], "[truncated at") || len(blocks[2]) > atMaxBytes+200 {
		t.Fatalf("big file not capped: len=%d", len(blocks[2]))
	}
	if got := ExpandAt("mail a@b.com", root); len(got) != 0 {
		t.Fatalf("an @ inside a word is not a reference: %q", got)
	}
}
