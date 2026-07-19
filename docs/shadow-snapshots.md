# Removing jj: shadow-git snapshots

Status: SHIPPED (2026-07-18, phases 1–4) — shadow is the only backend; jj is
fully removed. Legacy jj-era sessions' changes rails read empty (their
`bough/*` git refs remain in origin repos for salvage). Replaced Jujutsu with a per-project **shadow git repository** for all
working-tree snapshotting, session branching, subagent workspaces, and the
changes rail.

## Why move off jj

- The colocated mode holds bough's own checkout hostage: detached-HEAD snapshot
  chains, no `git stash`, ship-to-main via temp worktrees, forks that clobber
  the working tree. Daily-driving friction, permanently.
- jj is an external binary dependency with its own failure modes we already
  work around in code (store quarantine, stale-workspace repair, `jj git
  export` dances).
- Everything bough actually needs from jj is expressible with git plumbing —
  proven in production by opencode (identical shadow-repo design, 180k★) and
  dura (snapshot commits without touching HEAD).

## Design

One shadow git dir per project, never inside it:

```
~/.bough/shadow/<project-hash>/        # bare git dir (no worktree of its own)
~/.bough/workspaces/<session-id>/      # linked worktrees of the shadow repo (unchanged path)
```

All operations run as `git --git-dir=<shadow> --work-tree=<ws>` (or with cwd
inside a linked worktree). The user's repo keeps its own `.git` untouched:
no refs, no index writes, no HEAD moves, ever. Non-git projects work
identically — the shadow repo doesn't care what the worktree is.

**Every session gets an isolated workspace.** The colocated/external mode
split dies; the "external" recipe becomes the only recipe. Root sessions and
subagents alike get a shadow-repo linked worktree under `~/.bough/workspaces/`.
Bough's own repo stops being special: its primary checkout stays on `main`
like any civilian repo.

### Operation mapping

| jj today | shadow replacement |
|---|---|
| working-copy snapshot (`jj st`) | per-session index: `GIT_INDEX_FILE=<shadow>/indexes/<session> git add -A` (check exit!), `write-tree`, `commit-tree -p tip`, `update-ref refs/bough/sessions/<id>` |
| base capture (first turn) | commit the tree as-is → base sha in `sessions.base` |
| `jj diff -r bookmark` (changes rail) | snapshot-on-read, then `git diff <base> <tip>` → existing `parseGitDiff` |
| fork (`jj new <from>`) | `update-ref refs/bough/sessions/<new> <snapshot-sha>` + new worktree at that sha |
| workspace add (subagents) | `git --git-dir=<shadow> worktree add <dir> <sha>` (worktree metadata lives in the shadow dir) |
| adopt (`jj squash --from --into`) | diff sub-base..sub-tip → `git apply --3way` into parent worktree → snapshot parent (same code as materialize) |
| materialize into user checkout | unchanged — already pure git (`apply --3way`, hash-object); port as-is |
| per-path revert (`jj restore`) | `git checkout <prev-snap> -- <explicit paths>` in the session worktree |
| whole-change undo (`jj undo`) | restore explicit file list from base (never `checkout-index -a`) |
| accept/seal (`describe`+`new`+bookmark move) | `update-ref refs/bough/accepted/<id> <tip>`; tip becomes new base |
| `jj op log` / `op restore` | dead in production today — delete, no replacement |
| store quarantine / stale repair | mostly obsolete; keep a corrupt-shadow quarantine (rename aside + reinit) |

### Pitfalls designed around (opencode scar tissue)

- **Per-session index files**, never a shared index: a shared one restored
  stale cross-session files (opencode #7774).
- **Restores are explicit-path-list only** (from `ls-tree`/`diff --name-only`),
  never index-wide.
- **`git add` failures are fatal to the snapshot**, surfaced as the existing
  workspace `warning` — silent swallow caused undo data loss (opencode #12719).
- Relative paths from worktree root everywhere (opencode #8631).
- `.gitignore` is honored (same semantics as jj). Embedded git repos inside
  the tree become gitlinks — snapshot skips descending into them, same as jj.
- Shadow config: `core.autocrlf=false`, `core.quotepath=false`,
  `gc.auto=0` + explicit prune of session refs older than N days.

### Data model

- `sessions.workspace`, `sessions.base` — same columns, shas instead of jj ids.
- `Diff.source`: `"jj" | "clonefile"` → `"shadow" | "clonefile"` (web + TUI
  enum plumbing only; no logic changes).
- `snapshots` table is dead code (DDL only, never written) — drop it.
- Old jj-era sessions: changes rail returns empty for them (their `bough/*`
  git refs remain in the user repos for manual recovery). No data migration.
- clonefile backend stays for now (non-repo dirs); collapsing it into shadow
  is a possible later simplification since shadow handles non-git dirs too.

## Possible later simplifications

Unify clonefile into shadow; `bough recover <session>` CLI over shadow refs;
fsmonitor for big trees.

(The phased jj→shadow migration plan that used to live here completed on
2026-07-18 — see the git history of this file for the cutover steps.)
