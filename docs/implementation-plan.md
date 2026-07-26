# bough — implementation plan

Companion to [`spec.md`](./spec.md). The spec says *what* the system is; this says
*how to build it*, in what order, and how you know a piece is done.

Written to be handed out task by task. Each task names the files it owns, the
contract it must satisfy, and its acceptance criteria. Tasks within a milestone are
mostly parallelizable; milestones are not.

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
  invariant it holds* — not what the code does. Read any existing file for the
  register.
- **Dependency injection over globals.** The LLM client, the clock, and the
  database are injected so tests never hit the network or the real filesystem.
  Every test must be hermetic and runnable offline.
- **Pure core, imperative shell.** Parsing, diffing, patch application, spec math,
  and thread reconstruction are pure functions with `now` injected. HTTP handlers
  and workers are thin wrappers over them.
- **Errors.** Domain errors subclass `HttpError` and carry their status. One catch
  in the router turns them into responses; handlers do not catch per error type.
- **Zod at the boundary.** Every request body and every wire shape is a Zod schema
  in `src/schema/`. Round-trip tests pin them.
- Colocate tests: `foo.ts` → `foo.test.ts`.

**Definition of done for any task:** `deno task check` clean, tests written and
passing, module comment present, no TODOs left in the diff.

---

## 1. Repository layout

```
src/
  schema/          wire contracts (Zod) — parts, sessions, events, requests
  db/              SQLite: schema, migrations, typed accessors
  bus.ts           in-process event fan-out → SSE
  errors.ts        HttpError hierarchy
  paths.ts         ~/.bough layout, path confinement helpers

  server/
    main.ts        entry: open db, start ticker, serve
    app.ts         router + handler registry
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
    vm.ts          host side of the program worker
    vm_worker.ts   program side (permissions: inherit)
    wf_vm.ts       host side of the workflow worker
    wf_worker.ts   workflow side (permissions: none)
    protocol.ts    shared postMessage message types

  hostfn/
    shell.ts       bash, sh
    jobs.ts        bashBg, bashOutput, bashWait, bashKill
    files.ts       view, write
    patch.ts       the patch grammar — parse + apply (PURE)
    ask.ts         ask() holds
    state.ts       state.* durable KV
    schedule.ts    schedule.* verbs
    net.ts         fetch()
    image.ts       image()

  agents/
    subagent.ts    launch, caps, lineage, reporting
    notes.ts       system-note delivery + idle wake

  workflow/
    meta.ts        meta literal extraction (PURE)
    run.ts         run lifecycle, journal, rerun
    schema.ts      structured-output validation

  schedules/       ticker + spec parsing (PURE)
  search/          FTS indexing + query
  history/         fork, compact, sections, extract, move, handoff, branch seeding
  mcp/             registry, stdio client, remote transport, oauth
  lsp/             curated verbs over the CLI backend
  skills/          discovery + prompt assembly
  prompt/          system prompt sections (.md) + assembly
  tui/             see M9
  cli/exec.ts      headless one-shot

docs/              spec.md, this plan
skills/history/    the one bundled skill
```

**Rule:** `hostfn/` modules never import from `server/` or `tui/`. They take a
context object. This keeps them testable without a server.

---

## 2. Milestones

```
M0 foundations
 └─ M1 server skeleton
     └─ M2 turn runner ──┬─ M3 program worker ─┬─ M4 subagents ─── M5 workflows
                         │                     └─ M6 program verbs
                         └─ M8 history ops
M7 integrations (after M3)
M9 TUI (after M6)
M10 polish (last)
```

Ship M0→M3 as a working vertical slice before starting anything else. A model that
can write a program, patch a file, and stream output is the whole product in
miniature; everything after is surface area.

---

## M0 — Foundations

*No server yet. Pure data and pure functions.*

### T0.1 — Project skeleton
Files: `deno.json`, `Makefile`, `.gitignore`, `src/paths.ts`, `src/errors.ts`

- Tasks `check` / `test` / `dev` / `tui`. Import map pinning every dependency to an
  exact version.
- `paths.ts` owns the `~/.bough` layout (`bough.db`, `artifacts/`, `attachments/`,
  `skills/`, `workflows/`) and exports a `confine(root, candidate)` helper that
  throws on traversal outside a root.
- `errors.ts`: `HttpError` base with `status`, plus subclasses per domain.

**AC:** `deno task check` passes on an empty tree. `confine` has tests covering
`..`, absolute escapes, and symlink-shaped inputs.

### T0.2 — Wire schema
Files: `src/schema/parts.ts`, `sessions.ts`, `events.ts`, `requests.ts` + tests

- Part union: `text` · `reasoning` · `tool_call` · `tool_result` · `image` · `ask`.
- `Message`, `Session` (fields per spec §4), `Turn`.
- `BoughEvent` envelope: `{type, sessionId?, data, seq, ts}`. `data` is `unknown` at
  the envelope; typed payload schemas live beside it.
- Event types: `session.created` · `session.updated` · `session.activity` ·
  `message.started` · `message.delta` · `message.part` · `message.finished` ·
  `message.retry` · `turn.finished` · `ask.question` · `job.spawned` · `job.exited` ·
  `workflow.updated` · `workflow.agent` · `workflow.log`.

**AC:** Round-trip test per schema — parse(serialize(x)) deep-equals x. Adding an
unknown part type fails parsing loudly.

### T0.3 — Database
Files: `src/db/schema.sql`, `src/db/db.ts`, `src/db/migrate.ts` + tests

Tables: `sessions`, `messages`, `turns`, `workflows`, `workflow_agents`,
`session_state`, `schedules`, `messages_fts`.

- Typed accessors only — no raw SQL outside `db/`.
- `messagesFor(sessionId)` orders by `(created_at, rowid)` so same-millisecond
  inserts keep insertion order.
- `threadFor(sessionId)` = ancestors' messages root→parent, then own messages.
- `ancestorChain(sessionId)` for lineage-root scoping.
- Migrations are forward-only and idempotent; opening an existing db must never
  lose rows.

**AC:** A test builds a 3-level session chain and asserts `threadFor` order. A test
opens a db twice and asserts migration idempotence. Same-ms insert ordering is
pinned.

### T0.4 — Event bus
Files: `src/bus.ts` + test

`publish(e)` stamps `seq`/`ts`, delivers synchronously to every subscriber, returns
the stamped event. `subscribe(fn)` returns an unsubscribe. **A listener that throws
must not break fan-out to the others** — log and continue.

**AC:** Test that a throwing listener doesn't prevent later listeners from
receiving. `seq` is monotonic.

### T0.5 — Patch engine *(pure, high-value, do it early)*
Files: `src/hostfn/patch.ts` + a thorough test file

The grammar in spec §6. Two pure functions:

```ts
parsePatch(input: string): PatchOp[]        // throws with a precise message
applyPatch(files: Map<string,string>, ops: PatchOp[]): Map<string,string>
```

Requirements:
- Line numbers are in the **viewed** version's coordinates — compute all edits
  against the original, then materialize once. Do not apply sequentially.
- Multi-file patches are all-or-nothing.
- TAG computation: a short stable hash of file content. An empty tag resolves
  against the session's last `view()` of that path.
- **Conflict semantics:** if the file changed since the tag but none of the patched
  line ranges were touched, rebase onto the new version and succeed. If a patched
  range *was* touched, fail with a conflict naming the file and range.

**AC:** Tests for every operation; overlapping ranges rejected; out-of-bounds
rejected; multi-file atomicity; the rebase-vs-conflict distinction covered in both
directions. This module is the safeguard against subagents clobbering each other —
test it like it matters.

---

## M1 — Server skeleton

### T1.1 — Router
Files: `src/server/app.ts`, `src/server/main.ts`

`URLPattern`-based route table `{method, pattern, handler}`. One try/catch mapping
`HttpError` → response. 404 fallback. `GET /` returns a plain-text pointer.

Handlers take `(req, ctx, params)` where `ctx` carries `{db, bus, llm, model}` —
this is what makes tests hermetic.

**AC:** `createHandler(ctx)` is exported and tests drive it with a fabricated ctx,
no socket bound.

### T1.2 — Sessions + messages
Files: `src/server/sessions.ts`

`GET/POST /sessions`, `GET /sessions/:id` (returns `{session, thread}`),
`POST /sessions/:id/messages`, `PUT /sessions/:id/draft`.

Listing derives visibility: exclude `subagent` and `workflow_agent` kinds from the
top level; expose them via a `?originId=` filter for drill-in.

**AC:** Creating a subagent-kind session does not appear in `GET /sessions` but does
appear under its origin filter.

### T1.3 — SSE
Files: `src/server/events.ts`

`GET /events[?sessionId=]` — named events, heartbeat, clean unsubscribe on
disconnect. Filtering by session must not drop global events the UI needs.

**AC:** Test subscribes, publishes, asserts framing and that disconnect
unsubscribes (no leak after N connect/disconnect cycles).

---

## M2 — Turn runner

### T2.1 — Provider routing
Files: `src/llm/client.ts`, `src/llm/pricing.ts`

Route by model id prefix: `openai:x` → OpenAI, `vendor/model` → OpenRouter, bare →
Anthropic. One `LlmClient` interface all three satisfy — **the turn runner must not
know which provider it is talking to.**

**AC:** A fake client satisfying the interface drives the runner in tests. Pricing
lookup falls back sanely for unknown ids.

### T2.2 — The turn loop
Files: `src/turn/runner.ts`, `src/turn/replay.ts`

The lifecycle in spec §5. Replay mapping:
- user message → one user message of text blocks (+ image blocks loaded from
  `~/.bough/attachments`; a lost attachment replays as placeholder text)
- supervisor → assistant message (text + tool_use), followed by a user message of
  tool_result blocks when it produced results
- **reasoning parts are dropped on replay** — persisted for display only
- `ask` parts replay as plain text and can never re-block

**AC:** A scripted fake LLM drives a full multi-round turn. A test asserts reasoning
parts never reach the provider payload.

### T2.3 — Checkpointing and recovery
Files: `src/turn/state.ts`

Turn rows checkpoint as the turn progresses. On boot, any turn still `running` is
marked `orphaned` and its session unblocked.

**AC:** Test simulates a mid-turn crash and asserts the session is usable after
restart with no stuck `pending` message.

### T2.4 — Interrupt and queueing
Files: `src/turn/queue.ts`

Interrupt signals the turn, kills the program's children, persists the partial
result with `interrupted: true`. A message posted mid-turn queues and drains into a
fresh turn.

**AC:** Interrupt mid-program leaves a well-formed transcript. Two rapid messages
produce two turns in order, never a lost message.

---

## M3 — Program worker

### T3.1 — Worker bridge
Files: `src/harness/protocol.ts`, `vm.ts`, `vm_worker.ts`

Protocol:
```
main → worker    {type:"run", code}
worker → main    {type:"host", id, fn, args}
main → worker    {type:"host_result", id, ok, value}
worker → main    {type:"log", line}
main → worker    {type:"abort"}
worker → main    {type:"aborted"}
worker → main    {type:"done", logs} | {type:"error", message, logs}
```

- `permissions: "inherit"`.
- Host function names are declared **once** in `protocol.ts` and imported by both
  sides. A test pins the two lists equal — a program that shadows a host name
  (`let bash = 1`) must fail pre-flight with a clear syntax error, not at runtime.
- `console.*` both streams (`{type:"log"}`) and batches into `logs`.
- **Exit trap:** `process.exit`/`Deno.exit` must throw a catchable error, not
  terminate the worker. With inherited permissions an uncaught exit can take the
  server down.
- **Wind-down:** track spawned children; on abort or wall-clock timeout, kill
  children first, *then* terminate the worker.

**AC:** Tests for: a program that throws surfaces the message; a program that spawns
a child and is aborted leaves no orphan process; `Deno.exit()` is catchable; a
shadowed host name fails pre-flight.

### T3.2 — Shell verbs
Files: `src/hostfn/shell.ts`, `src/hostfn/jobs.ts`

`bash` with interrupt propagation and the 60s auto-background handoff; `sh` for
concurrent execution returning `[{code, out}]`; the four job verbs with retained
output buffers and `job.spawned`/`job.exited` events.

Deterministic truncation for oversized output: head + tail verbatim with an explicit
omission marker.

**AC:** A 61-second command returns the auto-background message and the job is later
readable via `bashOutput`. `sh` never throws on non-zero exit.

### T3.3 — File verbs
Files: `src/hostfn/files.ts` (wires T0.5)

`view` renders `[path#TAG]` + numbered lines and records the tag for the session.
`write` creates or replaces. `patch` applies via the pure engine and echoes the new
TAG.

**AC:** A round-trip test: view → patch with empty tag → succeeds and echoes a new
tag → a second patch chains on that tag without viewing again.

### T3.4 — run_steps tool + prompt
Files: `src/prompt/*.md`, `src/prompt/assemble.ts`

The system prompt is composed of markdown sections assembled at turn start, with
delegation sections added conditionally by session kind (top-level vs. subagent vs.
nested).

**AC:** Assembly test asserting a subagent's prompt gets the nested-delegation
section and not the top-level one.

---

## M4 — Subagents

### T4.1 — Launch and lifecycle
Files: `src/agents/subagent.ts`

One launch path serving both modes. A subagent is a real session, `parentId: null`
(no inherited context), `originId`/`originMessageId` set for lineage, same
workspace, inheriting the spawning turn's MCP grants.

**AC:** A spawned subagent's thread contains only its task. Its lineage points at
the spawning turn.

### T4.2 — Blocking vs detached
`agent()` blocks and returns in-band; interrupting the spawner interrupts it.
`spawn()` returns a handle immediately and runs on regardless. `join()` claims a
detached result in-band. `adopt()` takes over a subagent session.

**AC:** Interrupting a spawner mid-`agent()` interrupts the child. A detached child
survives its spawner's turn ending.

### T4.3 — Caps
8 spawns per turn; 4 running concurrently tree-wide. Exceeding a cap fails **that
launch only** with a clear error.

**AC:** A `Promise.allSettled` fan-out of 12 gets 8 fulfilled and 4 rejected, and
the 8 results are intact.

### T4.4 — Reporting and wake
Files: `src/agents/notes.ts`

A detached subagent's report posts as a system note that wakes an idle spawner with
a fresh turn, or rides the queued drain if a turn is mid-flight.

**AC:** Idle spawner receives a new turn on child completion. Busy spawner receives
the note without a duplicate turn. **Test the failure paths too** — child errored,
child interrupted, child refused at launch, server restarted mid-flight — each must
reach the parent distinguishably.

---

## M5 — Workflows

### T5.1 — Meta extraction *(pure)*
Files: `src/workflow/meta.ts`

Balanced-brace scan for `export const meta = {…}` that skips string and template
contents and comments, so a description containing `{` cannot derail it. Returns the
literal text or null. Validate with Zod: `name`, `description`, optional `phases`.

**AC:** Tests with braces in strings, template literals, comments, and a missing
meta.

### T5.2 — Workflow worker
Files: `src/harness/wf_vm.ts`, `wf_worker.ts`

`permissions: "none"`. Bridges only `agent`/`phase`/`log`. In-worker combinators:
- `parallel(thunks)` — barrier; a throwing thunk resolves to `null`; never rejects.
- `pipeline(items, ...stages)` — **no barrier**; stages get `(prev, item, index)`; a
  throwing stage drops that item to `null` and skips its remaining stages.
- Same exit trap as the program worker.

**AC:** A pipeline test proves no barrier — item B reaches stage 3 while item A is
still in stage 1 (assert via ordering of recorded timestamps injected as a fake
clock).

### T5.3 — Structured output
Files: `src/workflow/schema.ts`

`agent(prompt, {schema})` forces the subagent to return JSON validated against the
JSON Schema; validation is at the tool-call layer so the model retries on mismatch.
`agent()` resolves to the parsed object.

**AC:** A subagent returning malformed JSON retries; a persistently-malformed one
fails the call with a clear error rather than returning a broken object.

### T5.4 — Journal and rerun
Files: `src/workflow/run.ts`

Journal each `agent()` into `workflow_agents` keyed by `hash(prompt + opts)`.
`rerun({id, script?})` replays hits from the source run instantly and re-runs only
changed keys. Mirror scripts to `~/.bough/workflows/<id>.js`.

**AC:** Rerunning an unchanged script issues **zero** live agent calls. Editing one
call's prompt re-runs exactly that call and everything downstream of it.

### T5.5 — Control and REST
`POST /workflows`, `GET /workflows`, `GET /workflows/:id`, and
`stop`/`pause`/`resume`/`rerun` plus a per-agent action route.

- **stop** kills the worker *and* interrupts in-flight subagent turns via the run's
  abort signal.
- **pause** gates new `agent()` calls while running ones finish.
- Run semaphore: 4 concurrent agents. Subagent caps do not apply inside a workflow.

**AC:** Stop leaves no running subagent turn. Pause lets in-flight agents finish but
starts no new ones.

---

## M6 — Program verbs

Independent tasks, parallelizable across people.

| Task | Files | Key requirement | AC |
|---|---|---|---|
| **T6.1 `ask()`** | `hostfn/ask.ts` | Memory-only registry; blocks until answered/declined/interrupted; settles as an `ask` part | A fresh client rebuilds the pending card from `GET /questions`; a restart leaves nothing stale |
| **T6.2 `state.*`** | `hostfn/state.ts` | Scoped to the **lineage root**, not session id; 16KB/key cap | A fork and its parent read the same store |
| **T6.3 `schedule.*`** | `schedules/` | Spec grammar + `nextRun(spec, now)` pure; catch-up advances **from now** | A ticker down through 5 slots fires **once**, not 5× |
| **T6.4 `image()`** | `hostfn/image.ts` | Copies to attachments, arrives as a next-turn system note | Missing/unsupported file throws catchably |
| **T6.5 `fetch()`** | `hostfn/net.ts` | 1MB cap with `truncated`, 30s deadline; non-2xx is data | Only transport failure/deadline/interrupt throws |
| **T6.6 artifacts** | `server/artifacts.ts` | Path confinement per session; filesystem is source of truth | Traversal blocked; listing survives a db reset |
| **T6.7 comments** | `server/comments.ts` | Widget injected at serve time; sidecar JSON **outside** the artifact dir | Sidecar never appears in `listArtifacts`; send posts a system note |
| **T6.8 jobs API** | `server/app.ts` | List, kill, read output for a session **and its subagents** | Killing a job emits `job.exited` |

---

## M7 — Integrations

### T7.1 — MCP core
Files: `mcp/config.ts`, `client.ts` (stdio), `manager.ts`, `status.ts`

Registry persisted under `~/.bough/mcp/`. Stdio servers spawn as subprocesses.
Per-session grants carry into subagents. `mcpStatus()` reports
`{registry, auth, active, connections}` — always live, never cached.

**AC:** A test echo server registers, connects, and answers a `mcp()` call. A server
that fails to start surfaces as a catalog status, **not a hang**.

### T7.2 — Remote MCP + OAuth
Files: `mcp/remote.ts`, `mcp/oauth.ts`

Streamable HTTP transport with OAuth/PKCE, tokens under `~/.bough/mcp/tokens/`,
callback route on the server.

**AC:** A 401 surfaces as "not authorized — /mcp auth `<name>`" in the turn's
catalog, never as a hang. An expired refresh token degrades the same way.

### T7.3 — LSP
Files: `lsp/`

Curated verbs with bough-owned names over the CLI backend. Lazy — nothing spawns
until the first call.

**AC:** An empty result is an ordinary answer, not an error. A dead backend is
reported once and does not retry every verb.

---

## M8 — History operations

### T8.1 — Branch seeding *(shared mechanism — build first)*
Files: `history/branch.ts`

`openBranch()` creates and announces a new session and returns a `Seeder`. Callers
add messages in thread order; every seeded message emits `message.started` so the
UI's existing reducers render it with no changes.

**Ordering invariant:** seeded messages use `Date.now()` and order by
`(created_at, rowid)`. Do **not** advance an artificial clock — a real turn started
afterwards must always sort after the seed.

**AC:** A seeded branch followed immediately by a real turn orders correctly.

### T8.2 — Fork · T8.3 — Compact · T8.4 — Sections · T8.5 — Extract · T8.6 — Move · T8.7 — Handoff

Semantics per spec §14. Shared constraints:
- Fork and compact operate only on the session's **own** messages; reaching into
  ancestor history is a 400 with a message telling the user to operate on the
  ancestor instead.
- Extract may pick **any** message in the visible thread, ancestors included, and
  may carry part indexes.
- Handoff never mutates the source; the draft lands on the new session.

**AC each:** the original session is byte-identical afterwards. Non-contiguous
compaction selections collapse each maximal run to one summary with unselected
messages copied verbatim around them.

### T8.8 — Changes
Files: `server/changes.ts`

`git diff <sessions.base>` plus untracked. Revert restores tracked paths from base
and deletes untracked ones, **per path**.

**AC:** Revert never touches a path the session did not change.

### T8.9 — Search
Files: `search/`

FTS index over message text, updated on insert. `GET /search?q=`.

**AC:** Rebuilding the index from scratch produces identical results to incremental
indexing.

---

## M9 — TUI

The previous TUI's single 3,600-line component is the thing this milestone exists to
avoid. **Hard rule: no component file over ~300 lines.**

### T9.1 — Transport and store
Files: `tui/api.ts`, `tui/events.ts`, `tui/store.ts`

`api.ts` is typed calls only. `events.ts` owns the SSE connection and reconnection.
`store.ts` holds state and reducers — **no React, no rendering, no ANSI**. This
separation is what makes the UI testable.

**AC:** Store reducers are unit-tested by feeding recorded event sequences with no
renderer mounted. A dropped SSE connection reconnects and reconciles without
duplicating messages.

### T9.2 — Rendering primitives
Files: `tui/lines.ts`, `format.ts`, `theme.ts`, `term.ts`, `selection.ts`, `mouse.ts`

Transcript→lines is a pure function: `(messages, width, theme) → Line[]`. Test it on
strings, not on a mounted tree.

**AC:** Wrapping, ANSI width, and selection math have direct unit tests.

### T9.3 — Components
`App` (composition only), `Composer`, `Transcript`, `StatusBar`, `Panel` + tabs
(sessions, tree, changes, model, MCP, skills, theme), `SubagentRail`, `Workflows`,
`Jobs`, `AskCard`, `DiffView`.

**AC:** Each renders from fixture state without a server.

### T9.4 — Input
Keymap table in one place. `@` file picker (respecting `.gitignore`, matching
directories too), `/` skill picker firing at any word boundary.

**AC:** Keymap is data, and a test asserts no duplicate bindings.

---

## M10 — Polish

| Task | Note |
|---|---|
| **T10.1 cheap tier** | Titles, ghost text, activity blurbs. Each **fails silently** and never blocks or delays a turn. One in-flight blurb per session — drop, don't queue. |
| **T10.2 skills** | Discovery, frontmatter, `${SKILL_DIR}`, per-skill MCP grants. Ship `history` only. |
| **T10.3 theme** | Persistence, HTTP route, live-preview-on-cursor that reverts on exit. |
| **T10.4 CLI** | `bough exec` — **open the event stream before posting**, or a fast turn finishes unseen. Exit 0/1/2 per spec §15. |
| **T10.5 install** | `install.sh` + launchd service. |
| **T10.6 README** | Describe what bough is, including the absence of any isolation boundary. No claim the code does not back. |

---

## 3. Testing strategy

| Layer | Approach |
|---|---|
| Pure functions | Direct unit tests. Patch engine, meta extraction, spec math, thread assembly, line wrapping. **Heaviest coverage here.** |
| Turn runner | Scripted fake `LlmClient`. Never touches the network. |
| Server | `createHandler(ctx)` with a fabricated ctx and in-memory db. No socket. |
| Workers | Real workers, trivial programs. Assert on the bridge protocol. |
| Subagents/workflows | Fake LLM + real orchestration. Failure paths are mandatory, not optional. |
| TUI | Store reducers over recorded event sequences; renderers over fixture state. |

**Non-negotiable:** every test runs offline and hermetically. A test that needs an
API key does not belong in `deno task test`.

---

## 4. Risks

1. **The patch engine is the safety mechanism.** Subagents share one checkout with
   nothing else enforcing separation. A rebase-vs-conflict bug means silent data
   loss across parallel agents. Over-test T0.5; treat a bug there as a stop-work.
2. **Delegated work reports no machine-readable result.** There is no acceptance
   gate, so structured output (T5.3) is the only typed signal a fan-out produces.
   Ship it with the first workflow, not later.
3. **Three provider paths drift independently.** Keep provider differences behind
   `LlmClient` (T2.1); if provider-specific handling leaks into the turn runner, it
   will leak everywhere.
4. **The cheap tier bills continuously.** Titles, ghost text and blurbs run on every
   round. A synchronous failure there stalls turns for a cosmetic feature — enforce
   fail-silent in review.
5. **Worker wind-down.** With inherited permissions, an unkilled child or an
   uncaught `Deno.exit()` can outlive the turn or take the server down. T3.1's
   teardown tests are load-bearing.
