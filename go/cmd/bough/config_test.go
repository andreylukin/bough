package main

import (
	"os"
	"path/filepath"
	"testing"
)

// TestResolveConfig checks the source order: explicit --config verbatim,
// else ./bough.yml, else ~/.bough/bough.yml, else the embedded default.
// Cases build up from nothing so each added file flips the result.
func TestResolveConfig(t *testing.T) {
	home := t.TempDir()
	cwd := t.TempDir()
	t.Setenv("HOME", home)
	t.Chdir(cwd)

	if src := resolveConfig(true, "/no/such/bough.yml"); src.path != "/no/such/bough.yml" {
		t.Fatalf("explicit: got %q", src.path)
	}
	if src := resolveConfig(false, ""); src.path != "" {
		t.Fatalf("nothing on disk: want embedded, got %q", src.path)
	}

	global := filepath.Join(home, ".bough", "bough.yml")
	if err := os.MkdirAll(filepath.Dir(global), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(global, []byte("[]"), 0o644); err != nil {
		t.Fatal(err)
	}
	if src := resolveConfig(false, ""); src.path != global {
		t.Fatalf("global only: got %q, want %q", src.path, global)
	}

	if err := os.WriteFile(filepath.Join(cwd, "bough.yml"), []byte("[]"), 0o644); err != nil {
		t.Fatal(err)
	}
	if src := resolveConfig(false, ""); src.path != "bough.yml" {
		t.Fatalf("local + global: got %q, want bough.yml", src.path)
	}
	if src := resolveConfig(true, "/no/such/bough.yml"); src.path != "/no/such/bough.yml" {
		t.Fatalf("explicit beats local: got %q", src.path)
	}
}

// TestEmbeddedDefaultLoads: the embedded bough.yml parses into the base
// row set.
func TestEmbeddedDefaultLoads(t *testing.T) {
	rows, err := (configSource{}).load()
	if err != nil {
		t.Fatal(err)
	}
	ids := map[string]bool{}
	for _, r := range rows {
		ids[r.ID] = true
	}
	for _, want := range []string{"llm", "loop", "ui", "history"} {
		if !ids[want] {
			t.Fatalf("embedded config missing row %q; rows: %v", want, ids)
		}
	}
}
