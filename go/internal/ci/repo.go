// Package ci is `bough ci`: named checks run against a checkpoint of
// the working tree — a git tree id, the same object /undo restores
// from — with each result stored under a key derived from what the
// check can see, so a check whose inputs did not change is reported
// from the cache instead of run again.
//
// Checks never run in the live working directory: an agent is editing
// it, and a build that writes into it would show up as the agent's own
// change. They run in one persistent git worktree per repository, moved
// from tree to tree with read-tree so an unchanged file keeps its mtime
// and go's build and test caches keep hitting.
package ci

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/andreylukin/bough/plugins/history"
)

// gitEnv pins what git reads from outside the repo. commit-tree needs an
// identity, and a checkout on a machine with no user.name must still
// run its checks; the system config is skipped so a packaged default
// (a hook path, a filter) cannot change what lands in the CI worktree.
var gitEnv = []string{
	"GIT_CONFIG_NOSYSTEM=1",
	"GIT_AUTHOR_NAME=bough-ci", "GIT_AUTHOR_EMAIL=bough-ci@localhost",
	"GIT_COMMITTER_NAME=bough-ci", "GIT_COMMITTER_EMAIL=bough-ci@localhost",
	"GIT_TERMINAL_PROMPT=0",
}

// git runs one git command in dir, returning trimmed stdout; stderr
// rides along in the error.
func git(ctx context.Context, dir string, stdin []byte, args ...string) (string, error) {
	out, err := gitRaw(ctx, dir, stdin, args...)
	return strings.TrimSpace(string(out)), err
}

func gitRaw(ctx context.Context, dir string, stdin []byte, args ...string) ([]byte, error) {
	c := exec.CommandContext(ctx, "git", args...)
	c.Dir = dir
	c.Env = append(os.Environ(), gitEnv...)
	if stdin != nil {
		c.Stdin = bytes.NewReader(stdin)
	}
	var out, stderr bytes.Buffer
	c.Stdout, c.Stderr = &out, &stderr
	if err := c.Run(); err != nil {
		return nil, fmt.Errorf("git %s: %w: %s", args[0], err, strings.TrimSpace(stderr.String()))
	}
	return out.Bytes(), nil
}

// Repo is the checkout a `bough ci` call is about.
type Repo struct {
	Top       string // toplevel of the checkout the call started in
	CommonDir string // absolute, symlinks resolved: shared by every worktree
	// Key names the repo's CI state dir. It hashes the common dir, so
	// every worktree of one repository — each orb session's included —
	// shares one CI worktree and one result cache: a tree checked in one
	// is cached for all.
	Key string
}

// Open finds the repository around dir.
func Open(dir string) (Repo, error) {
	ctx := context.Background()
	top, err := git(ctx, dir, nil, "rev-parse", "--show-toplevel")
	if err != nil {
		return Repo{}, fmt.Errorf("ci: %s is not in a git repository: %w", dir, err)
	}
	common, err := git(ctx, dir, nil, "rev-parse", "--path-format=absolute", "--git-common-dir")
	if err != nil {
		return Repo{}, fmt.Errorf("ci: git common dir of %s: %w", top, err)
	}
	if real, err := filepath.EvalSymlinks(common); err == nil {
		common = real
	}
	sum := sha256.Sum256([]byte(common))
	return Repo{Top: top, CommonDir: common, Key: repoName(common) + "-" + hex.EncodeToString(sum[:])[:12]}, nil
}

// repoName is a readable prefix for the key: the main checkout's
// directory name, or a bare repo's name without ".git".
func repoName(common string) string {
	name := filepath.Base(common)
	if name == ".git" {
		name = filepath.Base(filepath.Dir(common))
	}
	name = strings.TrimSuffix(name, ".git")
	if name == "" || name == "." || name == string(filepath.Separator) {
		return "repo"
	}
	return name
}

// Resolve turns a tree-ish into a tree id. "" is the working tree as it
// is now, written with history.Snapshot — the same content-addressed id a
// turn checkpoint of the same files gets, so both share a cache entry.
// The newest turn ref is deliberately not the default: a turn is
// checkpointed before the model acts, so it never holds the agent's
// latest edits.
func (r Repo) Resolve(ctx context.Context, treeish string) (string, error) {
	if treeish == "" {
		t, err := history.SnapshotContext(ctx, r.Top)
		if err != nil {
			return "", fmt.Errorf("ci: snapshot %s: %w", r.Top, err)
		}
		return t, nil
	}
	if strings.HasPrefix(treeish, "-") {
		return "", fmt.Errorf("ci: --tree %q is not a tree-ish", treeish)
	}
	t, err := git(ctx, r.Top, nil, "rev-parse", "--verify", "--quiet", treeish+"^{tree}")
	if err != nil || t == "" {
		return "", fmt.Errorf("ci: --tree %q does not name a tree in %s", treeish, r.Top)
	}
	return t, nil
}

// StateDir is where the repo's CI worktree, lock and results live.
func StateDir(home string, r Repo) string {
	return filepath.Join(home, ".bough", "ci", r.Key)
}

func short(tree string) string {
	if len(tree) > 10 {
		return tree[:10]
	}
	return tree
}
