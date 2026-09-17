package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/andreylukin/bough/internal/container"
	"github.com/andreylukin/bough/internal/orb"
)

// B1: prune removes failed and archived orbs after a confirm that lists
// what goes and what stays; rm refuses an orb whose owner still runs.
func TestProjectPruneAndRm(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	rt := container.NewFake()
	old := projectRuntime
	projectRuntime = func() container.Runtime { return rt }
	t.Cleanup(func() { projectRuntime = old })
	put := func(s orb.State) {
		s.UpdatedAt = time.Now()
		b, _ := json.Marshal(s)
		dir := orb.Dir(home, s.Session)
		os.MkdirAll(dir, 0o755)
		os.WriteFile(filepath.Join(dir, "state.json"), b, 0o644)
	}
	put(orb.State{Session: "s-failed", Project: "web", Status: orb.StatusFailed})
	put(orb.State{Session: "s-arch", Project: "web", Status: orb.StatusStopped})
	put(orb.State{Session: "s-kept", Project: "web", Status: orb.StatusStopped})
	put(orb.State{Session: "s-live", Project: "web", Status: orb.StatusRunning, PID: os.Getpid()})
	put(orb.State{Session: "s-other", Project: "api", Status: orb.StatusFailed})
	os.MkdirAll(filepath.Join(home, ".bough", "serve"), 0o755)
	os.WriteFile(filepath.Join(home, ".bough", "serve", "meta.json"), []byte(`{"sessions":{"s-arch":{"archived":true}}}`), 0o644)
	run := func(stdin string, args ...string) (string, error) {
		var out bytes.Buffer
		err := project(&out, strings.NewReader(stdin), args)
		return out.String(), err
	}
	exists := func(s string) bool { _, err := os.Stat(orb.Dir(home, s)); return err == nil }

	out, err := run("n\n", "prune", "web")
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"delete  orb s-failed", "delete  orb s-arch", "keep    orb s-kept", "keep    orb s-live", "Nothing removed"} {
		if !strings.Contains(out, want) {
			t.Errorf("prune plan lacks %q:\n%s", want, out)
		}
	}
	if strings.Contains(out, "s-other") || !exists("s-failed") {
		t.Fatalf("declined prune touched things:\n%s", out)
	}

	if out, err = run("y\n", "prune", "web"); err != nil {
		t.Fatal(err)
	}
	if exists("s-failed") || exists("s-arch") || !exists("s-kept") || !exists("s-live") || !exists("s-other") {
		t.Fatalf("prune removed the wrong orbs:\n%s", out)
	}

	if _, err := run("", "rm", "s-live", "--yes"); err == nil || !exists("s-live") {
		t.Fatalf("rm removed a live orb: %v", err)
	}
	if out, err = run("", "rm", "s-kept", "--yes"); err != nil || exists("s-kept") {
		t.Fatalf("rm: %v\n%s", err, out)
	}
}
