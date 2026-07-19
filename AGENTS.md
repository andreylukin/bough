# Version control: plain git

This repo is a normal git checkout on `main`. Session snapshotting moved from
jj colocation to per-project **shadow git repos** (docs/shadow-snapshots.md);
bough keeps all snapshot state under `~/.bough/shadow` and `~/.bough/workspaces`,
so nothing here ever detaches HEAD or fights the index anymore.

- Ship work with ordinary commits on `main` (branch first for anything you'd
  want reviewed as a PR).
- Session snapshot history, if you ever need to salvage from it, lives in the
  shadow stores: `git --git-dir ~/.bough/shadow/<name>-<hash> log refs/bough/sessions/<id>`.
- Legacy jj-era session refs may still exist as `bough/<session-id>` branches in
  older repos; they are inert.

# Server / working tree

The live bough server builds from this working tree and is respawned by
launchd on kill. The UI is the `bough` TUI (src/tui/), which runs from source —
there is no web UI or build step.
