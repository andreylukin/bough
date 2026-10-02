package skills

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/andreylukin/bough/kernel"
)

func TestContextToolkitRequiresBothRows(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name     string
		loop     string
		disabled bool
		want     bool
	}{
		{"clm", "engine-clm", false, true},
		{"unreal", "engine-unreal", false, false},
		{"legacy", "loop", false, false},
		{"disabled toolkit", "engine-clm", true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			rows := []kernel.Row{{ID: "loop", Plugin: tc.loop}, {ID: "context-tools", Plugin: "context-tools", Disabled: tc.disabled}}
			if got := ContextToolkitEnabled(rows); got != tc.want {
				t.Fatalf("enabled = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestBuiltinContextSkillIsOnDemandAndDoesNotWriteHome(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	s := DefaultFor(home, home).WithContextToolkit(true)
	if got := s.Listing(); len(got) != 1 || got[0].Name != "context-toolkit" || got[0].Path != "builtin:context-toolkit" {
		t.Fatalf("built-in discovery = %+v", got)
	}
	if got := s.Catalog(); len(got) != 1 || got[0].Source != "builtin" || !got[0].Manual || got[0].Summary == "" {
		t.Fatalf("built-in catalogue = %+v", got)
	}
	if got := s.Inject("describe context-toolkit"); len(got) != 0 {
		t.Fatalf("builtin body injected on an ordinary mention: %v", got)
	}
	if got := s.Inject("/context-toolkit compact my notes"); len(got) != 1 || !strings.Contains(got[0], "expected_revision") || !strings.Contains(got[0], "context_offload") {
		t.Fatalf("builtin invocation = %v", got)
	}
	if files, err := os.ReadDir(home); err != nil || len(files) != 0 {
		t.Fatalf("discovery wrote HOME: %v %v", files, err)
	}
	if got := DefaultFor(home, home).Names(); len(got) != 0 {
		t.Fatalf("unconfigured builtin leaked into another engine: %v", got)
	}
}

func TestBuiltinSkillPreservesPoolOverrideAndOffSwitch(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	s := DefaultFor(home, home).WithContextToolkit(true)
	pool := filepath.Join(home, ".bough", "skills")
	addSkill(t, pool, "context-toolkit", "---\ndescription: My own workflow.\n---\nUSER-OVERRIDE")
	if got := s.Inject("/context-toolkit"); len(got) != 1 || !strings.Contains(got[0], "USER-OVERRIDE") || strings.Contains(got[0], "SnapshotID") {
		t.Fatalf("user override not used: %v", got)
	}
	if got := s.Catalog(); len(got) != 1 || got[0].Source != "pool" {
		t.Fatalf("override source = %+v", got)
	}
	writeOff(t, home, "skill:context-toolkit")
	if slices.Contains(s.Names(), "context-toolkit") || len(s.Listing()) != 0 || len(s.Inject("/context-toolkit")) != 0 {
		t.Fatal("off skill remained active")
	}
	if got := s.Catalog(); len(got) != 1 || !got[0].Off {
		t.Fatalf("off skill missing from settings: %+v", got)
	}
}

func TestBuiltinCatalogueTracksCurrentRows(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	s := DefaultFor(home, home)
	rows := []kernel.Row{{ID: "loop", Plugin: "engine-clm"}, {ID: "context-tools", Plugin: "context-tools"}}
	s.contextToolkit = func() bool { return ContextToolkitEnabled(rows) }
	if len(s.Listing()) != 1 {
		t.Fatal("CLM toolkit missing")
	}
	rows[0].Plugin = "engine-unreal"
	if len(s.Listing()) != 0 || len(s.Inject("/context-toolkit")) != 0 {
		t.Fatal("builtin remained enabled after engine change")
	}
	rows[0].Plugin = "engine-clm"
	if len(s.Listing()) != 1 {
		t.Fatal("builtin did not return after engine change")
	}
	rows[1].Disabled = true
	if len(s.Listing()) != 0 {
		t.Fatal("builtin remained enabled after toolkit disable")
	}
}
