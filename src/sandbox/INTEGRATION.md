# FS sandbox + snapshots — integration seams

Three pure modules, wired into the server in a later wave. This documents where they
plug in. Nothing here imports the server; the server imports these.

Modules:

- `src/sandbox/seatbelt.ts` — Seatbelt profile generation + `wrap()` for subprocesses.
- `src/vcs/jj.ts` — jj (Jujutsu) per-session snapshots/branching for **repo** work.
- `src/vcs/clonefile.ts` — APFS clonefile snapshots for **non-git config** (`~/.zshrc`, `~/.config`).
- `src/schema/changes.ts` — the `Diff` contract shared by both snapshot sources (the Changes tab).

## 1. Seatbelt — wrapping the bash tool

The bash tool (`src/tools/bash.ts`) currently spawns `sh` directly. To sandbox it,
wrap the argv before spawning:

```ts
import { wrap } from "../sandbox/seatbelt.ts";

const argv = wrap(["/bin/sh", "-c", command], {
  workspace,               // the session's repo/working dir (the rw root)
  allowWrite: [snapshotDir], // clonefile session dir, so the agent can edit config clones
  // home defaults to $HOME; denyRead adds to the credential denylist
});
const cmd = new Deno.Command(argv[0], { args: argv.slice(1), cwd: workspace, ... });
```

- The profile is **deny-write except workspace + a curated toolchain allowlist**
  (caches, `/private/tmp`, `~/.cargo`, `~/.npm`, …) and **allow-read except a
  credential/secret denylist** (`~/.ssh`, `~/.aws`, keychains, shell rc/history, browser
  data).
- **Network is intentionally NOT restricted here** — egress is owned by the Claw
  Patrol layer (`src/net/**`). This module is the filesystem/process half only.
- The profile travels inline (`sandbox-exec -p <profile>`), so there's no temp file
  and concurrent sessions don't race.
- macOS-only. On other platforms, don't wrap (or gate on `Deno.build.os === "darwin"`).

## 2. jj — session lifecycle for repo work

One jj bookmark per session (`bough/<sessionId>`), colocated with the existing git repo.

| Session event | Call |
|---|---|
| session created (root) | `ensureWorkspace(repo, sessionId, base?)` — new change off git HEAD (or `base`) |
| session resumed | `ensureWorkspace(repo, sessionId)` — idempotent; switches the working copy to it |
| session forked | `forkSession(repo, fromId, toId)` — new change off the source tip, then diverges |
| render Changes tab | `diff(repo, sessionId)` → `Diff` (source: `"jj"`) |
| revert | `undo(repo)` (last op) or `operations(repo)` + `restore(repo, opId)` |

- `base` is the commit a new session branches from. The `sessions` row is the natural
  home for it (store the repo's HEAD at attach time and pass it back on resume/fork);
  default is `git rev-parse HEAD`, falling back to the jj root for an empty repo.
- `diff()` snapshots first (jj auto-snapshots on any command) and returns the
  **change-vs-its-parent** diff — exactly what the session changed since it branched.
  If a session ever accumulates multiple internal jj commits, switch to a
  `--from <base> --to <bookmark>` range; the single-change form covers the normal flow
  where all edits land on one working-copy change.
- **Caveat:** `forkSession` makes the fork a child of the source. Editing the source
  again after forking rebases the fork onto the new tip. For a frozen fork, branch off
  the source's parent (or `jj duplicate`) instead — revisit if the UI needs it.

## 3. clonefile — non-git config

For paths outside a repo. The agent edits **clones**, not the originals.

```ts
import { snapshotPaths, diff, applyBack, sessionDir } from "../vcs/clonefile.ts";

await snapshotPaths(sessionId, ["/Users/x/.zshrc", "/Users/x/.config/nvim"]); // cp -c clones
// ... agent edits the clones under sessionDir(sessionId) ...
const d = await diff(sessionId);          // Diff (source: "clonefile"); paths are the originals
await applyBack(sessionId, approvedPaths); // copy approved clones back over originals
```

- **`sessionDir(sessionId)` must be in the seatbelt `allowWrite` list** (seam #1) so the
  sandboxed agent can write the clones. This is the one cross-module dependency.
- Default snapshot root is `~/.bough/snapshots/<sessionId>`; pass a `base` arg to
  override (tests do, to avoid touching real config).
- `applyBack` clones approved files back (`cp -c`) and honors deletions (a file the agent
  removed from the clone is removed from the original).
- macOS/APFS only (`cp -c` clonefile).

## 4. The `Diff` contract (Changes tab)

`src/schema/changes.ts`:

```ts
Diff     = { source: "jj" | "clonefile", files: FileDiff[] }
FileDiff = { path: string, status: "added" | "modified" | "deleted", hunks: Hunk[] }
Hunk     = { header: string, lines: string[] }   // "@@ … @@" + body lines with ` `/`+`/`-` markers
```

Both sources produce byte-identical structure via `parseGitDiff()`. The UI's Changes rail
renders a `Diff` and calls the matching apply path per file/hunk: jj → `undo`/`restore`,
clonefile → `applyBack(sessionId, approvedPaths)`. Renames surface as delete + add
(git default without `-M`); binary/empty files yield a `FileDiff` with no hunks.

## Permissions

All three shell out, so the server and tests need `--allow-run` (already present in the
`dev`/`test` tasks in `deno.json`). The subprocess tests self-skip when run permission is
absent, so they never turn the suite red under a reduced permission set; they execute for
real when `--allow-run` is granted. Required binaries on PATH: `jj` (>= 0.42, `brew install jj`),
`git`, `cp`, `/usr/bin/sandbox-exec`.
