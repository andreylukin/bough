# Guest-owned workspace — implementation plan

Synthesized 2026-07-23 from six mapper/spike reports (sandbox, shadow-git, consumers, net,
delete-list, live spike). All file:line refs are against this worktree (HEAD 3cfdd40).
The next workflow implements directly from this doc.

---

## 1. Verdict

**Viable — but NOT via the mounted-bare-repo push path.** The live spike proved the core
mechanism (guest clone from a bare repo in 0.88s for a 191MB pack, native-speed in-guest git
on the persistent /dev/vda ext4 already mounted at `/workspace`, single-writer push landing
host-side in 0.09s with clean fsck). But it **refuted mounted-store push under concurrency**:
in a symmetric two-VM race, 13/15 pushes failed on both sides with
`remote: fatal: bad object refs/heads/<other>` — guest-side receive-pack sees the other
writer's ref file through virtiofs before its objects, so git's connectivity check rejects
the push. Never corruption, but persistently failing pushes for the duration of contention.
Shadow stores are **per-origin, shared across sessions** (storeDirFor, src/vcs/shadow.ts:161-169),
and the host writes refs into them continuously (accept :551-554, adopt :543, captureBase
:317-319) — so multi-writer is the normal case, not the edge case. The spike's own verdict:
"if concurrent sessions share one shadow store, push-via-gateway is required."

Additionally, mounted-store **clone** is broken for real stores regardless of the race: the
store's object db is incomplete without `objects/info/alternates → <origin>/.git/objects`
(shadow.ts:245-257), an absolute host path unresolvable in-guest — every grafted base
commit's parent chain is missing (vcs mapper, verified on disk). The spike cloned a
self-contained bare copy, not a real alternates-bearing store, so its 0.88s number is the
happy path only.

**Chosen fallback: a host-side git smart-HTTP endpoint ("store gateway") serving each
session's shadow store on the gate host IP.** Both fetch and push. This solves all four
problems at once:

- **receive-pack runs on the host** against the authoritative filesystem — no virtiofs
  staleness, and it can be serialized with the host's ref writers via the existing
  per-store `withLock` (shadow.ts:296-303).
- **upload-pack runs on the host** — alternates resolve natively, so the history graft
  transfers; `--depth`/filter options bound the transfer for huge origins.
- **no host-owned path is ever touched by guest git** — `safe.directory` and the
  virtiofs locking questions disappear from the git path.
- **reachability is free**: the VM's egress is `--allow-cidr <gateHostIp>/32` on ALL ports
  (vm.ts:120-122, gatehost.ts:12-21), so a listener on the gate IP needs no policy change.
  Store traffic goes **direct** (gate IP added to NO_PROXY), deliberately bypassing the
  Claw Patrol proxy: pushing snapshots to bough's own store is the snapshot mechanism, not
  egress — the review rail (ship/materialize) remains the only path to the origin, and it
  stays host-side (turn.ts:848-866). Routing store traffic through the proxy would classify
  UNKNOWN and hold every push in review mode (policy.ts:393-397, 503-507) — wrong layer.

Clone destination: `/workspace/repo` on the guest's persistent ext4 (spike discovery: the
golden guest already mounts a 20G `/dev/vda` at `/workspace`; today bough's virtiofs
workspace volume overmounts it — drop the volume and the disk is simply there. Zero image
changes for the disk).

Second refuted assumption, from the spike: **git identity must be injected** or every
ref/reflog-writing git op in-guest stalls a flat 5.0s on a DNS timeout (ident auto-detection
resolves hostname "container" against unreachable 8.8.8.8) and `git commit` fails outright.
Fix is baked identity + `/etc/hosts` entry (§3 build-golden.sh) — matches the store's pinned
snapshot identity `bough <bough@localhost>` (shadow.ts:43-44,223-224) so guest commits are
byte-compatible with host `track()` commits.

`smolvm machine fork` stays out of scope: it requires a `--forkable`-started golden that
**freezes while clones exist** (spike), so it cannot branch a live session; subagents clone
from the store into fresh VMs instead. The fork() stub stays rejecting (vm.ts:298-310).

**Non-git origin dirs keep the current mount path unchanged** (workspace.ts:125-137: real
dir mounted rw, clonefile snapshots only) — there is no store to clone for that class, so
the virtiofs mount machinery and golden `safe.directory` survive for it (narrowed rationale,
§4). The no-VM fallback (BOUGH_SANDBOX_VM=0 / no golden, bash.ts:72-74) keeps the existing
host-worktree world end-to-end; guest-owned is a VM-mode branch, not a replacement, until
the fallback is retired.

---

## 2. Architecture — session start to ship

Mode flag: guest-owned engages when `sandboxVm()` **and** the origin is a git/jj repo
(the same gate as prepareShadow today, workspace.ts:125-127). Everything below is that
branch; non-VM and non-git flows are unchanged.

**Session start (first turn)**
1. `prepareWorkspace` (workspace.ts:94-141) resolves the ORIGIN dir. The session
   `workspace` column now permanently stores the **origin** path, never a worktree path
   (semantic change; migration in §3 db.ts). `workspaceProblem`'s stat (:113-114) checks
   the origin — fine.
2. `prepareShadow` → `createSessionWorkspace` does everything it does today **except**
   `git worktree add` + `startHydration` (shadow.ts:321-322): ensureStore, captureBase,
   set `refs/bough/{sessions,base,originbase}/<id>` (:311-319). No host worktree, no
   hydration (cross-arch clonefile of darwin node_modules into a linux guest was already
   broken; the agent installs deps in-guest).
3. `ensureVm` creates the machine **without the workspace volume**, then runs the guest
   bootstrap via `execIn`: `git clone --single-branch <storeGatewayUrl> /workspace/repo`
   is wrong-shaped (refs/heads is empty, HEAD unborn — vcs mapper; shadow.ts:208), so the
   bootstrap is explicit:
   ```
   git init /workspace/repo
   git -C /workspace/repo remote add origin http://<gateIp>:<gitPort>/git/<sid>
   git -C /workspace/repo fetch origin +refs/bough/sessions/<sid>:refs/remotes/origin/session \
       +refs/bough/base/<sid>:refs/bough/base +refs/bough/originbase/<sid>:refs/bough/originbase
   git -C /workspace/repo checkout -B work refs/remotes/origin/session
   git -C /workspace/repo config http.extraHeader "Authorization: Bearer <sessionToken>"
   ```
   Guest cwd for bash becomes `/workspace/repo`.
4. Store gateway (new, §3): smart-HTTP stateless-rpc on the gate IP; per-session bearer
   token minted at ensureVm; receive limited to `refs/bough/sessions/<sid>` by a
   pre-receive hook written by ensureStore; receive wrapped in `withLock(store)`.

**During a turn**
- bash/oracle/bash_bg exec in-guest with cwd `/workspace/repo`; per-turn proxy/CA env via
  `-e` exactly as today (bash.ts:65-69, vm.ts:155-161). NO_PROXY gains the gate IP.
- File tools (read/write/edit) route through `vm.readFile`/`vm.writeFile` (vm.ts:212-259)
  with guest-path confinement — the agent's own writes are read-your-writes consistent.
- **Snapshot = guestTrack**: the replacement for `track()` (shadow.ts:453-476) is
  `execIn(sid, sh -c "git add -A && git commit -qm snapshot --allow-empty-message || true; git push -q origin HEAD:refs/bough/sessions/<sid>")`
  run from the host at the same call sites track() has today (diff, spawn, adopt,
  materialize, ship, revert) plus on `turn.finished` (crash-loss bound). After the push,
  every store-side op (diff base..tip, blobAt, materialize, accept) works unchanged with
  `--git-dir=<store>`.
- **Mirror checkout** (new, read-only): after each received push, the host refreshes
  `~/.bough/workspaces/<sid>` as a plain checkout of the session tip
  (`git --git-dir=<store> --work-tree=<dir> checkout -f refs/bough/sessions/<sid> -- .`,
  host never writes it otherwise). This keeps the whole read-side consumer family —
  @ picker, @file expansion, image attachments, AGENTS.md, LSP/leta, stdio-MCP cwd —
  working with near-zero per-consumer code change, at "fresh as of last push" semantics.

**Changes rail / apply / revert**
- `sessionChanges` (changes.ts:82-121): if the VM is live, guestTrack first, then store
  diff; if archived/dead, store diff of the last-pushed tip.
- apply = materialize+accept, host-side, unchanged post-push (shadow.ts:683-722, 551-554).
- revert = in-guest `git checkout <base> -- <paths>` via execIn (VM must be live; archived
  sessions can't revert — same as they can't run bash).

**Subagents / fork / handoff / extract**
- Spawn: guestTrack(parent) → `addWorkspace` sets refs off the pushed parent tip
  (shadow.ts:334-366 minus worktree add) → child VM bootstraps its own clone.
- Adopt: guestTrack(sub) → in the PARENT guest:
  `git fetch origin +refs/bough/sessions/<sub>:tmp && git diff <subBase> tmp | git apply --3way`
  (mirrors adoptInner shadow.ts:525-544) → guestTrack(parent) → host advances sub base ref.
- fork/handoff/extract (fork.ts:90-91, handoff.ts:77-78, extract.ts:53-54) copy the
  **origin** path; handoff/extract's accidental nested-store bug (createSessionWorkspace
  keyed on a worktree path) is fixed for free.

**Ship**
- `shipToOrigin` (shadow.ts:749-832): replace its `track` (:755) with guestTrack;
  everything else (materialize plan vs immovable originbase, throwaway-index commit, CAS,
  accept, push) is store+origin-side and unchanged. ship stays a host-side bridged fn
  (turn.ts:848-866) — the guest never gains origin write access.

**Lifecycle (machines stop being disposable)**
- ensureVm: replace `remove()`-then-create (vmsession.ts:97) with `status()` →
  `start()` if the machine exists (vm.ts:267-296 stop/start persist state); create only
  when absent. Drop the recreate-on-workspace-change path (:89-91) — the workspace no
  longer identifies the machine's mounts.
- Archive: `teardownVm` (main.ts:60-62) becomes flush-then-delete: guestTrack (best-effort)
  → `machine delete --force`. Post-archive diff/ship run store-only.
- Server restart: machines persist; reattach = status/start + re-stamp the remote URL and
  token (`git config` via execIn) since the git port may differ across server runs.

---

## 3. File-by-file changes

**NEW `src/vcs/gitgateway.ts`** — store gateway. `Deno.serve` bound on `gateHostIp()`
(random port, held for server lifetime; exported `gitGatewayUrl(sid)`).
Routes: `GET /git/<sid>/info/refs?service=git-{upload,receive}-pack` and
`POST /git/<sid>/git-{upload,receive}-pack`, implemented by spawning
`git {upload,receive}-pack --stateless-rpc [--advertise-refs] <store>` with request body
piped to stdin (standard smart-HTTP v0; no http-backend CGI needed). Auth: constant-time
compare of `Authorization: Bearer <token>` against the per-session token map (minted in
vmsession, crypto.randomUUID). Store resolved from sid via db originDir → storeDirFor.
receive-pack wrapped in `shadow.withLock(store)` (export it); on successful receive of
`refs/bough/sessions/<sid>`, refresh the mirror (below) and emit `changes.updated`.
Env for spawned git: ISOLATED (GIT_CONFIG_GLOBAL/SYSTEM=/dev/null, shadow.ts:107).

**NEW `src/vcs/guestgit.ts`** — guest-side git driver: `bootstrapClone(sid)` (init/remote/
fetch/checkout/config sequence from §2, incl. `git config user.name bough`,
`user.email bough@localhost`, `http.extraHeader` token), `guestTrack(sid)` (add/commit/push,
non-fatal empty), `guestRevert(sid, base, paths)`, `guestAdopt(parentSid, subSid, subBase)`.
All via `execIn`. Exports `GUEST_REPO = "/workspace/repo"`.

**NEW `src/vcs/mirror.ts`** — `refreshMirror(sid)`: checkout-f of the session tip into
`workspaceDirFor(sid)` (path reuse is deliberate — read-side consumers keep working) +
`git clean`-equivalent via `checkout -f … -- .` plus removal of files deleted at tip
(use `git --git-dir --work-tree checkout -f <tip> -- .` after `read-tree`+`checkout-index`,
or simplest: `rm -rf`+full checkout for v1, it's read-only). Called by the gateway on
receive and by prepareShadow after captureBase (initial mirror = base tree).

**`src/vcs/shadow.ts`**
- Export `withLock`, `storeDirFor` (already exported), keep ensureStore config pins.
- ensureStore: additionally write `hooks/pre-receive` (refuse any ref other than
  `$BOUGH_RECEIVE_REF`, passed via env by the gateway) and `chmod 755`.
- createSessionWorkspace (:311-325): add `{ worktree?: boolean }` opt — guest-owned mode
  skips `git worktree add` + `startHydration`, returns the store path + base sha.
- addWorkspace (:334-366): same split; its `track(fromDir)` (:349) becomes a caller-provided
  snapshot step (guestTrack) in VM mode.
- Store-side variants for dir-cwd git calls: `diff`, `blobAt` (:573-583), `deliveryBase`,
  `accept` already only need the git dir — add a `gitDirOf(sid)` resolution path so they
  run with `--git-dir=<store>` and stop requiring a worktree cwd. `originRepo` (:561-570)
  gains a DB-based resolver `storeForSession(sid)` (originDir → storeDirFor) used by all
  VM-mode callers (changes.ts:70, turn.ts:853, workspace.ts:164).
- `track()` itself stays for non-VM mode; VM-mode call sites switch to guestTrack.
- adoptChanges/revertPaths/undoAll: VM-mode variants delegate to guestgit.ts.
- Hydration (:383-443): not called in VM mode; unchanged otherwise.

**`src/sandbox/vmsession.ts`**
- ensureVm: guest-owned branch — no workspace mount (`mounts: []` for git origins;
  keep `{host, guest: GUEST_WORKSPACE}` for non-git origin dirs); status/start-reuse
  instead of remove-first (:97); after create: startBrokerBridge + bootstrapClone;
  after reattach: re-stamp remote URL + token. Mint + register the session token with
  gitgateway. `VmHandle.workspace` becomes `VmHandle.origin` (identity only, no recreate).
- New export `GUEST_REPO` re-export; `teardownVm`: guestTrack flush before delete.
- Fix stale header doc (:1-9, :26-30 — "one workspace" invariant and seatbelt-cutover
  language are both obsolete).

**`src/sandbox/vm.ts`** — CreateOpts: add optional `storageGiB` passthrough
(`--storage`) so large repos aren't capped by defaults (net mapper risk). No other change;
readFile/writeFile (:212-259) get their first production consumers.

**`src/tools/bash.ts`** — VM branch (:65-69): cwd `GUEST_REPO` for git origins
(GUEST_WORKSPACE for non-git); drop `cwd: ctx.workspace` from the host spawn (:113) —
host cwd is meaningless for a smolvm-exec child and throws NotFound once the worktree is
gone (consumers mapper). Same fix in bash_bg.ts:273 and oracle.ts:71. Rewrite the stale
Seatbelt comment block (:37-48).

**`src/tools/read_file.ts` / `write_file.ts` / `edit_file.ts` / `types.ts`**
- Add a `guestFs` seam on ToolRunCtx (types.ts): when set, read/write/edit route through
  vm.readFile/vm.writeFile with guest-path confinement (normalize + must-be-under
  `/workspace/repo`, plus scratch); `resolveInWorkspace` (types.ts:181-215) stays for
  host mode. run_steps.ts:118-131 wiring unchanged (tools branch internally).
- Perf note: base64-over-argv at 96KiB chunks is acceptable for v1; if latency bites,
  add a `machine exec` stdin/stdout raw mode later (open question §7).

**`src/net/gateway.ts`** — envFor VM branch (:384-396): append gate IP to NO_PROXY so
guest git reaches the store gateway direct. Everything else unchanged (github push via
proxy+bundle already works: bundles.ts:181-210).

**`src/supervisor/workspace.ts`**
- prepareWorkspace: in VM mode keep `cwd` = origin; do NOT repoint the workspace column
  at a worktree (:196 deleted in this branch); sessionDir/scratchDir unchanged (host-side;
  scratch stays host-tmp — pre-existing inconsistency, not widened here).
- prepareShadow (:152-209): VM branch calls the no-worktree createSessionWorkspace, then
  refreshMirror. Fork branch uses parent guestTrack + addWorkspace(no-worktree).
- Degrade path (:201-208) unchanged.

**`src/db/db.ts`** — migration for legacy rows: any `workspace` under
`~/.bough/workspaces/` is rewritten to the session's `originDir` (db.ts:381,489) at
startup; log what was rewritten. Column docstring updated (:504-517).

**`src/server/changes.ts`** — `hasShadowWorkspace` (:67-71) → storeForSession;
sessionChanges (:82-121): guestTrack-if-live before diff; revert route (:197-211) →
guestRevert. Subagent sections diff store-side.

**`src/server/app.ts` / `files.ts`** — @ picker (:315-318, files.ts:57-86),
expandFileReferences (files.ts:226-248), collectImageAttachments (:284-318): all read the
**mirror** — zero code change needed beyond ensuring the mirror path == the old worktree
path; verify no writes sneak in.

**`src/supervisor/prompt.ts`** — workspaceNote (:479-483) prints `/workspace/repo` in VM
mode (fixes today's host-path/guest-cwd mismatch); readAgentsFile (:521-540) reads the
mirror. Rewrite ship-note (prompt/ship-note.md:15-22 + SHIP_NOTE_BUILTIN :267-275): the
"shared refs are write-denied" language is dead; the guest clone's refs are private, local
git branch/stash are now fine; `git diff refs/bough/originbase` works because the bootstrap
fetched it (§2).

**`src/mcp/manager.ts` (:196-208) / `src/mcp/lsp.ts` (:120-170)** — cwd/workspace = the
mirror path (freshness = last push; acceptable v1; note as degradation §7).

**`src/subagent.ts`** — spawn (:264-311): guestTrack(spawner) before addWorkspace;
adoptSubagent (:469-477) → guestAdopt. changedFiles (:212) store-side.

**`src/turn.ts`** — :639 toolCtx gains guestFs; :1128 awaitHydration skipped in VM mode;
turn.finished hook adds guestTrack flush; ship wiring (:853) uses storeForSession.

**`src/server/main.ts`** — construct gitgateway at boot (VM mode only); teardown order:
flush → delete (:60-62 handler).

**`scripts/guest-image/build-golden.sh`**
- Bake identity: `git config --system user.name bough; git config --system user.email
  bough@localhost` next to safe.directory (:116-120) — kills the 5s ident-DNS stall and
  the commit failure (spike trap).
- Add `echo "127.0.0.1 container" >> /etc/hosts` belt-and-suspenders for any other
  hostname-resolving tool under egress lockdown.
- KEEP safe.directory '*' (non-git origin dirs still mount host-owned paths); narrow the
  comment. KEEP /workspace mkdir (:122-127) — harmless, and non-git mode still mounts there.
- Bump verify block to assert the identity config (:243-258 pattern).
- Golden fingerprint (scripts/bough:220-226) picks the script change up automatically.

---

## 4. Delete list

Framing correction from the delete-list mapper (verified): **Seatbelt is already gone**
(d2b296a deleted seatbelt.ts/shims.ts/sandboxGitWriteDirs/gitWriteDirs; this branch is 15
commits past it). Nothing is "gated on seatbelt removal" — that gate is moot. The real
gates are (a) guest-owned landing and (b) the no-VM host fallback's retirement.

**Delete now (gated on nothing):**
- `src/vcs/shadow.ts:229` `maintenance.auto=false` + rationale comment :218-220 — existed
  solely for seatbelt-era in-sandbox `git commit`; host plumbing never triggers
  auto-maintenance. (Keep gc.auto=0/gpgsign/autocrlf/quotepath — pre-sandbox determinism,
  shadow commit 80096a1.)
- `src/sandbox/INTEGRATION.md` §1 (seatbelt wrap(), :15-38) — documents a deleted file.
  Rewrite the doc around the VM + this plan; §2's shadow lifecycle table stays accurate.
- Stale seatbelt comment sweep (comments only): run_steps.ts:6, types.ts:200, oracle.ts:5,
  bash.ts:38/44, workspace.ts:28/51/106, kubeconfig.ts:33/145, proxy.ts:4, gateway.ts:47,
  mcp/manager.ts:3/31/180, mcp/gate.ts:5, mcp/config.ts:25/30, mcp/client.ts:4/76,
  lsp.ts:12, app.ts:986, vm.ts:210, harness/vm.ts:6, clonefile.ts:5, + test comments.
- vmsession.ts:26-30 "opt-in during the seatbelt→VM cutover" doc.

**Delete when guest-owned lands (this workflow):**
- Workspace-volume wiring for git origins: vmsession.ts:23-24 GUEST_WORKSPACE (kept only
  for non-git mode), EnsureOpts.workspace/VmHandle.workspace recreate logic :46-49,:87-92,
  the mount at :102, remove-stale-first :97.
- bash.ts:58-69 virtiofs comments + GUEST_WORKSPACE cwd (git origins), host `cwd:
  ctx.workspace` at bash.ts:113 / bash_bg.ts:273 / oracle.ts:71.
- shadow.ts worktree-add + startHydration in VM-mode paths (:321-322, :362-363) — code
  stays (non-VM fallback), VM-mode call sites stop reaching it.
- prompt ship-note "write-denied refs" language (both copies).
- workspace-column repoint (workspace.ts:196) in VM mode.

**Cannot delete yet (gated on no-VM fallback retirement + non-git origins):**
- Host worktree machinery wholesale (createSessionWorkspace/addWorkspace worktree branch,
  prepareShadow, track, hydration, resolveInWorkspace) — the BOUGH_SANDBOX_VM=0 world and
  machines without a golden (field data: they run unsandboxed) still live on it
  (bash.ts:72-74, main.ts:39-50).
- Golden `safe.directory '*'` (build-golden.sh:116-120) and /workspace mkdir (:122-127) —
  non-git origin dirs still virtiofs-mount host-owned paths.
- scripts/bough:526,538 safe.directory grant/revoke — host agent-uid identity-boundary
  feature, unrelated. Keep.
- vm.ts Mount/--volume machinery — non-git mode still mounts.

---

## 5. Consumer migration table

Routes: **store** = host git op on the store post-push; **guest** = VM exec / vm.readFile-writeFile;
**mirror** = read-only host checkout refreshed on push; **unchanged**.

| Consumer | Ref | Route |
|---|---|---|
| bash / bash_bg / oracle shell | bash.ts:65-69, bash_bg.ts:269-277, oracle.ts:66-82 | guest (cwd /workspace/repo; drop host cwd) |
| read/write/edit file tools | read_file.ts:17, write_file.ts:20, edit_file.ts:20-43 | guest (vm.readFile/writeFile, guest confinement) |
| track() snapshots | shadow.ts:453-476 | guest (guestTrack: add/commit/push) |
| diff / Changes rail | shadow.ts:489-500, changes.ts:82-121 | store (guestTrack first if VM live) |
| apply (materialize+accept) | shadow.ts:683-722,551-554, changes.ts:160-186 | store+origin, unchanged post-push |
| ship | shadow.ts:749-832, turn.ts:848-866 | store+origin, unchanged post-push (host-side) |
| revert / undoAll | shadow.ts:850-874, changes.ts:197-211 | guest (guestRevert; needs live VM) |
| subagent spawn | subagent.ts:264-311, shadow.ts:334-366 | guest push (parent) + store refs + child clone |
| adopt | shadow.ts:525-544, subagent.ts:469-477 | guest (fetch+3way in parent VM) + store ref move |
| originRepo store resolution | shadow.ts:561-570 | store (DB originDir → storeDirFor) |
| @ file picker | app.ts:315-318, files.ts:57-86 | mirror |
| @file expansion / images | files.ts:226-248, 284-318 | mirror |
| AGENTS.md read | prompt.ts:521-540, turn.ts:811 | mirror |
| LSP (leta) | lsp.ts:120-170 | mirror (stale-until-push; accepted v1 degradation) |
| stdio MCP servers | manager.ts:196-208 | mirror cwd (same caveat) |
| hydration | shadow.ts:383-443, turn.ts:1128 | dropped in VM mode (agent installs deps) |
| workspace column | db.ts:504-517, workspace.ts:196 | changed meaning: origin path; startup migration |
| fork/handoff/extract | fork.ts:90-91, handoff.ts:77-78, extract.ts:53-54 | origin path inherit + store refs (fixes nested-store bug) |
| clonefile config snapshots | clonefile.ts | unchanged (non-workspace paths) |
| artifacts, recall, worker consumers, skills, schedules, exec | artifacts.ts:7 etc. | unchanged (verified no workspace fs) |
| non-git origin sessions | workspace.ts:125-137 | unchanged (rw mount + clonefile) |
| no-VM fallback | bash.ts:72-74, main.ts:39-50 | unchanged (host worktree world) |

---

## 6. Test plan

**Rewrite (pin new behavior):**
- `src/sandbox/vmsession.test.ts:33-57` — replace "guest write lands on HOST worktree"
  with: ensure → bootstrap clone exists at /workspace/repo on ext4 (no virtiofs mount for
  git origins); reattach-after-restart reuses the machine (no remove); teardown flushes
  (last commit visible in store after teardownVm).
- `src/tools/bash.vm.test.ts:35-75` — cwd is /workspace/repo; guest `git status` clean and
  instant (identity baked — assert `git commit` succeeds and completes <2s, pinning the
  5s-stall regression); guest write then guestTrack → host store diff shows it.

**New:**
- `src/vcs/gitgateway.test.ts` — host-only (no VM): stateless-rpc round-trip with a plain
  local `git` client against the served store: fetch of refs/bough/* refs incl. alternates-
  grafted parents; push to refs/bough/sessions/<sid> accepted, any other ref refused by
  pre-receive; bad/absent token → 401; concurrent receive vs host accept() serialized by
  withLock (no lost CAS).
- `src/vcs/guestgit.test.ts` (VM-gated like vm.test.ts:16-39) — bootstrapClone against a
  real store WITH alternates (regression for the mounted-clone gap); guestTrack push
  visible in store; adopt fetch+3way between two VMs sharing one store via the gateway
  (the exact topology the spike broke — must pass 15/15).
- `src/vcs/mirror.test.ts` — host-only: mirror reflects tip incl. deletions; read-only.
- migration test: legacy workspace column row under ~/.bough/workspaces rewritten to originDir.

**Keep as-is:** shadow.test.ts (store contract — materialize/ship still rely on it,
delete-list mapper item 12), vm.test.ts mount round-trips (Mount stays for non-git mode),
all non-VM-path tests.

**Live verification (this machine, golden present):**
1. Rebuild golden (identity + hosts entry), confirm fingerprint swap (scripts/bough:283-292).
2. Fresh session on a real repo: first turn boots VM, clones via gateway; `git log` in-guest
   shows grafted origin history; `git commit` <1s.
3. Edit via file tool + via in-guest bash; ^d rail shows both (guestTrack path).
4. Two concurrent sessions on the SAME origin, pushing simultaneously — zero push failures
   (the spike's 13/15 failure case, now through host receive-pack).
5. Subagent spawn + adopt across two VMs; ship to origin end-to-end incl. github push
   through the proxy (bundle hold: two approval cards, gate.ts:105-106).
6. Server restart mid-session → machine reattaches, uncommitted guest work intact.
7. Archive → flush → store diff still renders.

---

## 7. Risks & open questions

- **Unpushed-work loss window**: guest work between snapshots dies with a crashed/deleted
  VM. Mitigated by guestTrack on turn.finished + flush-on-archive; a mid-turn crash still
  loses the tail. Open: a periodic in-guest auto-commit daemon?
- **smolvm machine persistence across host reboots** unverified (stop/start proven within
  a server lifetime; spike didn't reboot). Verify before relying on reattach; fallback is
  re-clone from the last-pushed tip (cheap, but loses uncommitted files).
- **File-tool latency**: one smolvm exec per read/write/edit, base64-over-argv 96KiB
  chunks (vm.ts:233-259). Fine for code files; big binaries will hurt. Open: raw stdin
  exec mode in smolvm, or a files API over the store gateway.
- **Mirror freshness**: compose-time reads (@refs, AGENTS.md) are stale until the last
  push. guestTrack on turn.finished bounds this to "start-of-turn fresh", which matches
  when those reads happen — but mid-turn LSP queries lag. Accepted v1.
- **Gitignored artifacts never travel**: .env/.env.local/node_modules stayed host-visible
  via the mount; via git they never will. Sessions start cold; secrets need the user to
  provide them in-repo or via a future channel. This is also a deliberate security
  improvement (guest never sees host .env unless committed) — document it.
- **First-fetch size for huge origins**: host-served upload-pack transfers grafted history;
  bound with `--depth 1` + `fetch --deepen` on demand? Open: pick default depth (full for
  bough-sized repos per spike 0.88s; depth for >1GB packs).
- **Two writers remain on the store refs** (gateway receive vs host accept/adopt CAS) —
  serialized by withLock now, but withLock is in-process; a second bough server against the
  same BOUGH_HOME would race. Pre-existing risk, unchanged in kind.
- **Bearer-vs-Basic for github smart-HTTP** unverified live (net mapper) — verify during
  live step 5; fix is a one-line credential template (credentials.ts:33).
- **Oracle read-only still unenforced** (bash.ts:63-64) — unchanged by this work; a ro
  clone in the session VM becomes possible later (git makes ro cheap now).
- **No-PAT sessions**: in-guest `git push` to github without a credential binding 401s
  into a prompt (gateway.ts:98-103) — needs GIT_TERMINAL_PROMPT=0 in guest env so it
  fails fast instead of hanging.
- **Disk sizing**: /dev/vda default 20G; expose `storageGiB` (vm.ts CreateOpts) and pick a
  heuristic (2× origin pack size + 10G) or leave default + document.
- **fork()**: stays a rejecting stub; golden-freeze semantics make it unusable for live
  branching (spike). Revisit only for template fan-out.
