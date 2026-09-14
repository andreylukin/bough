package skills

import (
	"os"
	"path/filepath"
	"testing"
)

func TestManualSkillNotInvokedByAPathSegment(t *testing.T) {
	pool := t.TempDir()
	dir := filepath.Join(pool, "orb")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "SKILL.md"), []byte("---\nname: orb\nmanual: true\n---\nbody\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	s := New(pool)
	for _, in := range []string{
		`job 8 [failed] # bash "$BOUGH_SCRATCH/orb-setup.sh" > "$BOUGH_SCRATCH/orb-setup.log" 2>&1`,
		"see ~/.bough/skills/orb/SKILL.md",
		"set up the orb for this repo",
	} {
		if got := s.Inject(in); len(got) != 0 {
			t.Errorf("Inject(%q) injected %d blocks, want none", in, len(got))
		}
	}
	for _, in := range []string{"/orb example-app", "please run /orb now", "/ORB"} {
		if got := s.Inject(in); len(got) != 1 {
			t.Errorf("Inject(%q) injected %d blocks, want 1", in, len(got))
		}
	}
}
