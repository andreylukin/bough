package history

// Where a session's work happened, recorded on the "meta" entry.
//
// The working directory alone does not identify work: bough started
// from a home directory holding many repos records the same cwd for
// every session, so cwd-based grouping and filtering collapse to one
// bucket. The repository root and branch are the attributes people
// actually remember a task by ("the migration on feat-x"), so they are
// captured when they exist. Outside a repo both are "", and every
// caller treats that as "unknown" rather than an error: git may be
// missing, the directory may not be a checkout, and neither is worth
// failing a session over.

import "path/filepath"

// repoInfo is the git repository root and checked-out branch for dir,
// or "", "" when dir is not in a repo (or git is unavailable). A
// detached HEAD has a root but no branch name.
func repoInfo(dir string) (repo, branch string) {
	root, err := git(dir, nil, "rev-parse", "--show-toplevel")
	if err != nil || root == "" {
		return "", ""
	}
	// --abbrev-ref prints "HEAD" on a detached head; that is a state,
	// not a branch, so it is reported as no branch at all.
	if b, err := git(dir, nil, "rev-parse", "--abbrev-ref", "HEAD"); err == nil && b != "HEAD" {
		branch = b
	}
	return filepath.Clean(root), branch
}
