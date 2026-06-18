# bough — Specification

> A sandboxed coding agent with branchable history. Written in Gleam, sandboxed by
> nono, structured like opencode (server + clients), with closedshell-style live
> network visibility.

**Status:** draft v0.1 — derived from interview on 2026-06-16.

---

## 1. What bough is

`bough` is a coding-agent harness. Its name carries the two core promises:

- **A bough is a branch.** History is a tree; you can fork any earlier point and
  grow a new branch — *and* the filesystem forks with it.
- **It's safe to leave it growing.** Every agent runs under a kernel-enforced
  [nono](https://nono.sh) sandbox: network allowlist + atomic filesystem
  snapshots + tamper-evident audit. You can detach, walk away, and reattach.

It is *not* a wrapper around an existing agent (like closedshell wraps `claude`).
bough implements its own agent loop — and that loop is **supervisor-worker**
(the ReDACT idea, proven in [tent](https://github.com/andreylukin/tent)): a
hosted frontier model **plans and writes** but never touches the machine; a
deterministic harness is the **only** thing that executes — inside the nono
sandbox — and a small local model patches trivial breakage for free (see §5).

---

## 2. Confirmed decisions (interview)

| Area | Decision |
|------|----------|
| Language / target | **Gleam on the BEAM** (Erlang/OTP). |
| Agent | **Own supervisor-worker loop** (ReDACT-style, per `tent`), not a wrapper. Supervisor plans via plain-text artifacts; deterministic harness executes; local worker fixes. |
| LLM providers | **Provider-agnostic core**, ship **Anthropic** (supervisor) first; **local Ollama** (`qwen2.5-coder`) as the optional worker. |
| nono coupling | **Deep integration** (see §6 for what that means given no BEAM SDK). |
| Platform (v1) | **macOS only** (Seatbelt via nono). |
| History | **Session tree only** — pi-mono style (`id`/`parentId`, `/tree`, `/fork`, `/clone`). |
| Branch scope | **Conversation + filesystem snapshot.** Forking restores chat *and* files. |
| Net rule control | **Recommended:** observe live, "disallow" forks a stricter branch (see §7). |
| Agent actions (v1) | Plain-text artifacts the harness executes: `RUN`, `WRITE`, `EDIT`, `READ`, `GREP`, plus a `### CHECK`. Web fetch is a `RUN` through the nono allowlist. |
| TUI | Chat pane + live **network side pane**; session tree as an overlay. |
| Service model | **opencode-style**: headless server + thin clients, OpenAPI spec. |
| v1 milestone | **Thin vertical slice** — whole pipe end-to-end (see §10). |

---

## 3. Architecture overview

Following opencode's split: a long-lived **headless server** owns all state and
the agent loop; **clients** (TUI first) are thin and talk to it over HTTP + a
streaming channel. This pairs with nono's detached-session model: the server can
keep agents running while no client is attached.

```
┌─────────────────────────────────────────────────────────────┐
│ bough server  (Gleam / OTP application)                      │
│                                                              │
│  HTTP + SSE API  ──  OpenAPI 3.1 spec  (for SDKs/clients)    │
│        │                                                      │
│  ┌─────┴───────┐   ┌──────────────┐   ┌──────────────────┐  │
│  │ Session     │   │ Supervisor-  │   │ nono supervisor  │  │
│  │ tree store  │   │ worker loop  │   │ bridge           │  │
│  │ (JSONL)     │   │ (per session)│   │ ps/attach/audit/ │  │
│  │             │   │ + harness    │   │ rollback/policy  │  │
│  └─────────────┘   └──────┬───────┘   └────────┬─────────┘  │
└───────────────────────────┼────────────────────┼────────────┘
                            │ executes steps      │ launches + observes
                    ┌───────┴────────┐    ┌───────┴──────────────┐
                    │ nono sandbox   │    │ nono proxy + audit   │
                    │ (Seatbelt):    │    │ (unsandboxed parent) │
                    │ bash, fs, etc. │    │ net allowlist, snaps │
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
| `clone` | Duplicate the active branch into a new session. |
| `resume` | Pick a past session for the current project. |
| `label` | Name a node for navigation. |

### 4.1 Branch = conversation + filesystem

This is bough's differentiator over pi-mono. Each node *may* carry a
`snapshotRef` pointing at a nono rollback snapshot (content-addressable, SHA-256
dedup, APFS `clonefile` COW — cheap on macOS).

- Before each agent turn that can write files, the server requests a nono
  snapshot and records its ref on the resulting node.
- `fork`/`tree`-jump to a node **restores that node's snapshot** before
  continuing, so the agent resumes against the exact filesystem state of that
  point — not just the chat.
- Net effect: forking forks the world. Two branches can diverge in both
  conversation and code without clobbering each other.

> **Open design point:** snapshot granularity (every write-turn vs. on-branch
> only) and how restore interacts with the user's live working tree need a small
> prototype. Default to snapshot-per-write-turn, restore-on-branch.

---

## 5. Agent loop — the supervisor-worker harness

bough's agent is a **supervisor-worker** loop (the ReDACT idea, as proven in
`tent`): the hosted frontier model **plans and writes** but never executes; a
deterministic harness is the **only** thing that runs anything — inside the nono
sandbox — and a small **local** model absorbs trivial breakage for free.
Measured against the same model running a full agentic tool-use loop on the same
tasks, the architecture is roughly **2× cheaper, 2× faster, and ~20× fewer
tokens**.

**Why this over provider tool-use** (and why §2 no longer lists JSON-schema
tools): the supervisor emits **plain-text artifacts**, not provider tool-call
JSON. One stable, append-only conversation keeps the prefix cacheable (re-reads
at ~10% price); code travels as plain fenced text (structured JSON payloads
break on real code's escaping); and completion is gated on a deterministic CHECK
rather than the model's self-report.

### 5.1 The three roles

- **Supervisor** (frontier model; Anthropic Messages API + streaming first):
  plans in prose and emits `### STEP` artifacts plus a `### CHECK`. Has no way to
  touch the machine.
- **Harness** (the bough server, deterministic): parses the artifacts, executes
  each one inside the nono sandbox, feeds every result back round by round in one
  continuous conversation, and gates completion. The only actor with side
  effects.
- **Worker** (small local model, optional — e.g. Ollama `qwen2.5-coder`): when a
  step fails, gets one shot at a single fix command through the same policy +
  sandbox path. Disable it (`worker: null`) to fall back to supervisor-only
  fixes.

### 5.2 Artifact grammar (replaces provider tool-use)

The supervisor answers in one fixed plain-text shape — prose first, then `### STEP`
blocks, each holding exactly one action:

| Verb | Action |
|------|--------|
| `RUN` | shell command(s), in a fenced block |
| `WRITE <path>` | create/replace a file wholesale (fenced full content) |
| `EDIT <path>` | surgical replace of one exact, unique occurrence (two fences: find / replace) |
| `READ <path> [start-end]` | line-numbered read — the precise way to see bytes before an `EDIT` |
| `GREP <pattern>` | recursive, line-numbered workspace search |
| `### CHECK` | a command that exits 0 **iff** the task's literal acceptance criteria hold |

All of these execute inside the nono sandbox (workspace-scoped; network
default-deny except the provider). There is no separate `webfetch` tool — an
allowed fetch is just a `RUN` (`curl …`) through the nono net allowlist, and it
surfaces in the side pane like any other connection (§7).

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
same pattern bough already uses for nono (§6): an external process driven via
`shellout` with its lifecycle under an OTP supervisor.

A `worker_runtime` module in `bough_server`, on first worker use:

1. **Ensures the model is present** — downloads a quantized GGUF to
   `~/.bough/models/` (with a checksum) if absent. Default
   `qwen2.5-coder:7b` at Q4: a worker fix is one short command (~1500 output
   tokens), so a 7B is plenty and bigger models are not worth the latency on
   this hardware.
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

## 6. nono integration ("deep") — and its honest limits

nono ships a CLI + Rust/Go/Python/TS SDKs, **but no Erlang/BEAM SDK**. So "deep
integration" from Gleam means, in priority order:

1. **Drive the CLI / session runtime.** Launch agents via `nono run`
   (supervised, `--detached` for background sessions). Manage lifecycle with
   `nono ps / attach / inspect / stop / prune`.
2. **Consume nono's audit + proxy event stream** for the network side pane and
   the audit view (the proxy audit log + session audit events).
3. **Use rollback** (`nono run --rollback`, `nono rollback list/restore`) as the
   snapshot backend for §4.1.
4. **Use credential injection** so API keys (Anthropic, etc.) never enter the
   sandbox — the proxy injects them on egress.
5. **(Later, optional) Rustler NIF over `nono-core`** if CLI-level coupling
   proves too coarse for live policy control.

**Capability profile:** bough generates a nono capability profile/manifest per
session — allow the workspace dir, set the network allowlist (LLM provider +
explicitly approved hosts), block everything else.

---

## 7. Network visibility & control (the side pane)

Goal (from closedshell): a live side pane showing what the agent is reaching out
to, with the ability to tighten rules.

**Reality of nono:** its network layer is a *static allowlist* — the CONNECT
tunnel validates the host against the allowlist and either relays or returns
`403`. It exposes a **proxy audit log** of egress events, but no documented
runtime "hold-and-ask" or live allowlist mutation.

**Recommended v1 design** (works *within* nono's model and exploits bough's
snapshot branching):

- **Observe:** the side pane streams nono's proxy audit events — host, method,
  path, allow/deny, timestamp — parsed into readable actions.
- **Disallow = fork a stricter branch.** When you reject a host in the side
  pane, bough rewrites the session's capability profile with the new `forbid`,
  then **forks from the snapshot just before the offending turn** and re-runs
  under the tighter policy. Because branches are cheap (COW snapshots), "tighten
  and replay" is the natural undo — the offending egress never has to have
  happened on the branch you keep.
- **Default-deny posture:** start every session with an allowlist of only the
  provider endpoint(s); unknown hosts are blocked by nono and surface in the
  pane as denied attempts to optionally promote.

**Flagged risk / upstream dependency:** true *interactive* "pause the connection
and ask" (closedshell's hold) is not available through nono today. If we want it
without restarting, options are (a) request a control IPC / runtime-mutable
allowlist from nono upstream, or (b) layer bough's own hold-and-ask proxy in
front of nono (nono still provides kernel enforcement). **Deferred past v1.**

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
2. A session launches its agent under a nono sandbox (workspace allowed, network
   default-deny except the LLM provider). → *verify:* `nono ps` shows the
   session; an off-allowlist `RUN` fetch is blocked and shows in the side pane.
3. The supervisor-worker loop runs: the Anthropic supervisor emits
   `STEP`/`RUN`/`WRITE`/`EDIT` artifacts plus a `### CHECK`; the harness executes
   each step in the sandbox, the local worker patches a failed step, and `DONE`
   is gated on the CHECK passing plus an adversarial review. → *verify:* a task
   that edits a file and runs a command finishes only after its CHECK exits 0.
4. The network side pane streams real nono proxy audit events live. → *verify:*
   an allowed and a denied request both appear correctly.
5. A write-turn creates a nono rollback snapshot recorded on the session node.
   → *verify:* snapshot ref present; `nono rollback list` shows it.
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
- Does nono expose proxy audit events as a tail-able stream/socket, or only as a
  post-hoc log file? Determines how "live" the side pane can be without polling.
- Exact nono profile/manifest schema bough should emit per session.
- Streaming transport: SSE vs. WebSocket for bidirectional client control.
- Worker runtime: resolved to a bough-supervised inference server (§5.6); still
  open — pick the bundled engine (`llama-server` vs. `llamafile` vs. MLX), and
  decide whether the worker's *fix command* (not the inference) runs inside or
  outside the sandbox.
- Whether `fork-back-on-failed-review` (§5.4) is automatic or offered — it
  couples the supervisor-worker guardrails to the snapshot tree, which neither
  tent (no snapshots) nor pi-mono (no harness) had to reconcile.
```
