package tools

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/andreylukin/bough"
)

func TestViewBuiltinContextSkillUsesNormalRanges(t *testing.T) {
	t.Parallel()
	const path = "builtin:context-toolkit"
	body, ok := bough.BuiltinSkill(path)
	if !ok {
		t.Fatal("embedded context skill is missing")
	}
	p := filepath.Join(t.TempDir(), "SKILL.md")
	if err := os.WriteFile(p, []byte(body), 0600); err != nil {
		t.Fatal(err)
	}
	// Embedded bytes need no project filesystem, orb or HOME access.
	st := &Stats{project: &projectMode{orb: func() (orbExec, error) {
		t.Error("built-in skill tried to access a project filesystem")
		return nil, errors.New("no filesystem")
	}}}
	for _, rng := range [][]int{nil, {2, 4}, {5}, {99999}, {4, 2}} {
		got, ge := st.viewFile(path, rng...)
		want, we := readView(p, rng...)
		if (ge == nil) != (we == nil) || ge == nil && got != want {
			t.Fatalf("range %v: virtual %q/%v, disk %q/%v", rng, got, ge, want, we)
		}
	}
	if _, err := readView("builtin:unknown-context-skill"); err == nil || !strings.Contains(err.Error(), "unknown-context-skill") {
		t.Fatalf("unknown virtual path did not fail normally: %v", err)
	}
}
