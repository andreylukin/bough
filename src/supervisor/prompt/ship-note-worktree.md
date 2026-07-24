## Shipping to the user's repo

Another granted host function: await ship({message, paths?, push?}) lands this session's work in the
user's real repository checkout as a git commit. It delivers the changed files into the origin's
working tree (3-way merged with any edits the user made meanwhile; a conflict fails with the file
named), commits them on the origin's current branch with `message` — the user's own staged changes
stay staged and untouched — and with push:true also pushes the branch to its remote with the user's
credentials. `paths` limits the commit to those files; omitted means everything this session
changed. Returns {commit, branch, paths, pushed, note?}. Shipping publishes work outside your
workspace: call it ONLY when the user explicitly asks you to commit/push/ship — never as a routine
end-of-task step — and report the returned commit and branch afterward.

To open a pull request instead of committing onto the current branch, use await pr({title, body?,
branch?, base?, paths?, draft?}). It commits this session's changes onto a NEW branch (default
`bough/<slug>`) on top of the origin's HEAD WITHOUT touching the user's working copy, pushes it, and
opens a GitHub PR against `base` (default the current branch) via `gh pr create` with the host's gh
auth. `paths` limits the commit; omitted means everything. Returns {branch, base, commit, url?,
pushed, paths, note?} — report the returned PR url. Same rule as ship(): call it ONLY when the user
explicitly asks to open a PR.

The workspace itself is a shadow-git worktree that bough snapshots automatically every round: your
edits get committed as `bough: snapshot` and HEAD moves along, so a clean `git status`/`git diff`
does NOT mean your work was lost — it lives in the snapshot chain, and ship() reads it from there.
See the session's cumulative change with `git diff "refs/bough/originbase/$(basename "$PWD")"`.
Avoid `git stash`, `git branch`, `git worktree add`, and `git reset` here: the automatic snapshots
already cover what they would, and only what is on disk at the end of a round gets snapshotted —
leave your final state checked out, never parked in a stash or an unmerged branch.
