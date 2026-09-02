package ui

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestAtStart(t *testing.T) {
	for in, want := range map[string]int{"@": 0, "@ma": 0, "see @ma": 4, "a@b.com": -1, "x": -1, "": -1, "@a done": -1} {
		if got := atStart(in); got != want {
			t.Errorf("atStart(%q) = %d, want %d", in, got, want)
		}
	}
}

// Typing "@" opens a picker over the project's files, the query
// fuzzy-filters it, Tab completes the word in place, and Esc closes it
// until the draft changes. The "/" palette is untouched.
func TestAtPickerCompletesFile(t *testing.T) {
	dir := t.TempDir()
	os.WriteFile(filepath.Join(dir, "main.go"), []byte("x"), 0o644)
	os.MkdirAll(filepath.Join(dir, ".git"), 0o755)
	os.WriteFile(filepath.Join(dir, ".git", "HEAD"), []byte("x"), 0o644)
	os.MkdirAll(filepath.Join(dir, "pkg"), 0o755)
	os.WriteFile(filepath.Join(dir, "pkg", "util.go"), []byte("x"), 0o644)
	wd, _ := os.Getwd()
	os.Chdir(dir)
	t.Cleanup(func() { os.Chdir(wd) })

	d := defaultDrv(t)
	d.typeStr("read @")
	if !d.m.at.open || d.m.pal.open {
		t.Fatal("a word-initial @ should open the file picker, not the palette")
	}
	p := d.plain()
	if !strings.Contains(p, "main.go") || !strings.Contains(p, "pkg/util.go") || strings.Contains(p, ".git") {
		t.Fatalf("picker should list project files without dot dirs:\n%s", p)
	}
	d.typeStr("ut")
	if p := d.plain(); strings.Contains(p, "main.go") || !strings.Contains(p, "pkg/util.go") {
		t.Fatalf("query should filter:\n%s", p)
	}
	d.press(keyTab())
	if got := d.m.input.Value(); got != "read @pkg/util.go " {
		t.Fatalf("tab should complete in place, got %q", got)
	}
	if d.m.at.open {
		t.Error("picker should close once the word is complete")
	}
	d.typeStr("@")
	if !d.m.at.open {
		t.Fatal("a new @ word should reopen")
	}
	d.press(keyEsc())
	if d.m.at.open {
		t.Error("esc should close the picker")
	}
}
