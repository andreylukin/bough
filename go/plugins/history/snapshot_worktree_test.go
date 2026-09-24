package history

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

func snapshotGit(t *testing.T, dir string, args ...string) {
	t.Helper()
	c := exec.Command("git", args...)
	c.Dir = dir
	c.Env = append(os.Environ(), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t")
	if out, err := c.CombinedOutput(); err != nil {
		t.Errorf("git %v: %v %s", args, err, out)
	}
}

// snapshotRepo makes a repo under base with a.txt = "v0\n" committed
// on main, ready for `git worktree add`.
func snapshotRepo(t *testing.T, base string) string {
	t.Helper()
	repo := filepath.Join(base, "repo")
	if err := os.MkdirAll(repo, 0o755); err != nil {
		t.Error(err)
	}
	snapshotGit(t, repo, "init", "-q", "-b", "main")
	if err := os.WriteFile(filepath.Join(repo, "a.txt"), []byte("v0\n"), 0o644); err != nil {
		t.Error(err)
	}
	snapshotGit(t, repo, "add", "a.txt")
	snapshotGit(t, repo, "commit", "-q", "-m", "v0")
	return repo
}

// An edit written right after `git worktree add` wrote the index used
// to be missed about once in forty tries: Snapshot's temp index copy
// was stamped "now", so git trusted the stale cached stat and the
// checkpoint kept the old content. Orb sessions always start in a fresh
// worktree, and that is where TestForkThenUndoFileIsolation caught it.
// This is the real-timing version: 60 tries as before, spread over
// independent repos in parallel (it took 8.7 s sequential under -race).
// TestSnapshotSeesSameStatEdit is the deterministic reproduction.
func TestSnapshotSeesEditInFreshWorktree(t *testing.T) {
	t.Parallel()
	const workers, tries = 6, 10
	var wg sync.WaitGroup
	for w := range workers {
		wg.Go(func() {
			base := t.TempDir()
			repo := snapshotRepo(t, base)
			for i := range tries {
				wt := filepath.Join(base, fmt.Sprintf("wt%d", i))
				snapshotGit(t, repo, "worktree", "add", "-q", "-b", fmt.Sprintf("b%d", i), wt, "main")
				before, err := Snapshot(wt)
				if err != nil {
					t.Error(err)
					return
				}
				// Same size as v0, written immediately: the racy case.
				if err := os.WriteFile(filepath.Join(wt, "a.txt"), []byte("v1\n"), 0o644); err != nil {
					t.Error(err)
					return
				}
				after, err := Snapshot(wt)
				if err != nil {
					t.Error(err)
					return
				}
				if after == before {
					t.Errorf("worker %d try %d: snapshot missed the edit in a fresh worktree (tree %s)", w, i, after)
					return
				}
			}
		})
	}
	wg.Wait()
}

// The same race, forced: the edit keeps a.txt's size and mtime, and the
// index is stamped with that mtime too, as when the checkout, the index
// write and the edit all land in one clock tick. Only git's racy-clean
// check (file mtime >= index mtime) can then see the edit, and it only
// fires if Snapshot's temp index carries the real index's mtime. ctime
// is switched off because a test cannot set it, and a checkout and an
// edit in different seconds give the change away through it — likely
// why an earlier forced-mtime attempt never reproduced the bug.
func TestSnapshotSeesSameStatEdit(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	repo := snapshotRepo(t, base)
	snapshotGit(t, repo, "config", "core.trustctime", "false")
	wt := filepath.Join(base, "wt")
	snapshotGit(t, repo, "worktree", "add", "-q", "-b", "b", wt, "main")
	// A whole second an hour ago: a temp index stamped "now" is then
	// strictly newer than the file, whatever git's clock resolution.
	m := time.Now().Add(-time.Hour).Truncate(time.Second)
	file := filepath.Join(wt, "a.txt")
	if err := os.Chtimes(file, m, m); err != nil {
		t.Fatal(err)
	}
	// Cache that stat in the real index.
	snapshotGit(t, wt, "update-index", "--refresh")
	before, err := Snapshot(wt)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(file, []byte("v1\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(file, m, m); err != nil {
		t.Fatal(err)
	}
	index := filepath.Join(repo, ".git", "worktrees", "wt", "index")
	if err := os.Chtimes(index, m, m); err != nil {
		t.Fatal(err)
	}
	after, err := Snapshot(wt)
	if err != nil {
		t.Fatal(err)
	}
	if after == before {
		t.Fatalf("snapshot missed a same-size, same-mtime edit (tree %s)", after)
	}
}
