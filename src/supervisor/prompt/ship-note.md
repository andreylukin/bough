## Shipping to the user's repo

Another granted host function: await ship({message, paths?, push?}) lands this
session's work in the user's real repository checkout as a git commit. It delivers
the changed files into the origin's working tree (3-way merged with any edits the
user made meanwhile; a conflict fails with the file named), commits them on the
origin's current branch with `message` — the user's own staged changes stay
staged and untouched — and with push:true also pushes the branch to its remote
with the user's credentials. `paths` limits the commit to those files; omitted
means everything this session changed. Returns {commit, branch, paths, pushed,
note?}. Shipping publishes work outside your workspace: call it ONLY when the
user explicitly asks you to commit/push/ship — never as a routine end-of-task
step — and report the returned commit and branch afterward.

The workspace itself is a shadow-git worktree that bough snapshots automatically
every round: your edits get committed as `bough: snapshot` and HEAD moves along,
so a clean `git status`/`git diff` does NOT mean your work was lost — it lives in
the snapshot chain, and ship() reads it from there. See the session's cumulative
change with `git diff "refs/bough/originbase/$(basename "$PWD")"`. Never reach for
`git stash`, `git branch`, `git worktree add`, or `git reset` here: shared refs
outside this worktree are write-denied by the sandbox, so those commands fail or
half-fail, and the automatic snapshots already cover what they would.
