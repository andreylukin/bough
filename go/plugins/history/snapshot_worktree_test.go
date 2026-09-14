package history

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

// An edit written right after `git worktree add` wrote the index used
// to be missed about once in forty tries: Snapshot's temp index copy
// was stamped "now", so git trusted the stale cached stat and the
// checkpoint kept the old content. Orb sessions always start in a fresh
// worktree, and that is where TestForkThenUndoFileIsolation caught it.
// The race is timing-dependent, so this repeats it rather than forcing
// timestamps (a forced-mtime version never reproduced it).
func TestSnapshotSeesEditInFreshWorktree(t *testing.T) {
	t.Parallel()
	git := func(dir string, args ...string) {
		t.Helper()
		c := exec.Command("git", args...)
		c.Dir = dir
		c.Env = append(os.Environ(), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t")
		if out, err := c.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	base := t.TempDir()
	repo := filepath.Join(base, "repo")
	if err := os.MkdirAll(repo, 0o755); err != nil {
		t.Fatal(err)
	}
	git(repo, "init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(repo, "a.txt"), []byte("v0\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	git(repo, "add", "a.txt")
	git(repo, "commit", "-q", "-m", "v0")

	for i := range 60 {
		wt := filepath.Join(base, fmt.Sprintf("wt%d", i))
		git(repo, "worktree", "add", "-q", "-b", fmt.Sprintf("b%d", i), wt, "main")
		before, err := Snapshot(wt)
		if err != nil {
			t.Fatal(err)
		}
		// Same size as v0, written immediately: the racy case.
		if err := os.WriteFile(filepath.Join(wt, "a.txt"), []byte("v1\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		after, err := Snapshot(wt)
		if err != nil {
			t.Fatal(err)
		}
		if after == before {
			t.Fatalf("try %d: snapshot missed the edit in a fresh worktree (tree %s)", i, after)
		}
	}
}
