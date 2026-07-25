# Version control: plain git

This repo is a normal git checkout on `main`, and bough works in it **in place** —
no shadow store, no per-session worktree, no overlay. Edits land in the real files
immediately, so git is the only record of what a session changed (each session's
starting sha is recorded in the db and drives the Changes rail).

- Ship work with ordinary commits on `main` (branch first for anything you'd
  want reviewed as a PR).
- There is no snapshot store to salvage from: uncommitted work lives only in the
  working tree.
- Legacy jj-era session refs may still exist as `bough/<session-id>` branches in
  older repos; they are inert.

# Server / working tree

The live bough server builds from this working tree and is respawned by
launchd on kill. The UI is the `bough` TUI (src/tui/), which runs from source —
there is no web UI or build step.
