# FS sandbox + snapshots — integration seams

Three pure modules, wired into the server in a later wave. This documents where they plug in.
Nothing here imports the server; the server imports these.

Modules:

- `src/sandbox/seatbelt.ts` — Seatbelt profile generation + `wrap()` for subprocesses.
- `src/vcs/shadow.ts` — shadow-git per-session snapshots/branching for **repo** work
  (docs/shadow-snapshots.md).
- `src/vcs/clonefile.ts` — APFS clonefile snapshots for **non-git config** (`~/.zshrc`,
  `~/.config`).
- `src/schema/changes.ts` — the `Diff` contract shared by both snapshot sources (the Changes tab).

## 1. Seatbelt — wrapping the bash tool

The bash tool (`src/tools/bash.ts`) currently spawns `sh` directly. To sandbox it, wrap the argv
before spawning:

```ts
import { wrap } from "../sandbox/seatbelt.ts";

const argv = wrap(["/bin/sh", "-c", command], {
  workspace,               // the session's repo/working dir (the rw root)
  allowWrite: [snapshotDir], // clonefile session dir, so the agent can edit config clones
  // home defaults to $HOME; denyRead adds to the credential denylist
});
const cmd = new Deno.Command(argv[0], { args: argv.slice(1), cwd: workspace, ... });
```

- The profile is **deny-write except workspace + a curated toolchain allowlist** (caches,
  `/private/tmp`, `~/.cargo`, `~/.npm`, …) and **allow-read except a credential/secret denylist**
  (`~/.ssh`, `~/.aws`, keychains, shell rc/history, browser data).
- **Network is intentionally NOT restricted here** — egress is owned by the Claw Patrol layer
  (`src/net/**`). This module is the filesystem/process half only.
- The profile travels inline (`sandbox-exec -p <profile>`), so there's no temp file and concurrent
  sessions don't race.
- macOS-only. On other platforms, don't wrap (or gate on `Deno.build.os === "darwin"`).

## 2. shadow — session lifecycle for repo work

One shadow git repository per origin directory (`~/.bough/shadow/<name>-<hash>`), holding every
session's snapshot history on `refs/bough/{sessions,base}/<id>`. Every session runs in its own
linked worktree of the shadow repo under `~/.bough/workspaces/<sessionId>`, branched off a captured
snapshot of the origin's working tree (uncommitted + untracked included). The origin's checkout —
HEAD, branch, index, `git status` — is never modified; non-git origin dirs work identically.
`prepareShadow` in `src/supervisor/workspace.ts` wires the session lifecycle to it.

| Session event      | Call                                                                             |
| ------------------ | -------------------------------------------------------------------------------- |
| session created    | `createSessionWorkspace(origin, sessionId)` — worktree off a working-tree snapshot |
| session resumed    | nothing — the workspace column already points at the worktree                    |
| session forked / subagent spawned | `addWorkspace(parentDir, toId, dir, fromSessionId)` — worktree off the parent's tip |
| render Changes tab | `diff(dir, sessionId)` → `Diff` (source: `"shadow"`), always base..tip           |
| apply              | `materialize(dir, id, origin, paths)` + `accept(dir, id, msg)` when all covered  |
| revert             | `revertPaths(dir, id, paths)` (per-path) or `undoAll(dir, id)` (whole change)    |
| adopt (subagent)   | `adoptChanges(parentDir, subDir, fromId, intoId)` — 3-way apply + base advance   |

- `track()` (snapshot) staples the worktree to the session tip ref on every diff/apply/revert; a
  failed `git add` is fatal, never swallowed. Restores only ever touch explicit path lists.

## 3. clonefile — non-git config

For paths outside a repo. The agent edits **clones**, not the originals.

```ts
import { applyBack, diff, sessionDir, snapshotPaths } from "../vcs/clonefile.ts";

await snapshotPaths(sessionId, ["/Users/x/.zshrc", "/Users/x/.config/nvim"]); // cp -c clones
// ... agent edits the clones under sessionDir(sessionId) ...
const d = await diff(sessionId); // Diff (source: "clonefile"); paths are the originals
await applyBack(sessionId, approvedPaths); // copy approved clones back over originals
```

- **`sessionDir(sessionId)` must be in the seatbelt `allowWrite` list** (seam #1) so the sandboxed
  agent can write the clones. This is the one cross-module dependency.
- Default snapshot root is `~/.bough/snapshots/<sessionId>`; pass a `base` arg to override (tests
  do, to avoid touching real config).
- `applyBack` clones approved files back (`cp -c`) and honors deletions (a file the agent removed
  from the clone is removed from the original).
- macOS/APFS only (`cp -c` clonefile).

## 4. The `Diff` contract (Changes tab)

`src/schema/changes.ts`:

```ts
Diff     = { source: "jj" | "clonefile" | "shadow", files: FileDiff[] }
FileDiff = { path: string, status: "added" | "modified" | "deleted", hunks: Hunk[] }
Hunk     = { header: string, lines: string[] }   // "@@ … @@" + body lines with ` `/`+`/`-` markers
```

Both sources produce byte-identical structure via `parseGitDiff()`. The UI's Changes rail renders a
`Diff` and calls the matching apply/revert path: shadow apply → `materialize` + `accept` (seal:
base advances onto tip), shadow revert → `revertPaths`/`undoAll`, clonefile apply →
`applyBack(sessionId, approvedPaths)`. Renames surface as delete + add (git default without `-M`);
binary/empty files yield a `FileDiff` with no hunks.

## Permissions

All three shell out, so the server and tests need `--allow-run` (already present in the `dev`/`test`
tasks in `deno.json`). The subprocess tests self-skip when run permission is absent, so they never
turn the suite red under a reduced permission set; they execute for real when `--allow-run` is
granted. Required binaries on PATH: `git`, `cp`, `/usr/bin/sandbox-exec`.
