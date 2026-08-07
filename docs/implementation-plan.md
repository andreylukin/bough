# bough — implementation plan (historical)

**Stale by two rewrites, kept for its reasoning.** This was the build order for the
Deno/Ink implementation; the system is now Rust (see [`ARCHITECTURE.md`](../ARCHITECTURE.md)
for the crate layout and [`specs/`](../specs) for the per-subsystem contracts, which
carry the invariants this file describes). Read it for *why* a piece is shaped the way
it is, never for where a file lives or what command to run.

Companion to [`spec.md`](./spec.md). The spec says *what* the system is; this says
*how to build it*, in what order, and how you know a piece is done.

Written to be handed out task by task, including to agents that start with nothing
but their prompt. Every task names the files it owns, whether to port or write
fresh, and acceptance criteria a verifier can check mechanically.

---

## 0. Ground rules

**Stack.** Deno, TypeScript, SQLite (`node:sqlite`), Zod for wire schemas, Ink +
React for the TUI. No build step — everything runs from source.

**Commands.**

```
deno task check    # typecheck — must pass before every commit
deno task test     # unit + integration
deno task dev      # server with --watch
deno task tui      # TUI against the local server
```

**Conventions.**

- Every module starts with a block comment explaining *why it exists and what
  invariant it holds* — not what the code does.
- **Dependency injection over globals.** The LLM client, the clock, and the
  database are injected so tests never hit the network or the real filesystem.
- **Pure core, imperative shell.** Parsing, patch application, spec math, and
  thread reconstruction are pure functions with `now` injected. HTTP handlers and
  workers are thin wrappers.
- **Errors.** Domain errors subclass `HttpError` and carry their status. One catch
  in the router turns them into responses.
- **Zod at the boundary.** Every request body and wire shape is a Zod schema in
  `src/schema/`.
- Colocate tests: `foo.ts` → `foo.test.ts`.

**No performance targets.** The measurement rig is out of scope. Don't invent
benchmarks, don't optimize speculatively, don't add timing assertions to tests.

**Definition of done for any task:** `deno task check` clean, the named test file
exists and passes, module comment present, no TODOs in the diff, and no files
touched outside the task's owned set.

---

## 1. Build vs. buy

Checked against the current ecosystem before committing to the milestones below.

### Adopt

**Structured outputs — do not hand-roll.** The Anthropic SDK ships
`zodOutputFormat` + `client.messages.parse()` for schema-constrained responses, and
`strict: true` on a tool definition guarantees valid tool parameters. Schema
compilation is cached server-side for 24h and the model retries on mismatch at the
API layer. This *is* T5.3 — the workflow `agent(prompt, {schema})` contract becomes
wiring, not implementation.

Limits to design around: no recursive schemas, no numeric or string-length
constraints (`minimum`, `maxLength`), `additionalProperties: false` required on
every object. The SDK strips unsupported constraints and validates them
client-side.

**Evaluate the Tool Runner before hand-writing the turn loop.**
`client.beta.messages.toolRunner` drives the request → execute → loop cycle, and its
per-turn hooks cover more of M2 than a first read suggests: approval gating (that's
`ask()`), error interception, tool-result modification, per-turn retries, streaming,
and compaction. T2.2 is scheduled as a spike that decides this rather than assuming
either answer.

Two things that decide it: it is **beta**, and it does **not** auto-resume
`pause_turn` — a paused turn silently ends the loop and returns as the final
message, with no error. In an agentic loop that reads as a truncated answer. The
resume pattern (push the paused assistant turn back onto the runner) has to be
explicit either way.

### Skip, deliberately

**Claude Agent SDK** (`@anthropic-ai/claude-agent-sdk`) — Claude Code as a library:
agent loop, built-in file/bash/search tools, MCP, subagents, hooks, permissions,
sessions, context management. It overlaps M2, M3, M4 and M7.

We're not adopting it, and the reason is the whole point of the project: it is a
*tool-calling* harness. bough's thesis is one program per round. History-as-a-tree,
the patch idiom, and journal-rerun workflows don't exist in it either. Adopting it
would mean fighting it immediately.

State this honestly in the README: bough is an alternative harness *design*, not a
better coding agent.

**Durable workflow engines** (Temporal, Inngest, Restate) — all do journal-and-replay,
which is exactly the workflow journal, selective rerun, *and* turn checkpointing.
Restate's `ctx.run()` is conceptually closest to a journaled `agent()` call.

Skipped on operational cost: Temporal needs a server, Inngest is a hosted service,
Restate needs its own runtime. bough is a single-user local tool that already has
SQLite. **Take the discipline, not the dependency** — see the determinism rule in
T5.2, which is Temporal's core constraint applied to our workflow worker.

**OpenTUI** — Zig render core, no frame-rate cap, materially lower memory than Ink.
It targets **Bun and Node**, not Deno (its `AGENTS.md`: *"Shared runtime code must
preserve the supported Bun and Node paths"*). The NAPI issue that would have opened
this up is closed and Node support landed, but Deno was never the second runtime.
Stay on Ink. If Ink's performance becomes a real complaint, Melker is the
Deno-native option to evaluate then.

**A diff/patch library.** `diff` and `diff-match-patch` exist, but the hash-anchored
line-range grammar is custom, small, and the safety mechanism under shared-checkout
delegation. Own it.

**Server-side compaction** (`compact-2026-01-12`) compacts in place; ours branches.
Keep ours — but don't reinvent the summarization prompt.

### Worth reading before building M3

The API's **programmatic tool calling** (`code_execution_20260120` +
`allowed_callers`) has Claude write a script that calls your tools as functions
inside a code-execution container, with intermediate results going to the script
instead of the context window. That is bough's code-mode thesis implemented
server-side. It doesn't replace M3 — the container is sandboxed and can't touch the
user's checkout, which is exactly the constraint we removed — but read it before
designing the bridge protocol.

---

## 2. Disposition of the current tree

**The cutover has happened (T10.8).** `src/` *is* the rewrite. There is one tree,
one root `deno.json`, and one `deno.lock`; every path in this document reads
literally. Nothing below is an instruction any more — it is the record of how the
tree got here, kept because the `Port from` lines still refer to files that only
exist in history.

While the rewrite was under construction it was built at `next/`, with its own
`next/deno.json`, and the old `src/` was read-only reference material. That was not
cosmetic: the layout in §3 reuses paths the old tree already occupied
(`src/db/db.ts`, `src/schema/parts.ts`, …), so building in place would have had
parallel tasks overwriting live files and each other. Building beside it removed the
collision and kept every `Port from` reference readable.

**Reading a `Port from` line now.** The file it names is gone from the working tree.
Recover it from git — `git show 385962d~1:src/turn.ts` — or read it in the history
of the path it became. The last commit that contains the whole old tree is the one
immediately before the cutover.

Deleted at cutover, alongside the old tree: `docs/identity-boundary.md`,
`docs/mcp.md`, `docs/net-transparent-proxy.md`, `docs/subagent-failure-testing.md`,
`bench/`, `probes/`, the four bundled skills that don't survive (`cloud`,
`prewalk`, `theme`, `tui-test` — `history` is rewritten at `src/skills/history/`),
and `scripts/live-smoke.ts`, which drove the old monolithic `src/turn.ts`.

**What the cutover did not do.** T10.6 (install) did not land: `install.sh`,
`scripts/setup.sh`, `scripts/bough` and `scripts/worker-model.sh` still describe the
retired design — a Seatbelt sandbox, a local `llama-server` worker, a cloudflared
tunnel, and `BOUGH_PASSWORD` auth. All four are explicit non-goals (spec §17). The
entrypoints those scripts exec (`src/server/main.ts`, `src/tui/main.tsx`,
`src/cli/exec.ts`) are correct; the surrounding provisioning is not. Rewriting them
is the remaining M10 work.

---

## 3. Repository layout

```
src/
  schema/          wire contracts (Zod) — parts, sessions, events, requests
  db/              SQLite: schema, migrations, typed accessors
  bus.ts           in-process event fan-out → SSE
  errors.ts        HttpError hierarchy
  paths.ts         ~/.bough layout, path confinement helpers

  server/
    main.ts        entry: open db, start ticker, serve
    app.ts         router + handler registry          ← SHARED FILE
    events.ts      SSE endpoint
    sessions.ts    session CRUD, thread assembly
    changes.ts     git diff against sessions.base, revert
    artifacts.ts   publish, serve, path confinement
    comments.ts    artifact comment sidecar + widget injection
    files.ts       @-autocomplete workspace search
    theme.ts       palette persistence
    keys.ts        provider key management

  llm/
    client.ts      provider routing (Anthropic | OpenAI | OpenRouter)
    stream.ts      streaming → parts
    pricing.ts     cost + context window catalog
    cheap.ts       cheap-tier helper (titles, ghost text, blurbs)

  turn/
    runner.ts      the turn loop
    state.ts       turn checkpointing + orphan recovery
    replay.ts      stored parts → provider messages
    queue.ts       queued-message drain

  harness/
    protocol.ts    shared postMessage types + host-name list   ← SHARED FILE
    vm.ts          host side of the program worker
    vm_worker.ts   program side (permissions: inherit)
    wf_vm.ts       host side of the workflow worker
    wf_worker.ts   workflow side (permissions: none)

  hostfn/
    shell.ts jobs.ts files.ts patch.ts ask.ts state.ts schedule.ts net.ts image.ts

  agents/          subagent.ts, notes.ts
  workflow/        meta.ts, run.ts, schema.ts
  schedules/       ticker + spec parsing (PURE)
  search/          FTS indexing + query
  history/         branch.ts, fork.ts, compact.ts, sections.ts, extract.ts,
                   move.ts, handoff.ts
  mcp/             registry, stdio client, remote transport, oauth
  lsp/             curated verbs over the CLI backend
  skills/          discovery + prompt assembly
  prompt/          system prompt sections (.md) + assembly
  tui/             see M9
  cli/exec.ts      headless one-shot

docs/              spec.md, this plan
skills/history/    the one bundled skill
```

**Module boundary rule:** `hostfn/` never imports from `server/` or `tui/`. It takes
a context object. This is what makes it testable without a server.

---

## 4. File ownership and shared files

Parallel agents collide on a small number of files. Every other file has exactly one
owning task.

### The four shared files

| File | Owner | Everyone else |
|---|---|---|
| `deno.json` | T-1 | All dependencies are declared up front in T-1. If a later task needs a new one, it **stops and asks** rather than editing the import map. |
| `src/harness/protocol.ts` | T3.1 | Host-function names are declared here once, in one array, at T3.1 — including names whose implementations land later in M6. Later tasks implement against the existing name; they never add one. |
| `src/db/schema.sql` | T0.3 | Every table for every milestone is created in T0.3. A later task that needs a column stops and asks. |
| `src/server/app.ts` route table | T1.1 | **Append-only.** A task adds its route entries at the end of the `routes` array and its handler in its own file. It never reorders, never edits another task's entry. |

### Ownership by milestone

Every task below carries an **Owns** line. If a file is not in your task's Owns
list, you do not modify it — you read it. A task that believes it needs to edit
another task's file has found a design problem; surface it rather than editing.

---

## 5. Milestones

```
T-1 interface freeze  (blocking, one agent, no parallelism)
 └─ M0 foundations
     └─ M1 server skeleton
         └─ M2 turn runner ──┬─ M3 program worker ─┬─ M4 subagents ── M5 workflows
                             │                     └─ M6 program verbs
                             └─ M8 history ops
M7 integrations (after M3)
M9 TUI (after M6)
M10 polish + cutover (last)
```

Ship T-1 → M3 as a working vertical slice before starting anything else. A model
that can write a program, patch a file, and stream output is the whole product in
miniature.

---

## T-1 — Interface freeze

*One agent. Blocking. Nothing else starts until this lands.*

**Owns:** `src/schema/*.ts`, `src/errors.ts`, `src/paths.ts`, `deno.json`,
`src/db/schema.sql`, `src/harness/protocol.ts`, and the `AppCtx` / `TurnCtx` /
`LlmClient` / `HostFns` type declarations.

**Port from:** `src/schema/parts.ts`, `src/db/db.ts` (schema block), `src/errors.ts`,
`src/paths.ts`, `src/harness/vm.ts` (the `PROGRAM_PARAMS` array and `HostFns`
interface).

Types and schemas only — **no implementations, no function bodies beyond throwing
`not implemented`**. Everything downstream codes against these shapes.

**AC:** `deno task check` passes. Every subsequent task can import its types without
adding a file to the frozen set.

---

## M0 — Foundations

*Pure data and pure functions. No server.*

### T0.1 — Project skeleton
**Owns:** `Makefile`, `.gitignore`, `README.md` stub · **Write fresh**
(`deno.json` is frozen in T-1)

`paths.ts` owns the `~/.bough` layout and exports `confine(root, candidate)` that
throws on traversal. Honors a `BOUGH_HOME` override so the rewrite never touches the
live install.

**AC:** `src/paths.test.ts` covers `..`, absolute escapes, and symlink-shaped inputs.

### T0.2 — Database
**Owns:** `src/db/db.ts`, `src/db/migrate.ts` · **Port from `src/db/db.ts`**, dropping
`archived_at`, `deprecated_at`, `first_output_at`, `message_embeddings`

Typed accessors only — no raw SQL outside `db/`. `messagesFor` orders by
`(created_at, rowid)`. `threadFor` = ancestors root→parent, then own.
`ancestorChain` for lineage-root scoping.

**AC:** `src/db/db.test.ts` asserts three-level `threadFor` order, migration
idempotence across two opens, and same-millisecond insert ordering.

### T0.3 — Event bus
**Owns:** `src/bus.ts` · **Port from `src/bus.ts`** (near-verbatim)

**AC:** `src/bus.test.ts` proves a throwing listener does not prevent later
listeners from receiving, and that `seq` is monotonic.

### T0.4 — Patch engine ⚠️
**Owns:** `src/hostfn/patch.ts` · **Port from `src/tools/hashedit.ts`** — read it
closely, it encodes the conflict rules

Two pure functions:

```ts
parsePatch(input: string): PatchOp[]
applyPatch(files: Map<string,string>, ops: PatchOp[]): Map<string,string>
```

- Line numbers are in the **viewed** version's coordinates. Compute all edits
  against the original, then materialize once. Never apply sequentially.
- Multi-file patches are all-or-nothing.
- **Conflict rule:** file changed since the tag but the patched ranges untouched →
  rebase and succeed. Patched range *was* touched → fail, naming file and range.

**AC:** `src/hostfn/patch.test.ts` covers every operation; overlapping ranges
rejected; out-of-bounds rejected; multi-file atomicity; and the rebase-vs-conflict
distinction **in both directions**. This module is the safety mechanism under
shared-checkout delegation — a bug here is silent data loss. Treat a failure as
stop-work.

---

## M1 — Server skeleton

### T1.1 — Router
**Owns:** `src/server/app.ts`, `src/server/main.ts` · **Port from `src/server/app.ts`**
(structure only)

`URLPattern` route table. One try/catch mapping `HttpError` → response. Handlers
take `(req, ctx, params)` with `ctx` carrying `{db, bus, llm, model}`.

Establishes the append-only convention for the `routes` array.

**AC:** `src/server/app.test.ts` drives `createHandler(ctx)` with a fabricated ctx,
no socket bound.

### T1.2 — Sessions and messages
**Owns:** `src/server/sessions.ts` · **Write fresh** (visibility rules changed)

Listing derives visibility: exclude `subagent` and `workflow_agent` from the top
level, expose via `?originId=`.

**AC:** `src/server/sessions.test.ts` proves a subagent-kind session is absent from
`GET /sessions` and present under its origin filter.

### T1.3 — SSE
**Owns:** `src/server/events.ts` · **Port from `src/server/app.ts`** (`events` handler)

**AC:** `src/server/events.test.ts` asserts framing, and that N connect/disconnect
cycles leave no listener leak.

---

## M2 — Turn runner

### T2.1 — Provider routing
**Owns:** `src/llm/client.ts`, `src/llm/pricing.ts`, `src/llm/stream.ts` ·
**Port from `src/supervisor/llm.ts`, `src/pricing.ts`, `src/server/openai_models.ts`**

Route by prefix: `openai:x` → OpenAI, `vendor/model` → OpenRouter, bare → Anthropic.
One `LlmClient` interface all three satisfy — **the turn runner must not know which
provider it is talking to.**

**AC:** `src/llm/client.test.ts` drives the interface with a fake. Provider-specific
handling appears in no file outside `src/llm/`.

### T2.2 — Turn loop *(spike first)*
**Owns:** `src/turn/runner.ts`, `src/turn/replay.ts` · **Port from `src/turn.ts`**

**Deliver a written spike decision before the implementation:** hand-written loop, or
`client.beta.messages.toolRunner`. Evaluate against — does the beta status matter
here? does the `pause_turn` gap bite given we use no server-side tools? do the
per-turn hooks actually carry `ask()` holds and interrupt? Write the answer and the
reasoning into the task's PR description; both outcomes are acceptable.

Replay mapping:
- user → one user message of text blocks (+ images from `~/.bough/attachments`; a
  lost attachment replays as placeholder text)
- supervisor → assistant message (text + tool_use), then a user message of
  tool_result blocks
- **reasoning parts are dropped on replay** — persisted for display only
- `ask` parts replay as plain text and can never re-block

**AC:** `src/turn/runner.test.ts` drives a full multi-round turn with a scripted fake
LLM and asserts reasoning parts never reach the provider payload.

### T2.3 — Checkpointing and recovery
**Owns:** `src/turn/state.ts` · **Port from `src/supervisor/turns.ts`**

On boot, any turn still `running` becomes `orphaned` and its session unblocks.

**AC:** `src/turn/state.test.ts` simulates a mid-turn crash and asserts the session
is usable after restart with no stuck `pending` message.

### T2.4 — Interrupt, queueing, retry
**Owns:** `src/turn/queue.ts` · **Port from `src/turn.ts`**

Interrupt kills the program's children and persists the partial result with
`interrupted: true`. A mid-turn message queues and drains into a fresh turn. A tool
call truncated mid-stream is retried, not executed with `{}` — emit `message.retry`.

**AC:** `src/turn/queue.test.ts` covers: interrupt mid-program leaves a well-formed
transcript; two rapid messages produce two ordered turns with no loss; a truncated
tool call retries rather than executing.

---

## M3 — Program worker

### T3.1 — Worker bridge
**Owns:** `src/harness/protocol.ts`, `vm.ts`, `vm_worker.ts` ·
**Port from `src/harness/vm.ts`, `src/harness/vm_worker.ts`**

```
main → worker    {type:"run", code}
worker → main    {type:"host", id, fn, args}
main → worker    {type:"host_result", id, ok, value}
worker → main    {type:"log", line}
main → worker    {type:"abort"}      worker → main {type:"aborted"}
worker → main    {type:"done", logs} | {type:"error", message, logs}
```

- `permissions: "inherit"`.
- Host names declared once in `protocol.ts`, imported by both sides. A test pins the
  two lists equal. A program shadowing a host name (`let bash = 1`) fails pre-flight.
- `console.*` both streams and batches.
- **Exit trap:** `process.exit`/`Deno.exit` must throw a catchable error. With
  inherited permissions an uncaught exit can take the server down.
- **Wind-down:** track spawned children; on abort or timeout kill children *first*,
  then terminate the worker.

**AC:** `src/harness/vm.test.ts` covers: a throwing program surfaces its message; an
aborted program that spawned a child leaves no orphan process; `Deno.exit()` is
catchable; a shadowed host name fails pre-flight; the two name lists match.

### T3.2 — Shell verbs
**Owns:** `src/hostfn/shell.ts`, `src/hostfn/jobs.ts` ·
**Port from `src/tools/bash.ts`, `src/tools/bash_bg.ts`**

`bash` with interrupt propagation and the **60s auto-background handoff**; `sh`
concurrent returning `[{code, out}]`; four job verbs with retained buffers and
`job.spawned`/`job.exited`. Deterministic truncation: head + tail + omission marker.

**AC:** `src/hostfn/shell.test.ts` — a long command returns the auto-background
message and is later readable via `bashOutput`; `sh` never throws on non-zero exit.

### T3.3 — File verbs
**Owns:** `src/hostfn/files.ts` · **Write fresh** (wires T0.4)

**AC:** `src/hostfn/files.test.ts` — view → patch with empty tag → succeeds and
echoes a new tag → a second patch chains on that tag without viewing again.

### T3.4 — Prompt assembly
**Owns:** `src/prompt/*.md`, `src/prompt/assemble.ts` ·
**Port from `src/supervisor/prompt.ts` and `src/supervisor/prompt/*.md`**

Sections per spec §6. Conditional inclusion by session kind and granted capability.

**AC:** `src/prompt/assemble.test.ts` asserts a subagent gets the nested-delegation
section and not the top-level one, and that a section granting a host function is
absent when the capability is absent.

---

## M4 — Subagents

**Port all of M4 from `src/subagent.ts`** — it encodes the lineage and note-delivery
rules.

| Task | Owns | Requirement | AC |
|---|---|---|---|
| **T4.1** launch | `src/agents/subagent.ts` | Real session, `parentId: null` (no inherited context), lineage set, same workspace, inherits MCP grants | Thread contains only the task; lineage points at the spawning turn |
| **T4.2** modes | same | `agent()` blocks and propagates interrupt; `spawn()` detaches; `join()` claims in-band; `adopt()` takes over | Interrupting a spawner mid-`agent()` interrupts the child; a detached child survives its spawner's turn ending |
| **T4.3** caps | same | 8 spawns/turn, 4 concurrent tree-wide; exceeding fails **that launch only** | `Promise.allSettled` of 12 → 8 fulfilled, 4 rejected, the 8 intact |
| **T4.4** reporting | `src/agents/notes.ts` | System note wakes an idle spawner; rides the queued drain if mid-flight | Idle spawner gets a new turn; busy spawner gets the note with no duplicate turn |

**T4.4 additionally owns the failure matrix.** Port the cases from
`docs/subagent-failure-testing.md` before deleting it: child errored, child
interrupted, launch refused at cap, server restarted mid-flight. Each must reach the
parent **distinguishably**. This is the task most likely to be under-tested; its AC
is that all four cases have named tests.

---

## M5 — Workflows

### T5.1 — Meta extraction
**Owns:** `src/workflow/meta.ts` · **Port from `src/workflow.ts`** (`metaLiteral`)

Balanced-brace scan skipping string/template contents and comments.

**AC:** `src/workflow/meta.test.ts` — braces in strings, template literals, comments,
missing meta.

### T5.2 — Workflow worker
**Owns:** `src/harness/wf_vm.ts`, `src/harness/wf_worker.ts` ·
**Port from `src/harness/wf_worker.ts`**

`permissions: "none"`. Bridges only `agent`/`phase`/`log`.

- `parallel(thunks)` — barrier; a throwing thunk resolves to `null`; never rejects.
- `pipeline(items, ...stages)` — **no barrier**; stages get `(prev, item, index)`; a
  throwing stage drops that item and skips its remaining stages.
- Same exit trap as the program worker.

**Determinism rule (new).** Journal replay is only sound if the script is
deterministic. `Date.now()`, `new Date()` with no argument, and `Math.random()` are
**unavailable inside the workflow worker** — calling one throws with a message
saying to pass timestamps via `args` and to vary agent prompts by index instead.
This is Temporal's core workflow constraint; without it, "rerun replays unchanged
calls" is a lie the first time a script stamps a timestamp into a prompt.

**AC:** `src/harness/wf_worker.test.ts` — a pipeline test proves no barrier (item B
reaches stage 3 while A is in stage 1, asserted via an injected fake clock); and
`Date.now()` inside a script throws.

### T5.3 — Structured agent output
**Owns:** `src/workflow/schema.ts` · **Write fresh — thin wrapper over the SDK**

Use `zodOutputFormat` / `client.messages.parse()` rather than hand-rolling
validation and retry (§1). `agent(prompt, {schema})` resolves to the parsed object.

Design around the SDK's schema limits: no recursion, no numeric or length
constraints, `additionalProperties: false` required. Reject a script's schema at
**submit time** with a clear message rather than failing mid-run.

**AC:** `src/workflow/schema.test.ts` — a malformed return retries; a persistently
malformed one fails the call with a clear error rather than returning a broken
object; an unsupported schema is rejected at submit.

### T5.4 — Journal and rerun
**Owns:** `src/workflow/run.ts` · **Port from `src/workflow.ts`**

Journal each `agent()` keyed by `hash(prompt + opts)`. `rerun({id, script?})` replays
hits instantly, re-runs only changed keys. Mirror scripts to
`~/.bough/workflows/<id>.js`.

**AC:** `src/workflow/run.test.ts` — rerunning an unchanged script issues **zero**
live agent calls; editing one prompt re-runs exactly that call and everything
downstream.

### T5.5 — Control and REST
**Owns:** workflow route handlers + entries appended to `app.ts` ·
**Port from `src/server/app.ts`** (workflow handlers)

**stop** kills the worker *and* interrupts in-flight subagent turns via the abort
signal. **pause** gates new `agent()` calls while running ones finish. Run semaphore:
4 concurrent. Subagent caps do not apply inside a workflow.

**AC:** stop leaves no running subagent turn; pause lets in-flight agents finish and
starts none.

---

## M6 — Program verbs

Independent, parallelizable. Each owns exactly one file under `hostfn/` plus its
route entries.

| Task | Owns | Port from | Requirement | AC |
|---|---|---|---|---|
| **T6.1** `ask()` | `hostfn/ask.ts` | `src/asks.ts` | Memory-only registry; settles as an `ask` part | Fresh client rebuilds the card from `GET /questions`; restart leaves nothing stale |
| **T6.2** `state.*` | `hostfn/state.ts` | `src/state.ts` | Scoped to the **lineage root**; 16KB/key | A fork and its parent read the same store |
| **T6.3** `schedule.*` | `schedules/` | `src/schedules.ts` | Spec grammar + `nextRun(spec, now)` pure; catch-up advances **from now** | A ticker down through 5 slots fires **once** |
| **T6.4** `image()` | ~~`hostfn/image.ts`~~ | `src/server/files.ts` | **Removed.** A picture reaches the model when the HUMAN attaches one (`POST /attachments`); a program writes the file and says where it is | — |
| **T6.5** `fetch()` | ~~`hostfn/fetch.ts`~~ | `src/tools/fetch_url.ts` | **Removed.** HTTP is the runtime's own `fetch` inside the program; no host verb, no cap, no wrapper to keep in step with it | — |
| **T6.6** artifacts | `server/artifacts.ts` | `src/server/artifacts.ts` | Per-session path confinement; FS is source of truth | Traversal blocked; listing survives a db reset |
| **T6.7** comments | `server/comments.ts` | `src/server/comments.ts` | Widget injected at serve time; sidecar **outside** the artifact dir | Sidecar never appears in `listArtifacts`; send posts a system note |
| **T6.8** jobs API | job route handlers | `src/server/app.ts` | List/kill/read for a session **and its subagents** | Killing a job emits `job.exited` |

---

## M7 — Integrations

| Task | Owns | Port from | AC |
|---|---|---|---|
| **T7.1** MCP core | `mcp/config.ts`, `client.ts`, `manager.ts`, `status.ts` | `src/mcp/*` (drop seatbelt spawn) | Test echo server registers, connects, answers a tool call; a server that fails to start surfaces as catalog status, **not a hang** |
| **T7.2** remote + OAuth | `mcp/remote.ts`, `mcp/oauth.ts` | `src/mcp/remote.ts`, `src/mcp/oauth.ts` | A 401 surfaces as "not authorized — /mcp auth `<name>`", never a hang; expired refresh degrades the same way |
| **T7.3** LSP | `lsp/` | `src/mcp/lsp.ts` | Empty result is an ordinary answer; a dead backend is reported once and does not retry every verb |

---

## M8 — History operations

### T8.1 — Branch seeding *(build first, blocks the rest)*
**Owns:** `src/history/branch.ts` · **Port from `src/branch.ts`**

`openBranch()` creates and announces a session, returns a `Seeder`. Every seeded
message emits `message.started`.

**Ordering invariant:** seeded messages use `Date.now()` and order by
`(created_at, rowid)`. Do **not** advance an artificial clock — a real turn started
afterwards must sort after the seed.

**AC:** a seeded branch followed immediately by a real turn orders correctly.

### T8.2–T8.7 — One task per operation
*Six separate tasks. Do not bundle.*

| Task | Owns | Port from |
|---|---|---|
| T8.2 fork | `history/fork.ts` | `src/fork.ts` |
| T8.3 compact | `history/compact.ts` | `src/compact.ts` |
| T8.4 sections | `history/sections.ts` | `src/sections.ts` |
| T8.5 extract | `history/extract.ts` | `src/extract.ts` |
| T8.6 move-into | `history/move.ts` | `src/move.ts` |
| T8.7 handoff | `history/handoff.ts` | `src/handoff.ts` |

Shared constraints: fork and compact operate only on the session's **own** messages
(reaching into ancestors is a 400 telling the user to operate on the ancestor).
Extract may pick any message in the visible thread and may carry part indexes.
Handoff never mutates the source.

**AC each:** the original session is byte-identical afterwards. Compaction
additionally: non-contiguous selections collapse each maximal run to one summary
with unselected messages copied verbatim around them.

### T8.8 — Changes
**Owns:** `src/server/changes.ts` · **Port from `src/vcs/repodiff.ts`** (drop clonefile)

**AC:** revert never touches a path the session did not change; a non-git workspace
reports "not a repository" rather than an empty diff.

### T8.9 — Search
**Owns:** `src/search/` · **Write fresh** (replaces recall)

SQLite FTS5 over message text, updated on insert.

**AC:** rebuilding the index from scratch produces identical results to incremental
indexing.

---

## M9 — TUI

**Hard rule: no component file over ~300 lines.** The previous `App.tsx` was 3,618.

| Task | Owns | Port from | AC |
|---|---|---|---|
| **T9.1** transport + store | `tui/api.ts`, `events.ts`, `store.ts` | `src/tui/api.ts`, `store.ts` | Reducers unit-tested from recorded event sequences with no renderer mounted; a dropped SSE connection reconciles via fresh fetch without duplicating messages |
| **T9.2** primitives | `tui/lines.ts`, `format.ts`, `theme.ts`, `term.ts`, `selection.ts`, `mouse.ts` | same-named files | Wrapping, ANSI width, and selection math have direct unit tests on strings |
| **T9.3a** chat | `components/Transcript.tsx`, `Composer.tsx`, `StatusBar.tsx` | `src/tui/components/App.tsx` | Renders from fixture state without a server |
| **T9.3b** panel | `components/Panel.tsx` + tabs | `Panel.tsx`, `SessionPicker.tsx`, `ModelPicker.tsx` | Each tab renders from fixtures |
| **T9.3c** tree + rail | `ConversationTree.tsx`, `SubagentRail.tsx` | same-named files | Lineage collapse renders correctly |
| **T9.3d** work views | `Workflows.tsx`, `Jobs.tsx`, `AskCard.tsx`, `DiffView.tsx` | same-named files | Each renders from fixtures |
| **T9.4** input | `tui/keys.ts` | `src/tui/keys.ts` | Keymap is data; a test asserts no duplicate bindings |

`App.tsx` is composition only and owned by T9.3a.

---

## M10 — Polish and cutover

| Task | Note |
|---|---|
| **T10.1** cheap tier | Titles, ghost text, activity blurbs. Each **fails silently**, never blocks a turn. One in-flight blurb per session — drop, don't queue. Port from `src/worker/{suggest,activity,annotate}.ts`, `src/supervisor/title.ts`. |
| **T10.2** skills | Discovery, frontmatter, `${SKILL_DIR}`, per-skill MCP grants. Port from `src/supervisor/skills.ts`. |
| **T10.3** the `history` skill | **Rewrite, don't port** — it documented recall. The new body explains how to query the new SQLite schema and the FTS index directly. |
| **T10.4** theme | Persistence, route, live-preview-on-cursor that reverts on exit. Port from `src/server/theme.ts`, `src/tui/theme.ts`. |
| **T10.5** CLI | `bough exec` — **open the event stream before posting**, or a fast turn finishes unseen. Exit 0/1/2. Port from `src/cli/exec.ts`. |
| **T10.6** install | `install.sh` + launchd. Port from `install.sh`, `scripts/bough`. |
| **T10.7** README | Describe what bough is, including the absence of any isolation boundary, and that it is an alternative harness *design*. No claim the code does not back. |
| **T10.8** cutover | **Done.** Old tree deleted (§2), `next/` renamed to `src/`, the two `deno.json`s merged into one at the root. Pointing launchd at the new entrypoint rides with T10.6, which has not landed. |

---

## 6. Invariants

Hard-won rules the old implementation encodes. Every agent reads this section; none
of them are rediscoverable from a spec.

1. **Same-millisecond message ordering.** Messages order by `(created_at, rowid)`.
   Seeding uses real `Date.now()` — never an advanced artificial clock — so a real
   turn started afterwards always sorts after the seed.
2. **`process.exit` / `Deno.exit` must be trapped in both workers.** Uncaught, they
   terminate the worker silently and strand the turn until its wall timeout. With
   inherited permissions, an exit can take the server down.
3. **Kill children before terminating a worker.** Reverse order orphans processes.
4. **Reasoning parts are dropped on replay.** Persisted for display; there are no
   signed thinking blocks to echo.
5. **`ask` parts replay as plain text.** They must never re-block on replay.
6. **A bus listener that throws must not break fan-out** to the others.
7. **Auto-background at ~60s, don't kill.** The command keeps running and announces
   its exit. Programs must never write sleep/poll loops.
8. **Schedule catch-up advances from *now*.** A server down through N slots fires
   once, not N times.
9. **`Promise.allSettled`, not `Promise.all`, for fan-out.** One refused launch at a
   cap must not discard siblings that already started.
10. **Open the event stream before posting** in the CLI, or a fast turn finishes
    unseen.
11. **One in-flight cheap-model call per session.** Drop, don't queue — the next
    round describes itself.
12. **Artifact comment sidecars live outside the artifact directory**, or listing
    walks them.
13. **MCP state is never cached.** Answer from a fresh `bough mcp` (there is no MCP
    host function — a tool is called with `bough mcp call`); grants and connections
    change between turns.
14. **An empty LSP result is an answer, not an error.**
15. **Workflow scripts are deterministic.** No `Date.now()`, no `Math.random()` —
    rerun correctness depends on it.
16. **`seq` is a dedupe key, not a resume cursor.** It resets on restart.

---

## 7. Testing strategy

| Layer | Approach |
|---|---|
| Pure functions | Direct unit tests. Patch engine, meta extraction, spec math, thread assembly, line wrapping. **Heaviest coverage here.** |
| Turn runner | Scripted fake `LlmClient`. Never touches the network. |
| Server | `createHandler(ctx)` with a fabricated ctx and in-memory db. No socket. |
| Workers | Real workers, trivial programs. Assert on the bridge protocol. |
| Subagents/workflows | Fake LLM + real orchestration. Failure paths are mandatory. |
| TUI | Store reducers over recorded event sequences; renderers over fixture state. |

**Non-negotiable:** every test runs offline and hermetically. A test that needs an
API key does not belong in `deno task test`.

---

## 8. Risks

1. **The patch engine is the safety mechanism** (T0.4). Subagents share one checkout
   with nothing else enforcing separation. A rebase-vs-conflict bug is silent data
   loss across parallel agents. Treat a bug there as stop-work.
2. **Delegated work reports no machine-readable result.** With no acceptance gate,
   structured output (T5.3) is the only typed signal a fan-out produces. Ship it with
   the first workflow.
3. **Three provider paths drift independently.** Keep differences behind `LlmClient`
   (T2.1); if provider-specific handling leaks into the turn runner, it leaks
   everywhere.
4. **The cheap tier bills continuously.** A synchronous failure there stalls turns
   for a cosmetic feature — enforce fail-silent in review.
5. **Worker wind-down** (T3.1). With inherited permissions, an unkilled child or an
   uncaught exit outlives the turn or takes the server down.
6. **Workflow determinism** (T5.2). Without the ban, journal rerun silently returns
   stale results the first time a script stamps a timestamp into a prompt — and it
   fails as wrong output, not as an error.
