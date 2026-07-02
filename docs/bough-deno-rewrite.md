# bough — Deno rewrite design

> A from-scratch rewrite of bough in **Deno**, shipped as a **`deno desktop`** app. Keeps the four
> pillars (worker/supervisor, tree-history + branching, single-server web UI, network sandboxing)
> plus an **always-sandboxed, fully-snapshotted filesystem**. Decisions below; spike evidence is in
> project memory.

## Stack

| Concern | Choice |
|---|---|
| Runtime + packaging | **Deno 2.9+** as a **`deno desktop`** app — single binary, OS webview, in-process UI↔backend |
| Server | `Deno.serve` + OpenAPI (`/doc` → generated SDK); also listens for remote/TUI clients |
| Streaming | SSE over an in-process event bus |
| DB | **SQLite** (Drizzle) — sessions, messages, snapshots, net_events |
| Schema | Zod |
| LLM | `npm:@anthropic-ai/sdk`, default Claude Opus 4.8 |
| Web UI | Vite + React; tree via react-flow; consumes the SDK + SSE |
| Network + creds + gating | **Claw Patrol** |
| Filesystem enforcement | **macOS Seatbelt** + Deno permissions |
| Snapshot / review / branching | **jj (Jujutsu)** for repos; **APFS `clonefile`** for non-git config |
| Supervision | Persisted, resumable state machine (checkpoint per step to SQLite) |

Sessions/branching/worker-loop follow opencode's TypeScript model.

## Network sandboxing — Claw Patrol

- **Capture at L3** (WireGuard or macOS network extension): the agent dials hosts directly, no proxy
  env or CA trust — catches raw-socket / proxy-ignoring tools (npm/undici).
- **Protocol-aware gating** via CEL rules in HCL: HTTPS (`http.method`/`path`/`body_json`), k8s
  (`k8s.verb`/`resource`/`namespace`), Postgres (`sql.verb`/`tables`), SSH. GitHub is gated on the
  GraphQL **operation** (body), not method. **Human-approval verdict** = the hold-and-ask gate.
- **Credential injection**: real creds stamped on the wire (agent never holds them); AWS SigV4 via
  `clawpatrol-plugin-aws`. Deployment: single-host loopback WireGuard.
- bough writes the HCL and reads the event stream (the network side-pane). `policy.py`'s read/write
  matrix → CEL rules; `clawpatrol test` is the CI regression harness.

### Policy bundles (installable, publishable)

A **bundle** is a shareable, parameterized Claw Patrol policy for a service or tool — endpoints +
rules + credential handles + sane defaults, with typed parameters (hosts, which creds to connect,
allow/deny knobs). Generalizes today's per-provider capabilities into a community format.

- **Discover & install:** `bough net add <bundle>` pulls from a registry (JSR or a git-indexed
  list), surfaces the bundle's parameters as a form, composes it into the gateway HCL, runs
  `clawpatrol validate` + `clawpatrol test` against the bundle's fixtures, and hot-reloads.
- **Publish:** a bundle is an HCL template + manifest (params, description, required creds, test
  fixtures); anyone can publish one (e.g. `github`, `aws-readonly`, `kubernetes-prod`).
- Clean config = the bundle declares its knobs; bough renders them — no hand-edited HCL.

## Filesystem — always sandboxed

Always run sandboxed. Everything the agent touches is snapshotted first, so full isolation costs
nothing and there's no risky "direct host" mode. Two decomposed jobs:

- **Enforcement** ("some paths read-only, rest writable"): **Seatbelt** (`deny file-write*` except
  workspace + allowlist) + Deno `--allow-read`/`--deny-write`. Kernel-enforced, no mount/lock.
- **Snapshot / review / surgical apply**:
  - **Repo work** → **jj**: auto-snapshots every command, op-log undo, native branching,
    git-compatible. Also serves the branching pillar (below).
  - **Non-git config** (`~/.zshrc`, `~/.config`) → **APFS `clonefile`** copy → agent edits the clone
    → `git diff --no-index` review → copy approved files back.

Because every change is captured and reversible, the agent can run unattended and any edit — repo or
global config — lands only after review→apply.

## Tree history + branching

- Conversation tree: `sessions(id, parent_id, …)`; messages as typed parts
  (text/tool_call/tool_result/reasoning). FS branch = a **jj** change/branch pinned to the session
  node; forking a session forks the jj branch. `net_events(…)` = the live network feed.
- bough grows **many heads** — forks, compaction branches, worker branches — so history is shown two
  complementary ways (see UI): a readable per-head thread, and a spatial map across all heads.
- **Compaction is a branch.** Compaction points are shown as markers. Highlight a span of turns →
  "compact" replaces them with a summary on a **new branch** (same mechanism as forking), so the
  original history is preserved and you can compare compacted vs full.

## UI design

**Personas that set the bar.**
- **Driver** — runs bough on their own machine all day. Wants minimal chrome, one-key branching,
  glanceable status, and confidence that nothing reaches the network or touches files without a say.
- **Reviewer** — the same person returning after an unattended run. Needs to scan *what happened*
  across many heads and approve or revert in bulk.

**Principles.** Restraint over decoration; everything the agent did is visible and reversible;
approvals are first-class but never block until they must. With many heads, keep a *readable* view
(one head at a time) distinct from the *spatial* view (all heads).

**Aesthetic.** Clean neutral-dark, a single green accent (live / allowed / approve), IBM Plex Sans;
deny/danger a muted red used sparingly. Continues the current "Bough" look.

**Layout — one window.**
- **Left — history & heads.** The current head's conversation as a readable thread/outline, plus a
  switchable list of heads (branches) — move between the many heads without opening the map.
- **Center — active conversation.** The live thread; worker/tool activity folds inline (collapsed by
  default). The focus surface — quiet by default.
- **Right — context rail (collapsible tabs).** *Network* (live Claw Patrol feed + pending
  approvals); *Changes* (jj / clonefile diff → review/apply per file or hunk). Pending pulses the
  green accent; nothing else competes for attention.
- **Map — separate, expandable.** The full branching graph of *all heads*, expanded from the side
  (not just an overlay): zoom whole-tree → single-turn, compaction markers, highlight-a-span →
  compact-to-new-branch, click a node to jump or fork.

Two views of the same history: readable **per-head on the left**, spatial **across-heads in the map**.

## Worker / supervisor

CHECK-gated best-of-N ladder. Supervisor loop is a persisted state machine (checkpoint to SQLite
per step, resume on restart). Parallel workers via Deno Workers / subprocesses.

## Reuse from current bough

`seatbelt.gleam` (enforcement); `policy.py` matrix → CEL rules (+ `test_policy.py` → `clawpatrol
test`); `containment_probe.sh` → integration test; worker ladder; `net_audit` event shape.

## Open

- `deno desktop` maturity (experimental) — fallback: plain `Deno.serve` + browser.
- Claw Patrol credential *injection* (needs the NE session socket) + AWS/kubectl live path.
