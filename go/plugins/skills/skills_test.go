package skills

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// addSkill creates <pool>/<name>/SKILL.md with body.
func addSkill(t *testing.T, pool, name, body string) {
	t.Helper()
	dir := filepath.Join(pool, name)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestInjectMatch(t *testing.T) {
	pool := t.TempDir()
	addSkill(t, pool, "restish", "restish skill body")
	s := New(pool)

	got := s.Inject("please use Restish here")
	if len(got) != 1 {
		t.Fatalf("got %d blocks, want 1", len(got))
	}
	if got[0] != "[skill: restish]\nrestish skill body" {
		t.Errorf("block = %q", got[0])
	}
}

func TestInjectNoMatch(t *testing.T) {
	pool := t.TempDir()
	addSkill(t, pool, "restish", "body")
	s := New(pool)

	if got := s.Inject("nothing relevant"); len(got) != 0 {
		t.Errorf("got %d blocks, want 0", len(got))
	}
}

func TestInjectWordBoundary(t *testing.T) {
	pool := t.TempDir()
	addSkill(t, pool, "restish", "body")
	s := New(pool)

	if got := s.Inject("restisher"); len(got) != 0 {
		t.Errorf("got %d blocks for substring, want 0", len(got))
	}
}

func TestInjectShadowing(t *testing.T) {
	global := t.TempDir()
	project := t.TempDir()
	addSkill(t, global, "wiki", "global body")
	addSkill(t, project, "wiki", "project body")
	s := New(global, project)

	got := s.Inject("open the wiki")
	if len(got) != 1 {
		t.Fatalf("got %d blocks, want 1", len(got))
	}
	if !strings.Contains(got[0], "project body") {
		t.Errorf("project pool should shadow global: %q", got[0])
	}
}

func TestInjectCap(t *testing.T) {
	pool := t.TempDir()
	for _, n := range []string{"alpha", "beta", "gamma", "delta"} {
		addSkill(t, pool, n, n+" body")
	}
	s := New(pool)

	got := s.Inject("alpha beta gamma delta")
	if len(got) != 3 {
		t.Errorf("got %d blocks, want cap of 3", len(got))
	}
}

func TestInjectMissingPools(t *testing.T) {
	s := New("/nonexistent/a", "/nonexistent/b")
	if got := s.Inject("anything"); len(got) != 0 {
		t.Errorf("got %d blocks from missing pools, want 0", len(got))
	}
}

func TestScanFollowsSymlinkedSkill(t *testing.T) {
	pool := t.TempDir()
	real := t.TempDir()
	addSkill(t, real, "linked", "linked body")
	if err := os.Symlink(filepath.Join(real, "linked"), filepath.Join(pool, "linked")); err != nil {
		t.Fatal(err)
	}
	s := New(pool)
	if got := s.Inject("use linked"); len(got) != 1 {
		t.Fatalf("symlinked skill: got %d blocks, want 1", len(got))
	}
}
