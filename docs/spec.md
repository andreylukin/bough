# bough — harness specification

## 1. What bough is

bough is a coding agent that acts by **writing programs**. Instead of emitting one
tool call at a time and waiting, the model writes a single JavaScript program per
round — with loops, branching, and composition — and a harness executes it against
the user's machine. Delegation is first-class: a program can spawn subagents or
launch a detached workflow that fans work across many agents.

A headless server owns all state and execution. Clients are views over it.

## 2. Principles

1. **One program per round.** The model's only action is `run_steps(code)`. Control
   flow belongs in the program, not in a chain of round-trips.
2. **No isolation boundary.** Programs run as the user, with the user's full
   authority — filesystem, network, subprocesses, `npm:`/`jsr:` imports. Host
   functions are convenience and session integration, never confinement. bough
   states this plainly rather than implying safety it does not provide.
3. **In place.** The agent edits the user's real checkout. `git diff` is the review
   payload; `git commit` and `git push` are the delivery mechanism.
4. **History is a tree.** Any turn can be branched. Nothing is ever destructively
   rewritten — compaction and forking both produce new branches.
5. **The server is the system.** All state, execution, and orchestration live
   server-side. A client can crash or detach without affecting a running turn.
6. **Delegation is core.** Subagents and workflows are primary capabilities with
   first-class persistence, lifecycle control, and observability.

## 3. Architecture

```
┌─────────────┐   HTTP + SSE    ┌──────────────────────────────────────┐
│  TUI (Ink)  │ ───────────────▶│  server  (Deno, 127.0.0.1:4321)      │
│  CLI (exec) │ ◀─────────────── │  ├─ turn runner                      │
└─────────────┘   events         │  ├─ program worker   (inherit perms) │
                                 │  ├─ workflow worker  (no perms)      │
                                 │  ├─ SQLite  ~/.bough/bough.db        │
                                 │  └─ artifacts  ~/.bough/artifacts/   │
                                 └──────────────────────────────────────┘
```

- **Server** — one Deno process. JSON API, SSE event stream, static artifact
  hosting. Binds loopback only; no auth layer.
- **Program worker** — a fresh `Worker` per round with `permissions: "inherit"`.
  Host functions bridge over `postMessage`.
- **Workflow worker** — a `Worker` with `permissions: "none"` running one
  orchestration script. Its only capabilities are `agent()`, `phase()`, `log()`.
- **Clients** — an Ink TUI and a headless `bough exec` one-shot CLI.

## 4. Data model

### Sessions

A session is one conversation. Every session has a `workspace` (the checkout it
operates on) and optional lineage pointers.

| Field | Meaning |
|---|---|
| `id`, `title`, `createdAt` | identity |
| `kind` | `root` \| `fork` \| `compaction` \| `subagent` \| `workflow_agent` |
| `parentId` | thread inheritance — a session's thread is its ancestors' messages ++ its own |
| `originId`, `originMessageId` | lineage edge for the tree view (what it branched from) |
| `workspace`, `originDir` | the checkout; `originDir` is the stable project record |
| `model`, `effort` | per-session overrides; absent = global default |
| `draft` | prefilled composer text (set by handoff) |
| `base` | the git sha the session started from — drives the Changes rail |

**Visibility is derived, not stored.** Sessions of kind `subagent` and
`workflow_agent` collapse under their `originId` and surface only on drill-in.
Roots and their branches are always listed. There is no archive, deprecate, hide,
or purge action, and no corresponding columns.

### Messages and parts

A message has a role (`user` | `supervisor` | `system`), a `pending` flag, and an
ordered `parts[]` array. Parts are a discriminated union on `type`:

`text` · `reasoning` · `tool_call` · `tool_result` · `image` · `ask`

`system` messages are harness-injected notes (a detached subagent's report, a
background job exit, artifact comments). They render distinctly and replay to the
model as user-side text.

Image bytes live outside the parts JSON — the part stores a path under
`~/.bough/attachments/`, so rows stay small and replay survives a moved file.

### Turns

A turn is the persisted state machine covering everything after a user message
lands. Status is `running` | `done` | `error` | `interrupted` | `orphaned`. A turn
is checkpointed as it progresses, so a server restart mid-turn marks it `orphaned`
and the session recovers instead of hanging.

### Other tables

`workflows`, `workflow_agents`, `session_state` (durable KV), `schedules`,
`messages_fts` (keyword search over transcripts).

Artifacts and skills are filesystem-backed — the directory is the source of truth,
and both survive a database reset.

## 5. The turn

1. A user message is persisted; a pending supervisor message is created and
   `message.started` is emitted.
2. The model streams. Text arrives as `message.delta`; completed blocks land as
   `message.part`.
3. When the model calls `run_steps`, the program executes in a fresh worker.
   `console.*` lines stream live to the UI **and** batch into the tool result the
   model receives.
4. Loop until the model calls `stop`. Emit `message.finished`, then
   `turn.finished`.

**Interrupt.** A user interrupt signals the turn: the running program's children are
killed, the worker is wound down, and the partial tool result is persisted with
`interrupted: true`.

**Queued messages.** A message posted while a turn runs is queued and drains into a
fresh turn when the current one ends, rather than being dropped or racing it.

**Ending.** A turn never ends implicitly. The model calls `stop` after its final
text, in the same response. Every turn must produce user-visible text — a turn of
only tool calls shows the user nothing.

There is no acceptance gate. The model reports what it did; the user verifies. The
harness does not re-run a committed check or block completion.

## 6. The program environment

Host functions are pre-injected globals. The program also has the full Deno runtime
at the user's permission level and may ignore every host function.

### Shell

| Signature | Behavior |
|---|---|
| `await bash(cmd)` | Combined output. Carries the turn's interrupt. **Auto-backgrounds past 60s** — returns `…moved to background as bg_N` and keeps running; a `[background]` system note announces its exit. |
| `await sh(...cmds)` | Runs commands **concurrently**, returns `[{code, out}, …]` in order. Never throws on non-zero exit. |
| `await bashBg(cmd)` | Explicit background shell that outlives the turn. Returns `{id, pid}`. |
| `await bashOutput(id)` | Output since the last call plus a `[running]`/`[exited]` status line. Safe to call while running. |
| `await bashWait(id)` | Block until the job finishes. |
| `await bashKill(id)` | SIGTERM the job. |

Oversized output is truncated deterministically — head and tail kept verbatim with
an explicit omission marker in between. Programs are expected to filter at the
source (`rg`, `head`, `tail`, targeted reads) rather than dump and rely on
truncation.

### Files — one editing idiom

| Signature | Behavior |
|---|---|
| `await view(path)` | `[path#TAG]` header plus numbered `N:text` lines. |
| `await patch(input)` | Hash-anchored line edits. Returns the file's new TAG. |
| `await write(path, content)` | New files and wholesale rewrites. |

**Why only these.** Naming lines instead of quoting them means the code being edited
never has to survive the model's own string escaping. The TAG pins the version that
was read: when a file changed underneath but the patched lines are untouched, the
edit rebases onto the new version; when they *are* touched, it reports a conflict.
With subagents sharing one checkout, this is the primary safeguard against silent
clobbering — it is load-bearing, not merely preferred.

Patch grammar:

```
[src/server/files.ts#]
SWAP 74.=76:
+      if (subseq(q, rel)) hits.push(rel + "/");
DEL 91.=92
INS.PRE 30:
+// inserted before line 30
INS.POST 30:
+// inserted after line 30
INS.HEAD:
INS.TAIL:
```

- An **empty tag** (`[path#]`) means "the version I just viewed" — the normal case.
  An explicit tag chains a second patch onto the TAG a previous patch echoed,
  without viewing again.
- Body rows are `+`-prefixed **new text only**; `+` alone is a blank line. There are
  no `-` rows.
- Every line number is in the coordinates of the viewed version — earlier edits in
  the same patch do not shift later numbers.
- One patch may carry several files. It applies **all of them or none**.

Raw file content comes from `Deno.readTextFile` or `bash`; there is no `read()`.

### Delegation

| Signature | Behavior |
|---|---|
| `await agent(task, {name})` | Blocking. Runs a subagent to completion, returns `{sessionId, ok, report, changedFiles}`. |
| `await spawn(task, {name})` | Detached. Returns `{sessionId, title}` immediately. |
| `await join(sessionId)` | Claim a detached subagent's result in-band. |
| `await adopt(sessionId)` | Take over a subagent's session. |

### Orchestration

`workflow.start({script, args})` · `.status({id})` · `.stop({id})` · `.list()` ·
`.rerun({id, script?})`

### Session verbs

| Signature | Behavior |
|---|---|
| `await ask(q, {options})` | Parks the program and asks the human. Returns their answer; throws a catchable `user declined` on dismissal. Memory-only — the hold dies with the turn. |
| `state.get/set/list/delete` | Durable KV scoped to the **lineage root**, so forks, compactions and subagents of one piece of work share it. Any JSON, 16KB per key. Notes, not storage — keep payloads in files. |
| `schedule.list/add/enable/disable/remove` | Recurring runs. |
| `await image(path, note?)` | Attaches an image so the model can see it. Arrives as a system note on the **next** turn — attach and end the turn, never wait. |
| `await fetch(url, opts)` | Host HTTP. Returns `{status, ok, url, contentType, body, truncated}`; 1MB cap, 30s deadline. Non-2xx is data, not an exception. |
| `await artifact(name, content)` | Publishes a file for browser viewing; returns `{url, href}`. |
| `await mcp(server, tool, args)` / `mcpStatus()` | MCP tool invocation and live state. |
| `lsp.*` | Symbol navigation verbs. |
| `console.log(...)` | Streams live to the UI and batches into the model's tool result. |

## 7. Subagents

A subagent is a real session (`kind: "subagent"`) with a **fresh, task-only
thread** — no inherited context. The task string is the entire briefing, so it must
carry every path, constraint, and acceptance criterion. It does inherit the
spawning turn's MCP grants.

**Shared checkout.** Subagents work in the *same* checkout as their spawner. There
is no per-agent worktree and nothing to merge afterwards — a subagent's writes are
already present when it reports. The spawner is responsible for giving concurrent
agents disjoint files; `patch`'s hash anchoring is what turns a violation into a
reported conflict rather than a silent overwrite.

**Naming is required.** `agent(task, {name})` labels the branch in the live rail,
the finished card, and the session tree. Name it for what it is *for*, distinct
from its siblings.

**Reporting.** A blocking `agent()`/`join()` returns in-band. A detached `spawn()`
delivers its report as a system note that wakes an idle spawner with a fresh turn,
or rides the queued drain if one is mid-flight.

**Caps.** At most 8 spawns per turn and 4 subagents running concurrently across the
whole tree. Exceeding a cap fails that launch with an error — spawners should use
`Promise.allSettled`, not `Promise.all`, so one refused launch doesn't discard
siblings that already started. Subagents may delegate one level further, blocking
only.

## 8. Workflows

For fan-outs larger than the subagent caps allow. A workflow is a JavaScript script
that runs **detached** from the turn that started it, in a `permissions: "none"`
worker whose only bridge is `agent()`/`phase()`/`log()`.

### Script contract

```js
export const meta = {
  name: 'audit-handlers',
  description: 'Review every handler for missing error paths',
  phases: [{ title: 'Review' }, { title: 'Verify' }],
}

phase('Review')
const findings = await pipeline(
  FILES,
  f => agent(`Review ${f}`, { label: f, phase: 'Review', schema: FINDINGS }),
  r => parallel(r.findings.map(x => () =>
        agent(`Verify: ${x.title}`, { phase: 'Verify', schema: VERDICT }))),
)
return findings.flat().filter(Boolean)
```

`meta` must be a **pure literal** — no variables, calls, or interpolation — and is
extracted host-side by a balanced-brace scan before the body runs.

### Primitives

| Primitive | Semantics |
|---|---|
| `agent(prompt, opts)` | Runs a subagent, returns its report. Throws on failure. `opts`: `label`, `phase`, `model`, `schema`. |
| `parallel(thunks)` | Barrier — awaits all. A thunk that throws resolves to `null`; the call never rejects. |
| `pipeline(items, ...stages)` | Each item flows through all stages independently, **no barrier**. A throwing stage drops that item to `null`. Stage callbacks get `(prev, originalItem, index)`. |
| `phase(title)` / `log(msg)` | Fire-and-forget progress. Never blocks. |
| `args` | The run's input value, verbatim. |

### Structured output

`agent(prompt, {schema})` forces the subagent to return JSON validated against the
supplied JSON Schema, and `agent()` resolves to the parsed object. Validation
happens at the tool-call layer so the model retries on mismatch. Scripts branch on
typed data rather than parsing prose — this is the primary reliability mechanism
for fan-out, since delegated work reports no other machine-readable result.

### Journal and rerun

Every `agent()` call is journaled into `workflow_agents` keyed by
`hash(prompt + opts)`. `workflow.rerun({id, script?})` replays journal hits from the
source run **instantly** and re-runs only calls whose key changed. Editing a script
and rerunning therefore costs only the edited calls. Scripts mirror to
`~/.bough/workflows/<id>.js` so they can be edited on disk.

### Control

- **stop** kills the worker and interrupts in-flight subagent turns via the run's
  abort signal.
- **pause** gates *new* `agent()` calls while running ones finish; **resume** releases.
- Concurrency is capped by the run's own semaphore (4 agents at once). Subagent
  caps do not apply inside a workflow — queue as many calls as needed.

Workflow agents get no context beyond their prompt string. Workflows do not nest,
and there is no token budget ceiling.

## 9. Background and recurring work

**Schedules.** A stored `(title, prompt, workspace, spec)` tuple. A ~30s ticker
fires each enabled schedule whose `next_run_at` has passed by opening a fresh root
session and running the prompt there.

Spec grammar: `every:<N><m|h|d>` (N ≥ 1) or `daily@HH:MM` (local wall clock).

**Catch-up.** `next_run_at` advances *from now* at fire time, never from the stale
value. A server down through N missed slots fires **once** on the first tick after
boot, then resumes cadence — no burst of make-up runs.

**Background jobs.** Auto-backgrounded and explicit `bashBg` shells are tracked per
session, survive the turn, and publish `job.spawned` / `job.exited`. Their output
buffers are readable while running and after exit.

## 10. Integrations

**MCP.** A registry of servers, local (stdio subprocess) and remote (Streamable
HTTP with OAuth/PKCE against a bough-hosted callback). Per-session grants carry
into subagents. Servers are managed through bough itself — the model answers MCP
questions from a fresh `mcpStatus()` call, never from conversation memory, since
grants and connections change between turns.

**LSP.** Curated `lsp.*` verbs with bough-owned names over an external CLI backend,
so the model-facing surface stays stable if the backing tool changes. Lazy — nothing
spawns until the first `lsp.*` call. A verb that finds nothing has **not** failed;
that is an ordinary empty answer. If the backend itself errors, the program drops to
`rg` + `view` + `patch` for the rest of the task and finishes the job.

## 11. Artifacts

`artifact(name, content)` writes to `~/.bough/artifacts/<sessionId>/` — outside the
workspace, so publishing never pollutes the diff under review — and serves it from
the server origin. Names and session ids are confined to their directory.

Every served HTML artifact gets a **comment layer** injected at serve time: the user
pins notes anywhere on the page and sends the batch, which arrives as an
`[artifact comments]` system message for the agent to act on.

Artifacts are agent-authored HTML/JS served same-origin. That is explicit agent
output the user chooses to open, not a containment boundary.

Quality bar for generated pages: self-contained (no CDN, no external fonts or
images), dense over decorative, responsive to ~375px, selectable text, and an
"AI-generated — verify anything important" footer.

## 12. Models

Three provider routes, all first-class: **Anthropic**, **OpenAI**, and
**OpenRouter**. Model ids route by prefix (`openai:x` → OpenAI, `vendor/model` →
OpenRouter, bare → Anthropic). A vendored pricing catalog drives live cost and
context-window display.

Two tiers, both chosen in the model picker:

- **Frontier** — the supervisor. Per-session pinning; switching moves the default
  for new sessions and leaves other existing sessions alone.
- **Cheap** — powers auto session titles, composer ghost text, and live activity
  blurbs. Every one of these bills on every round, so each must fail silently and
  never block or delay a turn.

## 13. Changes review

The working tree is the tip. `sessions.base` records the sha the session started
from, so the change set is `git diff <base>` plus untracked files.

**Revert** is the only mutation: restore tracked paths from the base sha and delete
untracked ones, per path, never touching anything the session did not change.

## 14. History operations

All operate by **branching**, never by mutating history in place.

| Operation | Result |
|---|---|
| **Fork** | Cut a thread at one of the session's own turns; branch a sibling seeded with the messages before it. With `editedText`, replace that turn's user message and run a fresh turn ("edit & resend"). |
| **Compact** | Replace a selected span with an LLM summary on a new sibling branch. Non-contiguous selections collapse each maximal run to one summary in place. |
| **Sections** | Stateless LLM pass labeling turns into contiguous topic sections, so the UI can color history and offer whole sections as selections. |
| **Extract** | Copy hand-picked messages into a fresh **root** — any message in the visible thread, ancestors included. Picks may carry part indexes to copy a turn's prose without its tool calls. |
| **Move-into** | Append copies of picked messages onto an **existing** session. |
| **Handoff** | LLM drafts the opening prompt for a fresh root from a stated goal — the goal restated, only the context that matters, and the relevant paths. Persisted as the new session's `draft`; the source is never mutated. |

Fork and compact rely on thread-through-parents: a new session parented at the
target's parent inherits shared ancestors for free, and its own seeded messages
reconstruct the rest. Both are limited to the session's *own* messages; a selection
reaching into ancestor history is a 400.

## 15. Clients

**TUI (Ink).** State layer separated from rendering; no monolithic component. One
tabbed panel holds every non-chat surface (sessions, tree, changes, model picker,
MCP, skills, theme) with direct-jump keys. Chat shows folded reasoning, the program
that ran, live cost and context. A rail pins live subagents; the tree shows every
root, fork and subagent branch.

**CLI.** `bough exec [flags] "prompt"` — creates a session, opens the event stream
*before* posting (a fast turn must not finish unseen), streams assistant text to
stdout, exits 0 on a completed turn, 1 on an errored one, 2 on usage or connection
problems.

## 16. Skills and theming

**Skills** are `/name` instruction bundles: a folder with `SKILL.md`, frontmatter
(`name`, `description`, optional `mcp:` server list), and a markdown body appended
to the system prompt for that run. `${SKILL_DIR}` resolves to the skill's folder so
instructions can reference helper scripts. Sources, first name wins: bundled →
`~/.bough/skills`. One skill ships bundled: **`history`**, documenting how to query
bough's own SQLite.

**Theming** is a named partial palette over a fixed set of semantic tokens,
persisted server-side and served over HTTP. The TUI fetches it at boot and paints
truecolor — a theme is pure data, no rebuild. The picker previews live on cursor
move and reverts on exit, so browsing never commits.

## 17. Non-goals

Explicitly out of scope. These are decisions, not omissions:

- **No sandbox, egress proxy, or credential gating.** No confinement of any kind.
- **No acceptance gate.** No committed check, no harness-verified `done`.
- **No local inference.** No supervised `llama-server`, no GGUF management. The
  cheap tier is a hosted model.
- **No semantic recall.** Cross-session search is keyword (SQLite FTS) over
  transcripts. No embeddings, no vector index.
- **No output digestion or `extract()`.** Oversized output is truncated
  deterministically.
- **No `edit()` or `read()`.** One editing idiom.
- **No archive, deprecate, or purge.** Visibility is derived from lineage.
- **No per-agent worktrees or file leases.** One shared checkout.
- **No benchmarking, probes, or metrics endpoints** in this repo.
- **No remote access.** Loopback only; no auth layer, no tunnel.
- **No web UI.** The server is API plus artifact hosting.
- **No workflow nesting or token budgets.**
- **No non-git snapshotting.** Files outside a repo are not tracked or reviewable.

## 18. Open questions

Resolved by stated assumption; cheap to reverse:

- **`adopt()`** carries forward with the delegation verbs; it was not separately
  scoped.
- **Cheap-tier failure modes** — titles, ghost text and blurbs are specified to fail
  silently. Whether a failed title retries or the session keeps a truncated-prompt
  fallback name is unspecified.
- **FTS ranking** — keyword search is specified as SQLite FTS; ranking and snippet
  strategy are left to implementation.
