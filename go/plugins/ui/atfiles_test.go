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

// An "@" word that names a path lists that directory: the project walk
// only knows this directory's files, so "@~/…" used to leave the
// picker blank even though tab could complete it.
func TestAtPickerCompletesPaths(t *testing.T) {
	dir := t.TempDir()
	for _, name := range []string{"alpha.md", "beta.md"} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.Mkdir(filepath.Join(dir, "nested"), 0o755); err != nil {
		t.Fatal(err)
	}

	m := testModel(t)
	m.setDraft("look at @" + dir + "/")
	m.syncAt()
	if !m.at.open {
		t.Fatal("the picker is closed on a path query")
	}
	items := m.atItems()
	var names []string
	for _, it := range items {
		names = append(names, it.name)
	}
	joined := strings.Join(names, " ")
	for _, want := range []string{"alpha.md", "beta.md", "nested/"} {
		if !strings.Contains(joined, want) {
			t.Fatalf("path listing missing %q: %v", want, names)
		}
	}

	// Tab takes the selected entry, in place.
	m.setDraft("look at @" + filepath.Join(dir, "al"))
	m.syncAt()
	if handled, _ := m.atKey("tab"); !handled {
		t.Fatal("tab was not handled by the picker")
	}
	if got := m.input.Value(); got != "look at @"+filepath.Join(dir, "alpha.md")+" " {
		t.Fatalf("draft = %q", got)
	}

	// A directory does not end the reference: no trailing space, and
	// the picker stays open on its contents.
	m.setDraft("look at @" + filepath.Join(dir, "nes"))
	m.syncAt()
	m.atKey("tab")
	if got := m.input.Value(); got != "look at @"+filepath.Join(dir, "nested")+"/" {
		t.Fatalf("directory completion = %q", got)
	}
	if !m.at.open {
		t.Fatal("the picker closed on a directory")
	}
}

// pathQuery only claims the words the filesystem should answer for; a
// plain fuzzy search still goes to the project walk.
func TestPathQuery(t *testing.T) {
	for _, q := range []string{"~", "~/re", "/etc/ho", "./plug", "../go"} {
		if !pathQuery(q) {
			t.Fatalf("%q should list a directory", q)
		}
	}
	for _, q := range []string{"", "model.go", "plugins/ui/model.go", "ui model"} {
		if pathQuery(q) {
			t.Fatalf("%q should stay a project search", q)
		}
	}
}
