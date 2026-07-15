# FS sandbox + snapshots — integration seams

Three pure modules, wired into the server in a later wave. This documents where they plug in.
Nothing here imports the server; the server imports these.

Modules:

- `src/sandbox/seatbelt.ts` — Seatbelt profile generation + `wrap()` for subprocesses.
- `src/vcs/jj.ts` — jj (Jujutsu) per-session snapshots/branching for **repo** work.
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

## 2. jj — session lifecycle for repo work

One jj bookmark per session (`bough/<sessionId>`). jj state placement depends on the repo (decided
by `prepareRepo` in `src/supervisor/workspace.ts`):

- **External (default for plain git repos):** jj never touches the repo. The store lives under
  `~/.bough/jj/<repo>-<hash>` (`jj git init --git-repo`), and each session runs in its own jj
  workspace under `~/.bough/workspaces/<sessionId>`, branched off a captured snapshot of the repo's
  working tree (uncommitted + untracked included). The user's checkout — HEAD, branch, index,
  `git status` — is never modified; session tips are exported as `bough/<id>` git branches so plain
  git can still see them.
- **Colocated (legacy):** repos that already carry `.jj` next to `.git` (a checkout the user
  deliberately runs jj in, e.g. bough's own dev repo) keep the in-place model: sessions share the
  primary checkout and `jj new` moves it onto the session's change.

| Session event                     | Call                                                                                                   |
| --------------------------------- | ------------------------------------------------------------------------------------------------------ |
| session created (root, external)  | `createSessionWorkspace(repo, sessionId)` — isolated working copy off a working-tree snapshot          |
| session created (root, colocated) | `ensureWorkspace(repo, sessionId, base?)` — new change off the working-copy snapshot                   |
| session resumed                   | external: `updateStale(dir)`; colocated: `ensureWorkspace` (idempotent)                                |
| session forked                    | external: `addWorkspace(parentDir, toId, dir, bookmark)`; colocated: `forkSession(repo, fromId, toId)` |
| render Changes tab                | `diff(dir, sessionId)` → `Diff` (source: `"jj"`)                                                       |
| revert                            | `undo(dir)` (last op) or `operations(dir)` + `restore(dir, opId)`                                      |

- `base` is the commit a new session branches from. The `sessions` row is the natural home for it
  (store the repo's HEAD at attach time and pass it back on resume/fork); default is
  `git rev-parse HEAD`, falling back to the jj root for an empty repo.
- `diff()` snapshots first (jj auto-snapshots on any command) and returns the
  **change-vs-its-parent** diff — exactly what the session changed since it branched. If a session
  ever accumulates multiple internal jj commits, switch to a `--from <base> --to <bookmark>` range;
  the single-change form covers the normal flow where all edits land on one working-copy change.
- **Caveat:** `forkSession` makes the fork a child of the source. Editing the source again after
  forking rebases the fork onto the new tip. For a frozen fork, branch off the source's parent (or
  `jj duplicate`) instead — revisit if the UI needs it.

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
Diff     = { source: "jj" | "clonefile", files: FileDiff[] }
FileDiff = { path: string, status: "added" | "modified" | "deleted", hunks: Hunk[] }
Hunk     = { header: string, lines: string[] }   // "@@ … @@" + body lines with ` `/`+`/`-` markers
```

Both sources produce byte-identical structure via `parseGitDiff()`. The UI's Changes rail renders a
`Diff` and calls the matching apply/revert path: jj apply → `accept` (seal the change, advance the
session bookmark — whole-change in v1), jj revert → `undo`, clonefile apply →
`applyBack(sessionId, approvedPaths)`. Renames surface as delete + add (git default without `-M`);
binary/empty files yield a `FileDiff` with no hunks.

## Permissions

All three shell out, so the server and tests need `--allow-run` (already present in the `dev`/`test`
tasks in `deno.json`). The subprocess tests self-skip when run permission is absent, so they never
turn the suite red under a reduced permission set; they execute for real when `--allow-run` is
granted. Required binaries on PATH: `jj` (>= 0.42, `brew install jj`), `git`, `cp`,
`/usr/bin/sandbox-exec`.
