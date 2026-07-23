# Sandbox + snapshots — integration seams

How the sandbox and snapshot modules plug into the server. Nothing here imports the server; the
server imports these.

Modules:

- `src/sandbox/vm.ts` + `src/sandbox/vmsession.ts` — per-session smolvm micro-VM sandbox.
- `src/vcs/shadow.ts` — shadow-git per-session snapshots/branching for **repo** work
  (docs/shadow-snapshots.md).
- `src/vcs/guestgit.ts` + `src/vcs/gitgateway.ts` + `src/vcs/mirror.ts` — the guest-owned workspace:
  in-guest clone, host smart-HTTP store gateway, read-only host mirror
  (docs/guest-owned-workspace.md).
- `src/vcs/clonefile.ts` — APFS clonefile snapshots for **non-git config** (`~/.zshrc`,
  `~/.config`).
- `src/schema/changes.ts` — the `Diff` contract shared by the snapshot sources (the Changes tab).

## 1. VM sandbox — the exec + workspace boundary

Active when `sandboxVm()` (BOUGH_SANDBOX_VM=1) and a golden rootfs exists
(`scripts/guest-image/build-golden.sh`; `bough update` rebuilds it on input drift). One persistent
smolvm machine per session (`ensureVm`), booted from the golden with egress locked to the gate host
IP (`--allow-cidr <gateHostIp>/32`) — the guest can reach the Claw Patrol proxy and the git store
gateway there, and nothing else. Machines persist across server restarts: `ensureVm` reuses an
existing machine (status → start) and re-stamps the store-gateway remote + token; session archive
tears the machine down after flushing unpushed work (`teardownVm`).

Two workspace modes, chosen by the origin:

- **Git origin (guest-owned)**: no host mount. The working copy is a real clone at `/workspace/repo`
  (`GUEST_REPO`) on the guest's persistent ext4, bootstrapped from the session's shadow store
  through the git gateway (`guestgit.bootstrapClone`). Snapshots are guest pushes (`guestTrack`:
  add/commit/push to `refs/bough/sessions/<id>`); the host reads the store, never the guest
  filesystem. Host-side read consumers (@ picker, AGENTS.md, LSP) use the read-only mirror checkout
  (`mirror.refreshMirror`), fresh as of the last push.
- **Non-git origin dir**: the origin is virtiofs-mounted rw at `/workspace` (`GUEST_WORKSPACE`),
  unchanged; clonefile snapshots cover config edits.

Exec: bash/oracle/bash_bg run via `execCommand`/`execIn` (`smolvm machine exec`), cwd `GUEST_REPO`
(git) or `GUEST_WORKSPACE` (non-git). The per-turn proxy/CA env from `net/gateway.ts envFor` is
injected per exec call; NO_PROXY carries the gate IP so store-gateway git traffic bypasses the proxy
(snapshot pushes are the snapshot mechanism, not egress — the review rail via ship/materialize stays
the only path to the origin, host-side). In-process file tools route through
`vm.readFile`/`vm.writeFile` with guest-path confinement (the `guestFs` seam on ToolRunCtx).

Fallback: without BOUGH_SANDBOX_VM=1 or without a golden, sessions run unsandboxed in the host
worktree world (bash.ts fallback) — the shadow worktree machinery in §2 remains that path's
foundation.

## 2. shadow — session lifecycle for repo work

One shadow git repository per origin directory (`~/.bough/shadow/<name>-<hash>`), holding every
session's snapshot history on `refs/bough/{sessions,base,originbase}/<id>`. In the host (no-VM)
world every session runs in its own linked worktree of the shadow repo under
`~/.bough/workspaces/<sessionId>`, branched off a captured snapshot of the origin's working tree
(uncommitted + untracked included). In VM mode `createSessionWorkspace`/`addWorkspace` run with
`{ worktree: false }` — refs + base capture only; the working copy is the guest clone and
`~/.bough/workspaces/<sessionId>` is the read-only mirror. The origin's checkout — HEAD, branch,
index, `git status` — is never modified. `prepareShadow` in `src/supervisor/workspace.ts` wires the
session lifecycle to it.

| Session event                     | Call                                                                                |
| --------------------------------- | ----------------------------------------------------------------------------------- |
| session created                   | `createSessionWorkspace(origin, sessionId)` — worktree off a working-tree snapshot  |
| session resumed                   | nothing — refs + machine persist; VM mode re-stamps the gateway remote              |
| session forked / subagent spawned | `addWorkspace(parentDir, toId, dir, fromSessionId)` — branched off the parent's tip |
| render Changes tab                | `diff(dir, sessionId)` → `Diff` (source: `"shadow"`), always base..tip              |
| apply                             | `materialize(dir, id, origin, paths)` + `accept(dir, id, msg)` when all covered     |
| revert                            | `revertPaths(dir, id, paths)` / `undoAll(dir, id)`; VM mode: `guestRevert`          |
| adopt (subagent)                  | `adoptChanges(parentDir, subDir, fromId, intoId)`; VM mode: `guestAdopt`            |

- `track()` (snapshot) staples the worktree to the session tip ref on every diff/apply/revert; a
  failed `git add` is fatal, never swallowed. VM mode's analog is `guestTrack` (guest push).
  Restores only ever touch explicit path lists.

## 3. clonefile — non-git config

For paths outside a repo. The agent edits **clones**, not the originals.

```ts
import { applyBack, diff, sessionDir, snapshotPaths } from "../vcs/clonefile.ts";

await snapshotPaths(sessionId, ["/Users/x/.zshrc", "/Users/x/.config/nvim"]); // cp -c clones
// ... agent edits the clones under sessionDir(sessionId) ...
const d = await diff(sessionId); // Diff (source: "clonefile"); paths are the originals
await applyBack(sessionId, approvedPaths); // copy approved clones back over originals
```

- Default snapshot root is `~/.bough/snapshots/<sessionId>`; pass a `base` arg to override (tests
  do, to avoid touching real config).
- `applyBack` clones approved files back (`cp -c`) and honors deletions (a file the agent removed
  from the clone is removed from the original).
- macOS/APFS only (`cp -c` clonefile).

## 4. The `Diff` contract (Changes tab)

`src/schema/changes.ts`:

```ts
Diff     = { source: "clonefile" | "shadow", files: FileDiff[] }
FileDiff = { path: string, status: "added" | "modified" | "deleted", hunks: Hunk[] }
Hunk     = { header: string, lines: string[] }   // "@@ … @@" + body lines with ` `/`+`/`-` markers
```

Both sources produce byte-identical structure via `parseGitDiff()`. The UI's Changes rail renders a
`Diff` and calls the matching apply/revert path: shadow apply → `materialize` + `accept` (seal: base
advances onto tip), shadow revert → `revertPaths`/`undoAll` (VM: `guestRevert`), clonefile apply →
`applyBack(sessionId, approvedPaths)`. Renames surface as delete + add (git default without `-M`);
binary/empty files yield a `FileDiff` with no hunks.

## Permissions

Everything shells out, so the server and tests need `--allow-run` (already present in the
`dev`/`test` tasks in `deno.json`). The subprocess tests self-skip when run permission is absent, so
they never turn the suite red under a reduced permission set; they execute for real when
`--allow-run` is granted. Required binaries on PATH: `git`, `cp`; VM mode additionally needs
`smolvm` (or `BOUGH_SMOLVM_BIN`) and a golden rootfs.
