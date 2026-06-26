# bough — Specification

> A sandboxed coding agent with branchable history. Written in Gleam, sandboxed by
> macOS Seatbelt + a per-workspace mitmproxy, structured like opencode (server +
> clients), with closedshell-style live network visibility.

**Status:** draft v0.1 — derived from interview on 2026-06-16.

---

## 1. What bough is

`bough` is a coding-agent harness. Its name carries the two core promises:

- **A bough is a branch.** History is a tree; you can fork any earlier point and
  grow a new branch — *and* the filesystem forks with it.
- **It's safe to leave it growing.** Every agent runs under a kernel-enforced
  macOS Seatbelt sandbox + a per-workspace mitmproxy: workspace-write
  confinement + a default-deny egress allowlist + git-based filesystem snapshots
  + an egress audit feed. You can detach, walk away, and reattach.

It is *not* a wrapper around an existing agent (like closedshell wraps `claude`).
bough implements its own agent loop — and that loop is **supervisor-worker**
(the ReDACT idea, proven in [tent](https://github.com/andreylukin/tent)): a
hosted frontier model **plans and writes** but never touches the machine; a
deterministic harness is the **only** thing that executes — inside the Seatbelt
sandbox — and a small local model patches trivial breakage for free (see §5).

---

## 2. Confirmed decisions (interview)

| Area | Decision |
|------|----------|
| Language / target | **Gleam on the BEAM** (Erlang/OTP). |
| Agent | **Own supervisor-worker loop** (ReDACT-style, per `tent`), not a wrapper. Supervisor plans via plain-text artifacts; deterministic harness executes; local worker fixes. |
| LLM providers | **Provider-agnostic core**, ship **Anthropic** (supervisor) first; **local `vibethinker-3b`** (arXiv:2606.16140) as the worker, served via a bundled `llama-server`. |
| Sandbox | **macOS Seatbelt** (`sandbox-exec`) for filesystem/process confinement + a **per-workspace mitmproxy** for egress (see §6). |
| Platform (v1) | **macOS only** (Seatbelt). |
| History | **Session tree only** — pi-mono style (`id`/`parentId`, `/tree`, `/fork`, `/clone`). |
| Branch scope | **Conversation + filesystem snapshot.** Forking restores chat *and* files. |
| Net rule control | **Recommended:** observe live, "disallow" forks a stricter branch (see §7). |
| Agent actions (v1) | **Code-mode**: the supervisor writes Python run in a **monty** sandbox (§5.2), calling host functions `bash`/`read`/`write`/`edit`, plus a `### CHECK`. `bash` runs under the Seatbelt profile with egress via the mitmproxy allowlist; web fetch is just `bash("curl …")`. |
| TUI | Chat pane + live **network side pane**; session tree as an overlay. |
| Service model | **opencode-style**: headless server + thin clients, OpenAPI spec. |
| v1 milestone | **Thin vertical slice** — whole pipe end-to-end (see §10). |

---

## 3. Architecture overview

Following opencode's split: a long-lived **headless server** owns all state and
the agent loop; **clients** (TUI first) are thin and talk to it over HTTP + a
streaming channel. The server owns each session's sandbox and mitmproxy
lifecycle, so it can keep agents running while no client is attached.

```
┌─────────────────────────────────────────────────────────────┐
│ bough server  (Gleam / OTP application)                      │
│                                                              │
│  HTTP + SSE API  ──  OpenAPI 3.1 spec  (for SDKs/clients)    │
│        │                                                      │
│  ┌─────┴───────┐   ┌──────────────┐   ┌──────────────────┐  │
│  │ Session     │   │ Supervisor-  │   │ Seatbelt + mitm  │  │
│  │ tree store  │   │ worker loop  │   │ bridge           │  │
│  │ (JSONL)     │   │ (per session)│   │ profile/egress/  │  │
│  │             │   │ + harness    │   │ audit/snapshot   │  │
│  └─────────────┘   └──────┬───────┘   └────────┬─────────┘  │
└───────────────────────────┼────────────────────┼────────────┘
                            │ executes steps      │ launches + observes
                    ┌───────┴────────┐    ┌───────┴──────────────┐
                    │ Seatbelt cell  │    │ mitmproxy + audit    │
                    │ (sandbox-exec):│    │ (per workspace)      │
                    │ bash, fs, etc. │    │ egress allow, inject │
                    └────────────────┘    └──────────────────────┘

   clients ── TUI (v1) ── [web / desktop / IDE plugin later] ── via API
```

Each agent **session** is an OTP process tree (supervised), so a crash in one
session can't take down the server or sibling sessions. Detached sessions keep
running; clients attach/detach freely.

---

## 4. The session tree (history)

Modeled on pi-mono. Persisted as **JSONL**, one entry per line, under
`~/.bough/sessions/<project>/<session-id>.jsonl`.

Each entry: `{ id, parentId, role, content/tool, timestamp, label?, snapshotRef? }`.
The "active leaf" is the current position. Branching = appending an entry whose
`parentId` is an earlier node.

Operations (exposed as API verbs and TUI commands):

| Verb | Meaning |
|------|---------|
| `tree` | Navigate/visualize the tree; jump the leaf to any node. |
| `fork` | Start a new branch from an earlier user message (edit + resubmit). |
| `graft` | Reattach a section of the tree onto a different parent (§4.2, longterm). |
| `clone` | Duplicate the active branch into a new session. |
| `resume` | Pick a past session for the current project. |
| `label` | Name a node for navigation. |

### 4.1 Branch = conversation + filesystem

This is bough's differentiator over pi-mono. Each node *may* carry a
`snapshotRef` — a commit SHA in a per-session **shadow git repo** under
`~/.bough/snapshots/<id>` whose work-tree is the workspace (content-addressed,
deduped across turns, and never touching the user's own `.git`).

- Before each agent turn that can write files, the server captures the workspace
  into the shadow repo (`add -A` + `commit`) and records the commit SHA on the
  resulting node.
- `fork`/`tree`-jump to a node **restores that node's snapshot** before
  continuing, so the agent resumes against the exact filesystem state of that
  point — not just the chat.
- Net effect: forking forks the world. Two branches can diverge in both
  conversation and code without clobbering each other.

> **Open design point:** snapshot granularity (every write-turn vs. on-branch
> only) and how restore interacts with the user's live working tree need a small
> prototype. Default to snapshot-per-write-turn, restore-on-branch.

### 4.2 Graft — reattaching sections of the tree (longterm)

`fork` branches *forward* from a point; `graft` is its complement: reattach a
section of the tree onto a different parent. Uses: lift a run of refactor turns
off a buggy base, reorder turns, or — once subagents get isolated branches (§5)
— land a subagent's branch back onto its parent.

**Graft moves the conversation, not the files.** A grafted section carries no
snapshot; jumping to it inherits the new base's files (the tree already walks up
to the nearest ancestor snapshot, §4.1). So a graft relocates *intent*, not a
finished result — the moved turns may describe edits that aren't on disk. The
agent rebuilds them against the real files on its next turn. A one-line marker is
injected into the grafted conversation so it doesn't assume that work exists:
`[grafted — prior files aren't present; current files are the base]`.

This is deliberate: no diffs to replay, no rebase, no merge conflicts. Carrying
file history would mean rebasing snapshots and resolving conflicts — out of
scope.

**Storage — graft is more appends, never a mutation**, keeping the JSONL log and
tamper-evident audit (§6) intact:

- The section's nodes are re-emitted as **new `Entry`s with new ids**, parented
  onto the target, with **no `snapshotRef`** and a `graftedFrom: id?` pointing at
  the original (so the UI can show `↪ grafted from <branch>`).
- **Originals are never deleted.** One graft-event record marks them superseded —
  a third JSONL record kind beside the meta and entry lines:

  ```json
  {"op":"graft","id":"g_<rand>","sectionRoot":"<old_id>","onto":"<target_id>",
   "mapping":{"<old_id>":"<new_id>", ...},"ts":1718000000000}
  ```

  Undo = drop the new ids and clear the superseded flag; no prior line is edited.
  The default tree view hides superseded branches; a toggle reveals them.

**Section = subtree** (a node + all descendants). Reject grafting a subtree onto
its own descendant (cycle).

**Sketch** (`bough_core/session.gleam`, pure — IO lives in the server):

```gleam
pub type Entry {
  Entry(
    // ...existing fields (snapshot_ref stays None on graft copies)...
    grafted_from: Option(String),
  )
}

pub type GraftEvent {
  GraftEvent(
    id: String,
    section_root: String,
    onto: String,
    mapping: Dict(String, String),  // old id -> new id
    timestamp: Int,
  )
}

/// Validate (nodes exist, no cycle) and produce the re-parented copies + event.
/// The server appends them; nothing touches the filesystem.
pub fn plan_graft(
  tree: SessionTree,
  section_root: String,
  onto: String,
) -> Result(#(List(Entry), GraftEvent), GraftError)
```

**UX — in the tree overlay (`t`).** `g` on a node marks the section root and
enters "pick new parent" mode; navigate to the target and Enter to confirm,
after a preview ("Graft *<label>* (N descendants) onto *<target>*"). Originals
dim as superseded. API: `POST /session/:id/graft` `{sectionRoot, onto}`.

---

## 5. Agent loop — the supervisor-worker harness

bough's agent is a **supervisor-worker** loop (the ReDACT idea, as proven in
`tent`): the hosted frontier model **plans and writes** but never executes; a
deterministic harness is the **only** thing that runs anything — inside the
Seatbelt sandbox — and a small **local** model absorbs trivial breakage for free.
Measured against the same model running a full agentic tool-use loop on the same
tasks, the architecture is roughly **2× cheaper, 2× faster, and ~20× fewer
tokens**.

**Why this over provider tool-use**: instead of threading a long sequence of
provider tool calls, the supervisor writes **one Python program per round**
(code-mode, §5.2) that the harness runs in a monty sandbox. One stable,
append-only conversation keeps the prefix cacheable (re-reads at ~10% price); a
program expresses a whole round's worth of inspect→change→verify in a single
call (and a small code-strong model writes it well, §5.6); and completion is
gated on a deterministic CHECK rather than the model's self-report.

### 5.1 The three roles

- **Supervisor** (frontier model; Anthropic Messages API + streaming first):
  plans in prose and emits `### STEP` artifacts plus a `### CHECK`. Has no way to
  touch the machine.
- **Harness** (the bough server, deterministic): parses the artifacts, executes
  each one inside the Seatbelt sandbox, feeds every result back round by round in one
  continuous conversation, and gates completion. The only actor with side
  effects.
- **Worker** (small local model, optional — `vibethinker-3b`, arXiv:2606.16140): when a
  step fails, gets one shot at a single fix command through the same policy +
  sandbox path. Disable it (`worker: null`) to fall back to supervisor-only
  fixes.

### 5.2 Action model — code-mode in a monty sandbox

The supervisor acts by writing **Python**, not by emitting typed file/shell
actions. Each round it calls `run_steps` with an ordered batch whose primary
action is `code`: a program run in a [monty](https://github.com/pydantic/monty)
sandbox — a Rust Python interpreter that can touch nothing on the host except
the host functions we hand it. This is "code-mode": the small worker-class model
the harness is built around (VibeThinker-3B, §5.6) is far stronger writing a
short program than threading a long sequence of JSON tool calls, so the program
*is* the plan.

The host functions are the entire capability surface:

| Host function | Action |
|---------------|--------|
| `bash(cmd) -> str` | run shell command(s) in the sandbox; returns combined output |
| `read(path) -> str` | read a workspace file |
| `write(path, content)` | create/replace a file wholesale |
| `edit(path, old, new)` | surgical replace of one exact, unique occurrence |

Alongside `code`, the batch may carry `spawn`/`tell`/`collect` (the subagent
protocol, §5) and a `### CHECK` — a command that exits 0 **iff** the task's
literal acceptance criteria hold. There is no separate `webfetch` tool — an
allowed fetch is just `bash("curl …")` through the mitmproxy egress allowlist (§7).

**Two nested sandboxes** (the seam with §6): monty is a *language/capability*
sandbox confining the agent's Python (no `import os`, no sockets, resource
limits); `bash` — the one door that runs native processes — opens into a
*Seatbelt* cell (kernel-enforced workspace-write confinement) whose egress is
locked to the session's mitmproxy (default-deny allowlist). monty replaces the
tool-dispatch layer; the Seatbelt + mitmproxy layer stays exactly where it was,
behind `bash`. The interpreter lives in a small Rust sidecar (`bough-monty`,
driven over `shellout` — the BEAM can't host monty in-process); its `bash` host
function wraps each command in the generated Seatbelt profile
(`--seatbelt-profile`), and the typed `RUN`/`WRITE`/… verbs remain inside the
harness for the worker's fix commands and the CHECK.

**Honest limit:** `read`/`write`/`edit` run as trusted host code *inside* the
sidecar (path-scoped to the workspace there), and `bash` runs under Seatbelt
*inside* the sidecar too — so the engine's live net gate and egress feed (§7)
don't observe code-mode `bash` in real time: the mitmproxy flushes a session's
audited egress events only on finalization. Seatbelt + the mitmproxy still
enforce workspace confinement and default-deny egress; bough just loses the
interactive approve-and-retry loop for code-mode `bash`. Routing host calls back
through the engine (to restore that) is a deferred option (§11).

### 5.3 How a turn works

1. The user message joins the one append-only conversation. (A first-turn
   workspace probe tells the supervisor what machine and tree it is on.)
2. The supervisor replies with prose and/or `### STEP` blocks and a `### CHECK`.
3. The harness runs each step in the sandbox. Full output goes to a **blackboard**
   file; the conversation carries a digest + pointer (so context stays small and
   cacheable — the supervisor reads more with a follow-up `RUN`). An off-allowlist
   connection is held/denied per §7.
4. A failed step gets one fix command from the local worker, through the same
   approval + sandbox path; if it doesn't help, the result is fed back and the
   supervisor adjusts.
5. The `### CHECK` re-runs every round; the task cannot finish until it exits 0.
6. When CHECK passes, the harness demands an **adversarial self-review** (and
   reports any pre-existing files the session modified) before it will accept the
   single word `DONE`.
7. Budgets — rounds / steps / cost — cap every turn so failures can't grind into
   the API bill.

### 5.4 Guardrails (each one earned, from tent)

| Guardrail | Failure it stops |
|---|---|
| CHECK is ground truth | model self-reporting "done" while wrong |
| Adversarial review of a passing check | check passes but the artifact is wrong |
| Integrity tracking (hash files at task start) | model editing tests/references to cheat its check |
| Refusal detection (`stop_reason`) | silent decline burning rounds |
| Budgets (rounds / steps / $) | failures grinding to the API bill |

Integrity tracking hashes the workspace at task start; because bough already
snapshots the filesystem per write-turn (§4.1), a failed review can do more than
warn — it can **fork back** to the pre-task snapshot and re-run under corrective
steps, so the bad edit never has to have happened on the branch you keep.

### 5.5 Provider-agnostic core

The supervisor speaks through a `Provider` behaviour (Anthropic first); the
worker is **just a second `Provider`** pointed at a local OpenAI-compatible
endpoint (§5.6). Swapping either is a config change. The **artifact grammar is the
contract** between both models and the harness — defined once, independent of
provider, and parsed truncation-tolerantly so a cut-off reply still yields the
steps it did emit.

### 5.6 Worker runtime — no separate daemon to host

The BEAM can't run tensor inference itself, so "the worker runs as part of
bough" means **bough owns and supervises a small inference runtime as a child
process** — not that you install and babysit a separate Ollama daemon. It is the
same pattern bough already uses for the sandbox sidecar and mitmproxy (§6): an
external process driven via `shellout` with its lifecycle managed by the server.

A `worker_runtime` module in `bough_server`, on first worker use:

1. **Ensures the model is present** — downloads a quantized GGUF to
   `~/.bough/models/` (with a checksum) if absent. Default
   `vibethinker-3b` at Q4 (arXiv:2606.16140 — a reasoning/coding SLM distilled
   on Qwen2.5-Coder-3B, strong on LiveCodeBench/LeetCode): a worker fix is one
   short command (~1500 output tokens), and at 3B the ~1.9 GB GGUF fits in
   2–3 GB and keeps fix latency low on this hardware. Its code strength is also
   why the agent's tools are expressed as Python run in a monty sandbox (§5.2).
2. **Launches a bundled inference server** as an OTP-supervised external
   process — `llama-server --host 127.0.0.1 --port <p> -m <model.gguf>` (this is
   the engine Ollama itself wraps; bundling it cuts out the daemon). It exposes
   an **OpenAI-compatible** `/v1/chat/completions` endpoint — exactly the contract
   the worker `Provider` speaks.
3. **Health-checks the port**, then routes worker calls to it. The supervisor
   shuts the child down on exit; a crashed inference process restarts under its
   supervisor without touching the agent loop.

> Why a supervised port and not a Rustler NIF: long inference would block BEAM
> schedulers, and a crash in a NIF takes down the VM. A child process gives the
> crash isolation OTP wants anyway — embedding tighter buys nothing here.

**Distribution variants** (all behind the same endpoint, so the `Provider` is
unchanged): a single fused [`llamafile`](https://github.com/Mozilla-Ocho/llamafile)
in place of binary + GGUF (simplest to ship); or an **MLX** sidecar
(`mlx_lm.server`) for the fastest Apple-Silicon inference, at the cost of a
Python/Swift dependency on a macOS-only-anyway project.

**Escape hatches:** `worker: null` runs no runtime at all — the supervisor does
its own fixes (zero local infra); or point the worker endpoint at a remote
OpenAI-compatible API if you would rather not run anything locally.

---

## 6. Sandbox: Seatbelt + mitmproxy — and its honest limits

bough owns its sandbox end to end, in two complementary layers that bracket every
native process the agent runs (code-mode `bash`, `RUN`, the worker's fix command,
the CHECK). macOS-only by design.

**Layer 1 — macOS Seatbelt (filesystem/process).** `seatbelt.gleam` generates an
SBPL profile per run, written to `sandbox.sb`, and the `bough-monty` sidecar
wraps each `bash` command in it via `--seatbelt-profile` (`monty_bridge.gleam`).
The policy:

1. **Reads:** allow-default *minus* a curated credential/secret/private denylist —
   `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config/gcloud`, keychains, browser data,
   shell configs/history, `~/Library/Mail|Messages`, etc. So toolchains can read
   what they need, but keys and secrets are unreadable.
2. **Writes:** deny-default *except* the workspace plus a curated allowlist of
   dirs toolchains legitimately write to (temp, `~/.cache`, `~/.cargo`, `~/.npm`,
   `~/go`, …) and a few device files. Extend at runtime with `BOUGH_WRITE_ALLOW`.
3. **Network:** denied at the kernel except the loopback port of the session's
   mitmproxy — so the only way out is through Layer 2.

**Layer 2 — per-workspace mitmproxy (egress).** `proxy.gleam` runs a `mitmdump`
with the `bough_proxy` addon, one per workspace, state under
`~/.bough/proxy/<key>/` (config + pid + log) on a stable loopback port; it is
reused while alive and swept at server start. The sandbox reaches it via
`HTTPS_PROXY` + the proxy CA (env the sidecar passes to `bash`). The addon:

- **Default-deny egress allowlist.** Its `config.json` (`{allow, inject}`) lists
  the approved hosts; everything else is refused. The config is re-read on mtime
  change, so an approved host takes effect on the next command without a restart.
- **Managed-credential injection (`providers.gleam`).** For a provider in
  `egress` mode (e.g. `github`), the real secret is set in **bough's own env**
  (never the sandbox); the addon injects a phantom into the sandbox and swaps it
  for the real secret on egress, so API keys never enter the sandbox. `env`-mode
  providers forward scoped, short-lived creds into the sandbox env instead (for
  tools that must sign locally, e.g. AWS SigV4); `none`-mode just stands up a
  loopback endpoint and allowlists it.
- **Audit feed.** Allow/deny egress events become `net_audit.AuditEvent`s
  (host, port, method, path, decision, reason, timestamp) for the network side
  pane (§7).

**Two rings, one airlock.** With code-mode (§5.2) there are now sandboxes at two
layers. monty confines the *agent's Python* (a language/capability ring: the
program reaches the host only through the host functions we register). Seatbelt +
the mitmproxy confine the *native processes* that Python's `bash` launches (a
kernel ring: workspace-write confinement + a default-deny egress allowlist +
audit). They are orthogonal and complementary — monty has no visibility into a
subprocess once it shells out (Seatbelt's job), and Seatbelt can't do per-call
in-language capability gating (monty's job). They meet at exactly one point:
`bash`, the only host function that runs native code, opens into a Seatbelt cell
whose egress goes through the mitmproxy. The `bough-monty` sidecar is the trusted
broker between the two untrusted zones (the agent's Python and the shell
command). Because the sidecar applies Seatbelt itself and the mitmproxy only
flushes its audited events on finalization, code-mode `bash` is outside the
engine's live net gate — see the honest limit in §5.2.

---

## 7. Network visibility & control (the side pane)

Goal (from closedshell): a live side pane showing what the agent is reaching out
to, with the ability to tighten rules.

**Reality of the mitmproxy:** its egress layer is an allowlist — the addon checks
each request's host against `config.json`'s `allow` and either relays it
(injecting any managed credential) or refuses. It emits an **audit feed** of
egress events, and re-reads its config on mtime change so the allowlist can grow
between commands.

**Recommended v1 design** (works *within* the mitmproxy's model and exploits
bough's snapshot branching):

- **Observe:** the side pane streams the mitmproxy's audit events — host, port,
  method, path, allow/deny, timestamp (`net_audit.AuditEvent`) — parsed into
  readable actions.
- **Disallow = fork a stricter branch.** When you reject a host in the side
  pane, bough rewrites the session's egress allowlist without it, then **forks
  from the snapshot just before the offending turn** and re-runs under the
  tighter policy. Because branches are cheap (shadow-git snapshots), "tighten
  and replay" is the natural undo — the offending egress never has to have
  happened on the branch you keep.
- **Default-deny posture:** start every session with an allowlist of only the
  provider endpoint(s); unknown hosts are refused by the mitmproxy and surface in
  the pane as denied attempts to optionally promote.

**Flagged limit:** true *interactive* "pause the connection and ask" (closedshell's
hold) is not wired today — the addon decides each request against the current
allowlist rather than blocking on a human, and for code-mode `bash` the audit
events only flush on finalization (§5.2), so there is no timely per-command
signal to gate on. Adding a hold-and-ask control channel to the addon (so a
pending request can wait on the side pane) is the natural extension.
**Deferred past v1.**

---

## 8. Service / multi-client model (opencode-style)

- `bough serve` — headless HTTP server, default `127.0.0.1:<port>`; optional
  basic auth via env (mirror opencode's `*_SERVER_PASSWORD`).
- **OpenAPI 3.1 spec** published at `/doc`; clients/SDKs generated from it.
- **Streaming:** SSE (or WebSocket) channel for assistant tokens, tool events,
  and network/audit events feeding the side pane.
- Default `bough` (no subcommand) starts a server *and* attaches the TUI client,
  exactly like opencode; `bough serve` runs standalone for headless/background or
  remote-client use.
- A `/tui`-style endpoint to drive a running TUI (prefill/run a prompt) — enables
  IDE plugins later. Out of scope for v1 beyond reserving the shape.

---

## 9. TUI (v1 client)

Single screen:

- **Chat pane** (primary): conversation, streaming supervisor output, and the
  per-step timeline (each artifact's title, exit code, digest, CHECK result).
- **Network side pane**: live egress feed (allow/deny), with a key to
  reject/tighten a host (→ §7 fork-stricter flow).
- **Session tree**: an overlay (not a permanent pane) — open with a key,
  navigate, select to jump/fork. Keeps the default layout uncluttered.

Gleam TUI library: evaluate `shore`, `etch`, `plushie` during the slice; pick one
and isolate behind a thin rendering module so it can be swapped.

---

## 10. v1 milestone — thin vertical slice

Done when, end to end:

1. `bough` starts a server + TUI client. → *verify:* TUI connects over the API.
2. A session launches its agent under the Seatbelt sandbox + mitmproxy (workspace
   writable, egress default-deny except the LLM provider). → *verify:* the
   generated `sandbox.sb` profile is applied (a write outside the workspace is
   denied); an off-allowlist `RUN` fetch is blocked and shows in the side pane.
3. The supervisor-worker loop runs: the Anthropic supervisor emits
   `STEP`/`RUN`/`WRITE`/`EDIT` artifacts plus a `### CHECK`; the harness executes
   each step in the sandbox, the local worker patches a failed step, and `DONE`
   is gated on the CHECK passing plus an adversarial review. → *verify:* a task
   that edits a file and runs a command finishes only after its CHECK exits 0.
4. The network side pane streams real mitmproxy audit events live. → *verify:*
   an allowed and a denied request both appear correctly.
5. A write-turn captures a shadow-git snapshot recorded on the session node.
   → *verify:* snapshot ref (commit SHA) present; `git --git-dir=~/.bough/snapshots/<id>
   log` shows it.
6. `fork` from an earlier node restores that node's snapshot and continues.
   → *verify:* files on disk match the forked node's state, not the latest.
7. Session tree persists to JSONL and reloads via `resume`. → *verify:* restart
   server, reopen session, tree intact.

Explicitly **out of scope for v1:** OpenAI/other providers, web/desktop clients,
interactive hold-and-ask proxy, Rustler NIF, Linux, IDE `/tui` drive endpoint,
auth hardening beyond basic.

---

## 11. Open questions to resolve during the slice

- Snapshot cadence vs. cost: snapshot every write-turn, or only at branch points?
- Restore semantics vs. a user's live edits in the working tree (warn? stash?).
- Can the mitmproxy addon stream audit events live (a tail-able socket/IPC)
  rather than flushing on finalization? Determines how "live" the side pane can
  be for code-mode `bash` without polling (see §5.2 honest limit).
- The exact Seatbelt profile + mitmproxy allowlist bough should emit per session
  (which toolchain dirs are read/write by default).
- Streaming transport: SSE vs. WebSocket for bidirectional client control.
- Worker runtime: resolved to a bough-supervised inference server (§5.6); still
  open — pick the bundled engine (`llama-server` vs. `llamafile` vs. MLX), and
  decide whether the worker's *fix command* (not the inference) runs inside or
  outside the sandbox.
- Whether `fork-back-on-failed-review` (§5.4) is automatic or offered — it
  couples the supervisor-worker guardrails to the snapshot tree, which neither
  tent (no snapshots) nor pi-mono (no harness) had to reconcile.
```
