package orb

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

func TestInstallSkill(t *testing.T) {
	home := t.TempDir()
	p := filepath.Join(home, ".bough", "skills", "orb", "SKILL.md")
	if err := installSkill(home); err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(p); !bytes.Equal(b, skillMD) || !bytes.Contains(b, []byte("name: orb")) {
		t.Fatalf("installed skill = %q", b)
	}
	// A stale copy is replaced, so the skill never drifts from the binary.
	if err := os.WriteFile(p, []byte("stale"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := installSkill(home); err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(p); !bytes.Equal(b, skillMD) {
		t.Errorf("stale skill not replaced: %q", b)
	}
}
