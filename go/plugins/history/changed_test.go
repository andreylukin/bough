package history

// What a turn changed is observed by bracketing it with two
// checkpoints, so a file written by a shell command counts the same as
// one written through a tool.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// shellRepo is a checkout with one committed file, plus the env that
// makes git commit without a configured identity.
func shellRepo(t *testing.T) (dir string, env []string) {
	t.Helper()
	dir = t.TempDir()
	if r, err := filepath.EvalSymlinks(dir); err == nil {
		dir = r
	}
	env = []string{"GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t"}
	if _, err := git(dir, env, "init", "-q", "-b", "main"); err != nil {
		t.Skipf("git unavailable: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "tracked.txt"), []byte("one\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := git(dir, env, "add", "."); err != nil {
		t.Fatal(err)
	}
	if _, err := git(dir, env, "commit", "-qm", "one"); err != nil {
		t.Fatal(err)
	}
	return dir, env
}

func TestChangedFilesSeesAShellEditAndANewFile(t *testing.T) {
	t.Parallel()
	dir, _ := shellRepo(t)
	before, err := Snapshot(dir)
	if err != nil {
		t.Fatal(err)
	}
	// Written by neither tools.write nor tools.patch: the case the
	// turn-stats tally could never see.
	if err := os.WriteFile(filepath.Join(dir, "tracked.txt"), []byte("two\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "fresh.txt"), []byte("new\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	after, err := Snapshot(dir)
	if err != nil {
		t.Fatal(err)
	}

	got, err := ChangedFiles(dir, before, after)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 || !contains(got, "tracked.txt") || !contains(got, "fresh.txt") {
		t.Fatalf("changed = %v, want the edited and the new file", got)
	}
}

// The trap: git prints toplevel-relative paths, Restore joins relative
// paths to the working directory. From a subdirectory those differ, so
// the recorded path must be relative to cwd or /undo reverts the wrong
// one.
func TestChangedFilesPathsAreRelativeToTheWorkingDirectory(t *testing.T) {
	t.Parallel()
	top, _ := shellRepo(t)
	sub := filepath.Join(top, "go", "plugins")
	if err := os.MkdirAll(sub, 0o755); err != nil {
		t.Fatal(err)
	}
	before, err := Snapshot(sub)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(sub, "deep.txt"), []byte("x\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	after, err := Snapshot(sub)
	if err != nil {
		t.Fatal(err)
	}

	got, err := ChangedFiles(sub, before, after)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0] != "deep.txt" {
		t.Fatalf("changed = %v, want [deep.txt] relative to the cwd, not go/plugins/deep.txt", got)
	}
	// And the path Restore is handed must resolve back to the file.
	if _, err := os.Stat(filepath.Join(sub, got[0])); err != nil {
		t.Errorf("recorded path does not resolve from the cwd: %v", err)
	}
}

// A file above the working directory cannot be expressed relative to
// it, so it is recorded absolute — which Restore also accepts.
func TestChangedFilesOutsideTheWorkingDirectoryStayAbsolute(t *testing.T) {
	t.Parallel()
	top, _ := shellRepo(t)
	sub := filepath.Join(top, "sub")
	if err := os.MkdirAll(sub, 0o755); err != nil {
		t.Fatal(err)
	}
	before, err := Snapshot(sub)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(top, "tracked.txt"), []byte("changed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	after, err := Snapshot(sub)
	if err != nil {
		t.Fatal(err)
	}

	got, err := ChangedFiles(sub, before, after)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || !filepath.IsAbs(got[0]) || !strings.HasSuffix(got[0], "tracked.txt") {
		t.Fatalf("changed = %v, want one absolute path to tracked.txt", got)
	}
}

func TestChangedIsQuietWithoutACheckpoint(t *testing.T) {
	t.Parallel()
	c := &Checkpoints{session: "s"}
	if got := c.Changed(""); got != nil {
		t.Errorf("no checkpoint = %v, want nil", got)
	}
}

// A turn that changed nothing records nothing, rather than every file
// in the repo.
func TestChangedFilesEmptyWhenNothingMoved(t *testing.T) {
	t.Parallel()
	dir, _ := shellRepo(t)
	before, err := Snapshot(dir)
	if err != nil {
		t.Fatal(err)
	}
	after, err := Snapshot(dir)
	if err != nil {
		t.Fatal(err)
	}
	got, err := ChangedFiles(dir, before, after)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 0 {
		t.Errorf("changed = %v, want none", got)
	}
}

func contains(ss []string, want string) bool {
	for _, s := range ss {
		if s == want {
			return true
		}
	}
	return false
}
