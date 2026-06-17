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
bough implements its own agent loop.

---

## 2. Confirmed decisions (interview)

| Area | Decision |
|------|----------|
| Language / target | **Gleam on the BEAM** (Erlang/OTP). |
| Agent | **Own agent loop**, not a wrapper. |
| LLM providers | **Provider-agnostic core**, ship **Anthropic** first. |
| nono coupling | **Deep integration** (see §6 for what that means given no BEAM SDK). |
| Platform (v1) | **macOS only** (Seatbelt via nono). |
| History | **Session tree only** — pi-mono style (`id`/`parentId`, `/tree`, `/fork`, `/clone`). |
| Branch scope | **Conversation + filesystem snapshot.** Forking restores chat *and* files. |
| Net rule control | **Recommended:** observe live, "disallow" forks a stricter branch (see §7). |
| Agent tools (v1) | bash/shell, file read/write/edit, search (grep/glob), web fetch. |
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
│  │ Session     │   │ Agent loop   │   │ nono supervisor  │  │
│  │ tree store  │   │ (per session)│   │ bridge           │  │
│  │ (JSONL)     │   │ provider +   │   │ ps/attach/audit/ │  │
│  │             │   │ tools        │   │ rollback/policy  │  │
│  └─────────────┘   └──────┬───────┘   └────────┬─────────┘  │
└───────────────────────────┼────────────────────┼────────────┘
                            │ tool calls          │ launches + observes
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

## 5. Agent loop & tools

Provider-agnostic core with a `Provider` behaviour; **Anthropic** (Messages API +
tool use, streaming) is the first implementation. Loop: send context → stream
assistant output → execute requested tools → append results → repeat until no
tool calls or a stop condition.

**v1 tools** (all execute *inside* the nono sandbox):

- `bash` — shell exec.
- `read` / `write` / `edit` — file read, full write, surgical string-replace edit.
- `grep` / `glob` — content search and file globbing.
- `webfetch` — fetch a URL (subject to the nono net allowlist; appears in the
  side pane).

Tools are defined once with JSON schemas and surfaced to whichever provider is
active.

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

- **Chat pane** (primary): conversation, streaming output, tool calls/results.
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
   session; off-allowlist `webfetch` is blocked and shows in the side pane.
3. Anthropic agent loop runs with bash + file (read/write/edit) + grep/glob +
   webfetch tools. → *verify:* a task that edits a file and runs a command
   completes.
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
```
