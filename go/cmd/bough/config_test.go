package main

import (
	"github.com/andreylukin/bough/kernel"
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

// TestOverlay: a file on disk is merged onto the embedded default by
// id, so rows added to the default reach users who saved a bough.yml.
func TestOverlay(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "bough.yml")
	file := "- id: llm\n  plugin: llm-openrouter\n  config: {model: x/y}\n" +
		"- id: llm-small\n  plugin: llm-openrouter\n" +
		"- id: history\n  disabled: true\n  plugin: history\n"
	if err := os.WriteFile(path, []byte(file), 0o644); err != nil {
		t.Fatal(err)
	}
	rows, err := (configSource{path: path}).load()
	if err != nil {
		t.Fatal(err)
	}
	base, _ := (configSource{}).load()
	if len(rows) != len(base)+1 {
		t.Fatalf("want base+1 rows, got %d vs %d", len(rows), len(base))
	}
	byID := map[string]kernel.Row{}
	for _, r := range rows {
		byID[r.ID] = r
	}
	if r := byID["llm"]; r.Plugin != "llm-openrouter" || r.Config["model"] != "x/y" {
		t.Fatalf("llm row not replaced: %+v", r)
	}
	if !byID["history"].Disabled {
		t.Fatal("disabled: true did not carry over")
	}
	if rows[len(rows)-1].ID != "llm-small" {
		t.Fatalf("new row not appended last: %s", rows[len(rows)-1].ID)
	}
	if _, ok := byID["loop"]; !ok {
		t.Fatal("default row missing from overlay result")
	}
	// Override keeps the base slot.
	for i, r := range rows {
		if r.ID == "llm" && i != 0 {
			t.Fatalf("llm moved to slot %d", i)
		}
	}
}
