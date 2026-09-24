package ci

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// workDir is the repo's one CI worktree.
func (s *Store) workDir() string { return filepath.Join(s.dir(), "work") }

// commit wraps tree in a parentless commit. The worktree's HEAD points
// at it, which keeps the tree reachable (a Snapshot tree is otherwise
// loose and gc could take it mid-run) and lets a check that asks git
// about HEAD see the tree under test. commit-tree works in a repo with
// no commits yet, which `worktree add` of the tree itself would not.
func (s *Store) commit(ctx context.Context, tree string) (string, error) {
	return git(ctx, s.Repo.Top, nil, "commit-tree", "--no-gpg-sign", "-m", "bough ci "+tree, tree)
}

// checkout moves the CI worktree to tree, creating it on first use.
//
// read-tree -u --reset rewrites only the entries whose blob changed and
// deletes paths the tree lacks, so an unchanged file keeps its mtime and
// go's test cache (which keys on mtime and size among others) stays
// valid. It trusts the index, though, so a tracked file a previous
// check modified in place would be left modified: those are found with
// diff-files and checked out again. Untracked leftovers are cleaned
// without -x, so ignored caches (node_modules, build dirs) survive.
func (s *Store) checkout(ctx context.Context, tree string) error {
	work := s.workDir()
	c, err := s.commit(ctx, tree)
	if err != nil {
		return fmt.Errorf("ci: commit tree %s: %w", short(tree), err)
	}
	if !s.worktreeOK(ctx) {
		if err := s.create(ctx, c); err != nil {
			return err
		}
	}
	// Refresh first: a file a check touched without changing reads as
	// clean again, and read-tree then leaves it (and its mtime) alone
	// instead of rewriting it. --refresh exits 1 when files are really
	// modified, which is what diff-files below is for, so its error is
	// not one.
	_, _ = git(ctx, work, nil, "update-index", "-q", "--refresh")
	if _, err := git(ctx, work, nil, "read-tree", "-u", "--reset", tree); err != nil {
		return fmt.Errorf("ci: move worktree to %s: %w", short(tree), err)
	}
	_, _ = git(ctx, work, nil, "update-index", "-q", "--refresh")
	dirty, err := gitRaw(ctx, work, nil, "diff-files", "--name-only", "-z")
	if err != nil {
		return fmt.Errorf("ci: move worktree to %s: %w", short(tree), err)
	}
	if len(dirty) > 0 {
		if _, err := git(ctx, work, dirty, "checkout-index", "-f", "-z", "--stdin"); err != nil {
			return fmt.Errorf("ci: restore files in worktree: %w", err)
		}
	}
	if _, err := git(ctx, work, nil, "clean", "-fdq"); err != nil {
		return fmt.Errorf("ci: clean worktree: %w", err)
	}
	if _, err := git(ctx, work, nil, "update-ref", "--no-deref", "HEAD", c); err != nil {
		return fmt.Errorf("ci: point worktree HEAD at %s: %w", short(tree), err)
	}
	return nil
}

// worktreeOK reports whether the CI worktree exists and git still knows
// it. A source repo that was re-cloned, or a worktree pruned by hand,
// leaves a directory git no longer recognises.
func (s *Store) worktreeOK(ctx context.Context) bool {
	work := s.workDir()
	if _, err := os.Stat(filepath.Join(work, ".git")); err != nil {
		return false
	}
	common, err := git(ctx, work, nil, "rev-parse", "--path-format=absolute", "--git-common-dir")
	if err != nil {
		return false
	}
	if real, err := filepath.EvalSymlinks(common); err == nil {
		common = real
	}
	return common == s.Repo.CommonDir
}

func (s *Store) create(ctx context.Context, commit string) error {
	work := s.workDir()
	_, _ = git(ctx, s.Repo.Top, nil, "worktree", "prune")
	if err := os.RemoveAll(work); err != nil {
		return fmt.Errorf("ci: remove stale worktree %s: %w", work, err)
	}
	if err := os.MkdirAll(filepath.Dir(work), 0o755); err != nil {
		return fmt.Errorf("ci: state dir: %w", err)
	}
	// --no-checkout: read-tree does the checkout next, and it also keeps
	// the repo's post-checkout hook from running here.
	if _, err := git(ctx, s.Repo.Top, nil, "worktree", "add", "--detach", "--no-checkout", work, commit); err != nil {
		if strings.Contains(err.Error(), "already registered") {
			if _, err2 := git(ctx, s.Repo.Top, nil, "worktree", "add", "-f", "--detach", "--no-checkout", work, commit); err2 == nil {
				return nil
			}
		}
		return fmt.Errorf("ci: create worktree %s: %w", work, err)
	}
	return nil
}
