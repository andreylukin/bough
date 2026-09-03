package ui

// Tab path completion (pathcomplete.go): common prefix, cycling,
// directory slash, "~", "@" words, and Tab left to block focus off a
// path-like word.

import (
	"os"
	"path/filepath"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func keyLeft() tea.KeyPressMsg { return tea.KeyPressMsg{Code: tea.KeyLeft} }

// chdirTree builds a small tree in a temp dir and chdirs into it.
func chdirTree(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	for _, f := range []string{"main.go", "main_test.go", "pkg/util.go", "pkg/x.go", "docs/a.md", ".hidden", "my dir/f.txt"} {
		os.MkdirAll(filepath.Dir(filepath.Join(dir, f)), 0o755)
		os.WriteFile(filepath.Join(dir, f), []byte("x"), 0o644)
	}
	wd, _ := os.Getwd()
	os.Chdir(dir)
	t.Cleanup(func() { os.Chdir(wd) })
	return dir
}

func TestTabCompletesPaths(t *testing.T) {
	chdirTree(t)
	cases := []struct {
		name  string
		typed string
		tabs  []string // the draft after each successive Tab
	}{
		{"common prefix then cycle", "see ma", []string{"see main", "see main.go", "see main_test.go", "see main.go"}},
		{"dir gets a slash, next tab descends", "pk", []string{"pkg/", "pkg/util.go", "pkg/x.go"}},
		{"slash word in a subdir", "read ./docs/a", []string{"read ./docs/a.md"}},
		{"a final @ word is the picker's (trailing space)", "look at @pkg/u", []string{"look at @pkg/util.go "}},
		{"dotfile only when typed", ".hid", []string{".hidden"}},
		{"a blank in the name is escaped, next tab descends", "my", []string{"my\\ dir/", "my\\ dir/f.txt"}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			d := defaultDrv(t)
			d.typeStr(c.typed)
			for i, want := range c.tabs {
				d.press(keyTab())
				if got := d.m.input.Value(); got != want {
					t.Fatalf("tab %d: draft = %q, want %q", i+1, got, want)
				}
			}
		})
	}
}

func TestTabCompletesTilde(t *testing.T) {
	chdirTree(t)
	home := t.TempDir()
	t.Setenv("HOME", home)
	os.MkdirAll(filepath.Join(home, "repos"), 0o755)
	d := defaultDrv(t)
	d.typeStr("~")
	d.press(keyTab())
	d.typeStr("re")
	d.press(keyTab())
	if got := d.m.input.Value(); got != "~/repos/" {
		t.Fatalf("draft = %q, want ~/repos/", got)
	}
}

func TestTabCompletesMidDraft(t *testing.T) {
	chdirTree(t)
	d := defaultDrv(t)
	d.typeStr("open @pkg/u please")
	for range len(" please") {
		d.feed(keyLeft())
	}
	d.press(keyTab())
	if got := d.m.input.Value(); got != "open @pkg/util.go please" {
		t.Fatalf("draft = %q", got)
	}
	if d.m.input.Column() != len("open @pkg/util.go") {
		t.Errorf("cursor should sit after the completion, at col %d", d.m.input.Column())
	}
}

// Off a path-like word Tab is the keymap's block_next: the composer
// is untouched and the newest block gets the focus.
func TestTabOffPathFocusesBlock(t *testing.T) {
	chdirTree(t)
	for _, typed := range []string{"", "hello", "fix the bug", "see https://x.y/z"} {
		d := defaultDrv(t)
		d.event("result", nLines(20))
		d.typeStr(typed)
		d.press(keyTab())
		if got := d.m.input.Value(); got != typed {
			t.Errorf("%q: draft changed to %q", typed, got)
		}
		if d.m.focusID != d.m.blocks[0].id {
			t.Errorf("%q: tab should focus the block", typed)
		}
	}
	// A path-like word with no match is consumed (no focus jump),
	// like a shell's beep.
	d := defaultDrv(t)
	d.event("result", nLines(20))
	d.typeStr("see nope/zz")
	d.press(keyTab())
	if d.m.focusID != -1 || d.m.input.Value() != "see nope/zz" {
		t.Errorf("unmatched path: draft %q focus %d", d.m.input.Value(), d.m.focusID)
	}
}

// Moving the cursor ends a cycle: the next Tab completes the word
// there afresh instead of backspacing the old candidate's length.
func TestTabCycleEndsOnCursorMove(t *testing.T) {
	chdirTree(t)
	d := defaultDrv(t)
	d.typeStr("see ma")
	d.press(keyTab())
	d.press(keyTab())
	if got := d.m.input.Value(); got != "see main.go" {
		t.Fatalf("draft = %q", got)
	}
	for range 3 {
		d.feed(keyLeft())
	}
	d.press(keyTab())
	if got := d.m.input.Value(); got != "see main.go.go" {
		t.Fatalf("after a cursor move tab should complete %q afresh, got %q", "main", got)
	}
}

func TestPathCandidatesPure(t *testing.T) {
	chdirTree(t)
	got := pathCandidates("")
	want := []string{"docs/", "main.go", "main_test.go", "my dir/", "pkg/"}
	if len(got) != len(want) {
		t.Fatalf("cwd listing = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("cwd listing = %v, want %v", got, want)
		}
	}
	if p := commonPrefix([]string{"main.go", "main_test.go"}); p != "main" {
		t.Errorf("commonPrefix = %q", p)
	}
}
