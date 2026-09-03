package contextmd

import (
	"os"
	"path/filepath"
	"testing"
)

func TestPreamble(t *testing.T) {
	dir := t.TempDir()
	a := filepath.Join(dir, "AGENTS.md")
	c := filepath.Join(dir, "CLAUDE.md")
	missing := filepath.Join(dir, "BOUGH.md")
	if err := os.WriteFile(a, []byte("agents stuff"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(c, []byte("claude stuff"), 0o644); err != nil {
		t.Fatal(err)
	}

	s := New(a, missing, c)
	want := "# Context: " + a + "\nagents stuff\n" +
		"# Context: " + c + "\nclaude stuff\n"
	if got := s.Preamble(); got != want {
		t.Errorf("Preamble = %q, want %q", got, want)
	}
}

func TestPreambleAllMissing(t *testing.T) {
	s := New(filepath.Join(t.TempDir(), "nope.md"))
	if got := s.Preamble(); got != "" {
		t.Errorf("Preamble = %q, want empty", got)
	}
}

func TestPreambleFresh(t *testing.T) {
	p := filepath.Join(t.TempDir(), "AGENTS.md")
	s := New(p)
	if s.Preamble() != "" {
		t.Fatal("want empty before file exists")
	}
	if err := os.WriteFile(p, []byte("now"), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := s.Preamble(); got != "# Context: "+p+"\nnow\n" {
		t.Errorf("Preamble = %q", got)
	}
}

func TestLoaded(t *testing.T) {
	dir := t.TempDir()
	a := filepath.Join(dir, "AGENTS.md")
	missing := filepath.Join(dir, "BOUGH.md")
	c := filepath.Join(dir, "CLAUDE.md")
	for _, p := range []string{a, c} {
		if err := os.WriteFile(p, []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	s := New(a, missing, c)
	got := s.Loaded()
	if len(got) != 2 || got[0] != a || got[1] != c {
		t.Errorf("Loaded = %v, want [%s %s]", got, a, c)
	}
	if got := New(missing).Loaded(); len(got) != 0 {
		t.Errorf("Loaded with nothing on disk = %v, want none", got)
	}
}
