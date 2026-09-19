package contextmd

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
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

// Paths is the one list both the injector and the control room read, so
// its order and its relative/absolute split are the contract.
func TestPaths(t *testing.T) {
	home := "/h"
	got := Paths(home, "")
	want := []string{
		"AGENTS.md",
		"CLAUDE.md",
		filepath.Join(home, ".claude", "CLAUDE.md"),
		filepath.Join(home, ".bough", "BOUGH.md"),
	}
	if !slices.Equal(got, want) {
		t.Errorf("Paths(no project) = %v, want %v", got, want)
	}
	got = Paths(home, "web")
	if len(got) != len(want)+1 {
		t.Fatalf("Paths(web) = %v", got)
	}
	if first := filepath.Join(home, ".bough", "projects", "web", "MEMORY.md"); got[0] != first {
		t.Errorf("Paths(web)[0] = %q, want %q", got[0], first)
	}
	if !slices.Equal(got[1:], want) {
		t.Errorf("Paths(web) tail = %v, want %v", got[1:], want)
	}
}

// AGENTS.md and CLAUDE.md stay relative: a project session chdirs into
// its worktree AFTER this row mounts, and the row re-reads them against
// the process cwd every turn. Absolutizing them at Apply time would
// pin every orb session to $HOME/AGENTS.md.
func TestPathsKeepsRepoFilesRelative(t *testing.T) {
	for _, slug := range []string{"", "web"} {
		for _, p := range Paths("/h", slug) {
			if base := filepath.Base(p); base == "AGENTS.md" && filepath.IsAbs(p) {
				t.Errorf("Paths(%q): %q is absolute", slug, p)
			}
		}
	}
}

// The project's brief is read first, so a repo file that repeats it is
// the copy that gets dropped, not the other way round.
func TestMemoryFirstWinsDedup(t *testing.T) {
	home := t.TempDir()
	dir := filepath.Join(home, ".bough", "projects", "web")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	mem := filepath.Join(dir, "MEMORY.md")
	if err := os.WriteFile(mem, []byte("## Deploy\n\nship on green.\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	agents := filepath.Join(home, "AGENTS.md")
	if err := os.WriteFile(agents, []byte("## Deploy\n\nship on green.\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	paths := Paths(home, "web")
	paths[1] = agents // the relative entry, resolved the way serve does
	parts := New(paths...).Parts()
	if len(parts) != 1 || parts[0].Path != mem {
		t.Fatalf("parts = %+v, want only MEMORY.md", parts)
	}
}

// A project session names its project by slug; a local session assigned
// to one carries only the directory, because the slug service is the
// one written into the immutable history meta entry.
func TestSessionSlug(t *testing.T) {
	for _, c := range []struct {
		name string
		give map[string]string
		want string
	}{
		{"no project", map[string]string{}, ""},
		{"project session", map[string]string{"session-mode": "project", "session-project": "web"}, "web"},
		{"local in a project", map[string]string{"session-project-dir": "/h/.bough/projects/web/"}, "web"},
		{"a project session ignores a stray dir", map[string]string{
			"session-mode": "project", "session-project": "web",
			"session-project-dir": "/h/.bough/projects/other",
		}, "web"},
	} {
		ctx := kernel.NewContext()
		for k, v := range c.give {
			ctx.Provide(k, v)
		}
		if got := sessionSlug(ctx); got != c.want {
			t.Errorf("%s: sessionSlug = %q, want %q", c.name, got, c.want)
		}
	}
}

// The row reads what Paths names, for the session's own project.
func TestApplyReadsTheProjectBrief(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	dir := filepath.Join(home, ".bough", "projects", "web")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	mem := filepath.Join(dir, "MEMORY.md")
	if err := os.WriteFile(mem, []byte("ship on green\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	ctx := kernel.NewContext()
	ctx.Provide("session-project-dir", dir)
	if err := (plugin{}).Apply(ctx, nil); err != nil {
		t.Fatal(err)
	}
	s, err := kernel.Get[*SystemContext](ctx, "context-md")
	if err != nil {
		t.Fatal(err)
	}
	if got := s.Preamble(); !strings.Contains(got, "# Context: "+mem) || !strings.Contains(got, "ship on green") {
		t.Errorf("Preamble = %q, want the project's MEMORY.md", got)
	}
}
