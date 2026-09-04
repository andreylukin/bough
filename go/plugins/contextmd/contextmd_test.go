package contextmd

import (
	"os"
	"path/filepath"
	"strings"
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

// CLAUDE.md is very often a copy of AGENTS.md. The same section must
// reach the model once: the first file to say it keeps it, later files
// lose it, and a file left with nothing disappears entirely.
func TestPreambleDedupesSharedSections(t *testing.T) {
	dir := t.TempDir()
	shared := "## Testing\n\nRun make test before pushing.\n"
	agents := filepath.Join(dir, "AGENTS.md")
	claude := filepath.Join(dir, "CLAUDE.md")
	if err := os.WriteFile(agents, []byte("# House\n\nBe terse.\n\n"+shared), 0o644); err != nil {
		t.Fatal(err)
	}
	// Same section, reformatted: trailing spaces and a blank line do
	// not make it a different rule.
	if err := os.WriteFile(claude, []byte("## Testing   \n\n\nRun make test before pushing.\n\n## Extra\n\nUse tabs.\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	got := New(agents, claude).Preamble()
	if n := strings.Count(got, "Run make test before pushing."); n != 1 {
		t.Fatalf("shared section appears %d times:\n%s", n, got)
	}
	for _, want := range []string{"Be terse.", "Use tabs.", agents, claude} {
		if !strings.Contains(got, want) {
			t.Fatalf("preamble lost %q:\n%s", want, got)
		}
	}

	parts := New(agents, claude).Parts()
	if len(parts) != 2 || parts[1].Dropped != 1 || parts[1].Same != agents {
		t.Fatalf("parts = %+v", parts)
	}

	// A file that is a pure duplicate contributes nothing at all.
	copyOf := filepath.Join(dir, "COPY.md")
	body, _ := os.ReadFile(agents)
	if err := os.WriteFile(copyOf, body, 0o644); err != nil {
		t.Fatal(err)
	}
	parts = New(agents, copyOf).Parts()
	if len(parts) != 1 || parts[0].Path != agents {
		t.Fatalf("a duplicate file should vanish, got %+v", parts)
	}
}

// Different files with different rules are all kept, in path order.
func TestPreambleKeepsDistinctSections(t *testing.T) {
	dir := t.TempDir()
	a := filepath.Join(dir, "AGENTS.md")
	b := filepath.Join(dir, "CLAUDE.md")
	os.WriteFile(a, []byte("## A\n\nfirst rule\n"), 0o644)
	os.WriteFile(b, []byte("## B\n\nsecond rule\n"), 0o644)
	got := New(a, b).Preamble()
	if !strings.Contains(got, "first rule") || !strings.Contains(got, "second rule") {
		t.Fatalf("distinct sections must both survive:\n%s", got)
	}
	if strings.Index(got, "first rule") > strings.Index(got, "second rule") {
		t.Fatalf("path order not kept:\n%s", got)
	}
}
