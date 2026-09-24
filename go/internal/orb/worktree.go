package orb

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/projectdef"
)

// gitWaitDelay bounds how long a git killed by its context may keep its
// output open. At a credential prompt the process holding the pipe is
// not git but its remote helper and the prompt it waits on, which the
// kill does not reach: the start sat in CombinedOutput past the
// 30-minute context that was meant to end it.
const gitWaitDelay = 5 * time.Second

func gitOut(ctx context.Context, dir string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...)
	cmd.WaitDelay = gitWaitDelay
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, strings.TrimSpace(string(out)))
	}
	return strings.TrimSpace(string(out)), nil
}

// syncCache makes the bare clone for a remote repo, or refreshes it. A
// failed fetch of an existing clone is tolerated so an offline laptop can
// still resume a session.
func syncCache(ctx context.Context, home, slug string, r projectdef.Repo) error {
	gd := projectdef.CacheGitDir(home, slug, r)
	if _, err := os.Stat(gd); err != nil {
		if err := os.MkdirAll(filepath.Dir(gd), 0o755); err != nil {
			return err
		}
		// Cloned beside it and renamed into place: the cache's existence
		// is what preflight and the next start read as "a clone to fall
		// back on", and a clone in flight (or killed at a credential
		// prompt, which leaves its directory) is not one.
		tmp, err := os.MkdirTemp(filepath.Dir(gd), filepath.Base(gd)+".clone-")
		if err != nil {
			return err
		}
		cmd := exec.CommandContext(ctx, "git", "clone", "--bare", "--quiet", r.Remote, tmp)
		cmd.WaitDelay = gitWaitDelay
		out, err := cmd.CombinedOutput()
		if err != nil {
			os.RemoveAll(tmp)
			return fmt.Errorf("git clone %s: %w: %s", r.Remote, err, strings.TrimSpace(string(out)))
		}
		if err := os.Rename(tmp, gd); err != nil {
			os.RemoveAll(tmp)
			// Another start's clone got there first.
			if _, serr := os.Stat(gd); serr != nil {
				return err
			}
		}
		return nil
	}
	// The cache is keyed by the repo's name, so its origin is the URL of
	// the first clone: a token added to the remote in project.yml later
	// never reached the fetch, which went on prompting for credentials.
	gitOut(ctx, gd, "remote", "set-url", "origin", r.Remote)
	gitOut(ctx, gd, "fetch", "--quiet", "origin", "+refs/heads/*:refs/heads/*")
	return nil
}

// addWorktree checks repo r out at dst on branch bough/<session>, reusing
// an existing worktree (resume) or branch (worktree dir deleted by hand).
func addWorktree(ctx context.Context, home, slug, session string, r projectdef.Repo, dst string) error {
	if _, err := os.Stat(filepath.Join(dst, ".git")); err == nil {
		return nil
	}
	src := projectdef.SourceGitDir(home, slug, r)
	branch := "bough/" + session
	gitOut(ctx, src, "worktree", "prune")
	if _, err := gitOut(ctx, src, "rev-parse", "--verify", "--quiet", "refs/heads/"+branch); err == nil {
		_, err := gitOut(ctx, src, "worktree", "add", "--quiet", dst, branch)
		return err
	}
	_, err := gitOut(ctx, src, "worktree", "add", "--quiet", "-b", branch, dst, r.BaseRef())
	return err
}

// commonGitDir is the directory a worktree's .git file ultimately points
// into; it must be mounted in the container for git to work there.
func commonGitDir(ctx context.Context, worktree string) (string, error) {
	d, err := gitOut(ctx, worktree, "rev-parse", "--path-format=absolute", "--git-common-dir")
	if err != nil {
		return "", err
	}
	return d, nil
}

func removeWorktree(ctx context.Context, wt string) {
	common, err := commonGitDir(ctx, wt)
	if err != nil {
		return
	}
	if _, err := gitOut(ctx, common, "worktree", "remove", "--force", wt); err != nil {
		os.RemoveAll(wt)
		gitOut(ctx, common, "worktree", "prune")
	}
}
