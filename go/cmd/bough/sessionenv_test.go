package main

import (
	"os"
	"testing"
)

// Not parallel: t.Setenv forbids it, and the vars are process-wide.
// After the take, a tools.bash child must not inherit either var.
func TestTakeSessionEnvClears(t *testing.T) {
	t.Setenv("BOUGH_SPAWNED_BY", "parent-1")
	t.Setenv("BOUGH_SESSION_ID", "child-1")
	by, id := takeSessionEnv()
	if by != "parent-1" || id != "child-1" {
		t.Fatalf("takeSessionEnv = %q, %q", by, id)
	}
	for _, k := range []string{"BOUGH_SPAWNED_BY", "BOUGH_SESSION_ID"} {
		if _, set := os.LookupEnv(k); set {
			t.Fatalf("%s still set after take", k)
		}
	}
}

// BOUGH_PROJECT_DIR is this session's project directory, and it leaves
// the environment with the rest: a `bough` the agent runs from
// tools.bash must not inherit this session's project.
func TestTakeProjectDirEnvClears(t *testing.T) {
	t.Setenv("BOUGH_PROJECT_DIR", "/h/.bough/projects/web")
	if dir := takeProjectDirEnv(); dir != "/h/.bough/projects/web" {
		t.Fatalf("takeProjectDirEnv = %q", dir)
	}
	if _, set := os.LookupEnv("BOUGH_PROJECT_DIR"); set {
		t.Fatal("BOUGH_PROJECT_DIR still set after take")
	}
	if dir := takeProjectDirEnv(); dir != "" {
		t.Fatalf("unset takeProjectDirEnv = %q", dir)
	}
}

// The directory never chooses a mode, and a project session takes its
// project from its slug: a stray BOUGH_PROJECT_DIR must not point it at
// another project's MEMORY.md.
func TestProjectDirForLocalOnly(t *testing.T) {
	if got := projectDirFor("local", "/h/.bough/projects/web/"); got != "/h/.bough/projects/web" {
		t.Errorf("local = %q", got)
	}
	if got := projectDirFor("project", "/h/.bough/projects/other"); got != "" {
		t.Errorf("project session = %q, want none", got)
	}
	if got := projectDirFor("local", ""); got != "" {
		t.Errorf("unset = %q", got)
	}
}

// BOUGH_PROJECT_MAIN says this session is its project's main thread,
// and leaves the environment with the rest: a `bough` the agent runs
// from its own shell must not claim to be the main thread too.
func TestTakeMainEnvClears(t *testing.T) {
	t.Setenv("BOUGH_PROJECT_MAIN", "1")
	if !takeMainEnv() {
		t.Fatal("takeMainEnv did not read the flag")
	}
	if _, set := os.LookupEnv("BOUGH_PROJECT_MAIN"); set {
		t.Fatal("BOUGH_PROJECT_MAIN still set after take")
	}
	if takeMainEnv() {
		t.Fatal("unset takeMainEnv is true")
	}
}
