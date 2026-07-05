# Version control: git only

This repo is jj-colocated, but **jj belongs to bough itself** — the live bough
server drives jj and auto-snapshots the working tree into anonymous per-session
commits (`bough/*` bookmarks). In Claude Code, use plain git for everything.

- Never run `jj` commands. To inspect jj state read-only, use the git side:
  session snapshots are reachable as `bough/<session-id>` refs.
- Never `git stash` — jj's index snapshots make stash push/pop fail messily.
- The primary checkout usually sits on a detached-HEAD snapshot chain, and a
  clean `git status` does NOT mean "nothing pending": jj absorbs tree changes
  into snapshot commits. "What's unshipped" = `git diff main HEAD` (tree-to-tree).
- To ship work to main: create a temp `git worktree` on main, `git checkout
  <snapshot-sha> -- <paths>` per logical group, commit, test, push, remove the
  worktree. Never rewrite or reset the primary checkout.

A PreToolUse hook (.claude/hooks/block-jj.sh) enforces the first two rules.

# Server / working tree

The live bough server builds from this working tree and is respawned by
launchd on kill. UI changes additionally need `npm run build` in web/.
