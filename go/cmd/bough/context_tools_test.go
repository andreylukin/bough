package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestContextToolkitConfigDiscovery(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, config string
		want         bool
	}{
		{"default", "[]\n", false},
		{"unreal", "- id: loop\n  plugin: engine-unreal\n", false},
		{"clm", "- id: loop\n  plugin: engine-clm\n", true},
		{"disabled", "- id: loop\n  plugin: engine-clm\n- id: context-tools\n  disabled: true\n", false},
		{"invalid", "not: [yaml", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "bough.yml"), []byte(tc.config), 0600); err != nil {
				t.Fatal(err)
			}
			if got := contextToolsIn(dir); got != tc.want {
				t.Fatalf("discovery = %v, want %v", got, tc.want)
			}
		})
	}
}
