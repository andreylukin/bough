# docs — the authoritative behavior spec and system map (Rust port)

Source of truth: `docs/spec.md` (what bough IS) and `docs/implementation-plan.md`
(module boundaries, invariants §6, testing strategy §7, risks §8). This file is the
system-level map for the Rust rewrite: every invariant a port must preserve, quoted
from the TS module headers where they are stated, plus the surfaces, wire shapes,
and edge cases mined from the tests. Other per-subsystem specs in `specs/`
refine individual modules; on conflict, THIS file's invariant list wins because it
is transcribed from the spec and the module headers, which the plan calls "not
rediscoverable from a spec."

---

## 1. Purpose & invariants

### What bough is

A coding agent that acts by **writing programs**. The model's only action per round
is `run_steps(code)` — one JavaScript program with loops/branching — executed by a
harness against the user's real machine. A headless loopback server (currently Bun,
`127.0.0.1:4321`) owns all state and execution; clients (TUI, `bough exec` CLI) are
views over it. Six principles (spec §2), all load-bearing:

1. **One program per round.** Control flow belongs in the program, not a chain of
   round-trips.
2. **No isolation boundary.** Programs run as the user with full authority. Host
   functions are convenience and session integration, **never confinement**. bough
   states this plainly rather than implying safety it does not provide.
3. **In place.** The agent edits the user's real checkout; `git diff` is the review
   payload, `git commit`/`push` the delivery.
4. **History is a tree.** Nothing is ever destructively rewritten (one narrow
   exception: take-back, §4 below). Compaction and forking produce new branches.
5. **The server is the system.** A client can crash or detach without affecting a
   running turn.
6. **Delegation is core.** Subagents and workflows are primary capabilities with
   first-class persistence, lifecycle control, and observability.

### The 16 hard invariants (plan §6, verbatim intent, renumbered with additions)

1. **Same-millisecond message ordering.** Messages order by `(created_at, rowid)`.
   Branch seeding stamps the REAL clock — never an advanced artificial one (`base+i`)
   — so a real turn started microseconds after a seed sorts after it by insertion
   order. Test-pinned: "a turn started in the same millisecond as a seed sorts after
   it"; "created_at still dominates rowid when timestamps differ".
2. **`process.exit` must be trapped in both workers.** Uncaught, it terminates the
   worker silently and strands the turn until wall timeout; with inherited
   permissions it can take the server down. In Rust: program execution is a child
   process — an exiting child must surface as a catchable program error, never kill
   the server.
3. **Kill children before terminating a worker.** Reverse order orphans processes.
4. **Reasoning parts are dropped on replay** — persisted for display; but when the
   provider gave a `meta` (signed thinking) payload it goes back UNTOUCHED, and only
   to the exact model that signed it. Never reconstruct thinking text into a block.
5. **`ask` parts replay as plain text.** They must never re-block on replay.
6. **A bus listener that throws must not break fan-out** to the others. Each
   subscriber is an SSE connection; one dying mid-close must not silence the rest.
7. **Auto-background at ~60s, don't kill.** `bash` past the threshold returns
   "…moved to background as bg_N"; the command keeps running and a `[background]`
   system note announces its exit. Programs never write sleep/poll loops.
8. **Schedule catch-up advances from *now*.** A server down through N slots fires
   once. The advance happens BEFORE the fire, so a throwing fire cannot re-fire
   30 s later. `dueSchedules(now)` returns each enabled row once, no inner loop.
9. **`Promise.allSettled`, not `Promise.all`, for fan-out.** A refused launch at a
   cap costs nothing: not the slot, not a sibling already started, not the per-turn
   budget. (Rust: `join_all` over results, never early-abort on one `Err`.)
10. **Open the event stream before posting** in the CLI, or a fast turn finishes
    unseen — there is no replay to catch it.
11. **One in-flight cheap-model call per session. Drop, don't queue.** The cheap
    tier (titles, ghost text, activity blurbs) "can only ever ADD something. It can
    never take anything away, delay anything, or fail anything" — resolves
    `None` on failure, never errors, never blocks a turn.
12. **Artifact comment sidecars live OUTSIDE the artifact directory**
    (`~/.bough/comments/<sessionId>.json`), or listing walks them.
13. **MCP state is never cached.** One status builder, fresh reads of registry +
    grants + credentials + live connections on every call. There is no MCP host
    function — tools are called via `bough mcp call SERVER TOOL '{json}'` in the
    shell, grant-enforced against `$BOUGH_SESSION`.
14. **An empty structural-search result is an answer, not an error.** (The lsp.*
    bridge was DELETED; `ast-grep` on PATH + prompt section replaced it. Do not
    port an LSP subsystem.)
15. **Workflow scripts are deterministic.** `Date.now()`, argless `new Date()`,
    `Math.random()` THROW inside the workflow worker, with a message saying to pass
    timestamps via `args` and vary prompts by index. Without this, journal replay
    silently serves stale results.
16. **`seq` is a dedupe key, not a resume cursor.** Process-monotonic, resets on
    restart. No SSE `id:` field is emitted (that would advertise resume). A
    reconnecting client re-fetches `GET /sessions/:id` and reconciles by message
    id. "Any state a client cannot rebuild from a fresh fetch is a bug in the event
    design, not something to fix with replay."

### Additional invariants quoted from module headers (must survive the port)

- `db/db.ts`: "**no raw SQL exists outside `db/`.** … the ordering rules … are
  properties of three `ORDER BY` clauses in one file, not a convention every caller
  has to remember."
- `db/migrate.ts`: "**migration is forward-only and idempotent.** … `schema.sql` is
  one block of `CREATE ... IF NOT EXISTS` … the table set is closed … A later task
  that needs a column stops and asks." A newer schema version is REFUSED at open.
- `schema/parts.ts`: "*derived visibility*: a Session carries its lineage (`kind`,
  `parentId`, `originId`) and nothing else. There is no `archivedAt`, no
  `deprecatedAt`, no hidden flag."
- `errors.ts`: "a domain module never constructs a `Response`" — every domain error
  carries its HTTP status; ONE catch in the router renders it. "**Error text is a
  product surface**": each error names what failed, the state that caused it, and
  the move that resolves it. "A message that says only 'failed' is a defect."
  (Spec §6 table: patch conflict names file+range+cause; stale tag explains the
  empty-tag escape; spawn-cap errors name WHICH cap; declined `ask` says the user
  dismissed it; timeout vs interrupt are distinguished and say what partial work
  survived.)
- `hostfn/patch.ts`: "a patch never silently lands on text its author did not
  read." Rebase when the file changed but patched ranges are untouched; conflict —
  naming file and range — when they were touched. **This module is the safety
  mechanism under shared-checkout delegation; a bug here is silent data loss.
  Treat a failure as stop-work** (plan §8.1).
- `hostfn/files.ts`: "**`[path#]` — the empty tag — always means the exact bytes
  this session last saw at that path, and a patch is refused outright when there
  are no such bytes on record.**" Per-session snapshot store keyed by path→(tag,
  bytes).
- `harness/protocol.ts`: "**host names are declared here exactly once, and both
  sides import them.**" The list is CLOSED. A program shadowing a host name
  (`let bash = 1`) fails pre-flight. A test pins host-side and worker-side lists
  equal.
- `agents/subagent.ts`: "**a subagent starts from nothing but its task.**
  `parentId: null` is the whole feature" — a parent pointer would hand the child
  the spawner's entire conversation via `threadFor`.
- `agents/notes.ts`: "**a note reaches the session exactly once, and never as a
  second concurrent turn.**" Idle spawner → note starts a fresh turn; busy spawner
  → note rides the queued drain; no third state.
- `hostfn/delegate.ts`: "**a blocking child is part of its spawner's turn; a
  detached one is not.**" `agent()`/`join()` hang an interrupt cascade on the
  spawning turn's signal and DROP it the instant they resolve (cascading into a
  finished child would flip a completed branch to `interrupted` and erase a
  persisted report).
- `turn/runner.ts`: "**a turn always ends, always ends visibly, and always ends
  exactly once.**" Stop-nudges are bounded and NEVER persisted (loop control, not
  content). Every turn must produce user-visible text.
- `turn/state.ts`: "**a session is never busy forever.**" Busy = `turns WHERE
  status='running'`; boot recovery marks survivors `orphaned`, unblocks the
  session, tells the user the server restarted. `step` checkpoints are the
  evidence a restart reads.
- `turn/queue.ts`: interrupt "is not a flag the loop checks between rounds" — a
  registry holds the live per-session `AbortController` plus cascade hooks that
  reach children and detached work.
- `llm/client.ts`: "**the turn runner must not know which provider it is talking
  to.**" Routing by model id ONLY: `openai:x` → OpenAI Responses API,
  `vendor/model` → OpenRouter, `@cf/…` → Workers AI, bare → Anthropic.
- `llm/stream.ts`: "**a stream that stops without its completion marker is a
  failure, not a short answer**" (stall timeout on every read; partial =
  retryable transport fault). "**A tool call with missing arguments was
  truncated; it is not a call with no arguments**" — retry, emit
  `message.retry`, bounded; exhausted retry = turn error.
- `llm/pricing.ts`: "**a price is a lookup, never a negotiation**" — vendored
  `pricing.json`; unknown model prices as `null` ("we don't price this"), never 0.
- `schedules.ts` + `hostfn/schedule.ts`: "**`next_run_at` is always computed FROM
  NOW, never from the stale stored value.**"
- `workflow/run.ts`: "**every `agent()` call is journaled by key before it runs,
  and a relaunch replays the longest UNCHANGED PREFIX**" — key =
  `hash(prompt + label + phase + RESOLVED model + schema)` (hashing only the
  script-named model let a repinned session replay stale answers). Only
  successful calls replay; a failed call re-runs live, and so does everything
  after the first changed call.
- `workflow/report.ts` / spec §8: "**Replay is always reported.** … A rerun that
  silently replayed nothing looks exactly like a successful rerun, so the count is
  the only thing that makes a key defect visible. This is a required part of the
  response, not a UI nicety." Alarm case: `available > 0, replayed: 0`.
- `workflow/control.ts`: "**a control verb is not a status write.**" `stop` kills
  the worker AND interrupts every in-flight subagent turn; `pause` gates NEW
  `agent()` calls and lets dispatched ones finish (they journal and will replay).
- `workflow/meta.ts`: "**`meta` is a pure literal, located by a scan that cannot
  be derailed by the script's own text, and evaluated by a parser that cannot
  execute anything**" — balanced-brace scan skipping strings/templates/comments.
- `history/fork.ts` (and compact/extract/move): "**the source session is
  byte-identical afterwards.**" AC is literal JSON-identity of source rows.
- `history/unsend.ts` (take-back): the ONLY destructive op. Three guards: the
  session's OWN messages only, a USER message only, the LAST user message only,
  within `UNSEND_MS` of send. Deletes the message + the partial answer, stops the
  turn, returns the text to the composer.
- `server/sessions.ts`: collapsing kinds are `subagent`, `workflow_agent`,
  `schedule_run` (note: schema adds `schedule_run` beyond spec §4's list).
  `POST /sessions` REFUSES to create a collapsing kind (it would be invisible to
  every listing — no originId in the creation body).
- `server/changes.ts`: "**revert never touches a path the session did not
  change**" — requests are intersected with the CURRENT change set; outside paths
  are reported back as skipped, not restored. Non-git workspace: `available:
  false` + reason sentence, never an empty diff.
- `server/search.ts`: "**the search index is never load-bearing.** A failure to
  index must never fail the write that triggered it." (`searchSafeDb` wraps
  `indexMessage` to report-and-swallow.)
- `server/main.ts`: "**Loopback only.** The listener binds `127.0.0.1` with no
  override." `BOUGH_PORT` moves the port; `BOUGH_HOME` relocates the whole data
  tree (this is what lets the Rust port run beside the live install).
- `paths.ts`: "**no module builds a `~/.bough` path by string concatenation**";
  `confine(root, candidate)` throws on traversal (tests cover `..`, absolute
  escapes, symlink-shaped inputs).
- `prompt/assemble.ts`: "**the prompt IS the capability grant.** A section that
  documents a host function is included only when that host function is actually
  bridged for this turn, and a bridged function always has its section."
- `prompt/project.ts`: bough reads **`AGENTS.md` — never `CLAUDE.md`**. Tiers:
  `$BOUGH_HOME/AGENTS.md`, then every `AGENTS.md` from the git root down to the
  workspace dir, nearest LAST. Read **per turn**. Injection is reported (dim `#`
  transcript row, `/rules`, `[rules]` change-line).
- `hostfn/ask.ts`: "**a hold is memory-only, and it always settles**" — restart
  leaves nothing stale; `GET /questions` answers from memory (a reconnect path,
  not a feed).
- `hostfn/state.ts`: "**the store is keyed by the LINEAGE ROOT, never by the
  session id**" — 16 KB/key, any JSON; fork/compaction/subagent of one piece of
  work share it.
- `hostfn/artifact.ts`: publishing never touches the workspace; names and session
  ids are CONFINED to their directory. HTML gets the comment layer injected AT
  SERVE TIME only — bytes on disk stay exactly what the agent wrote.
- `mcp/config.ts` / `manager.ts`: "**being registered grants nothing**"; grants
  are read FRESH per call; the SPAWNING turn's grant carries into subagents
  (child neither resolves its own — nothing — nor re-reads the file — too much).
- `mcp/client.ts` / `remote.ts`: "**a server that does not work fails, by name,
  in bounded time**" — never a hang; a 401 is "not authorized — /mcp auth
  <name>", a QUESTION, not a fault.
- `worker/activity.ts` (cheap tier): "**one in-flight blurb per session — rounds
  that land while it is busy are DROPPED, not queued.**"
- `tui/store.ts`: reducer is pure and **idempotent under re-delivery**.
- `scratch.ts`: per-session scratchpad exists so temp files pollute neither the
  checkout nor shared `/tmp` (deliberately NOT under `/tmp` — reboot/tmpfiles
  reaping).

### Non-goals (spec §17 — decisions, not omissions; do NOT port)

No sandbox/egress proxy/credential gating; no acceptance gate; no local
inference; no semantic recall (keyword FTS only — note: an OPTIONAL vector layer
over the command-history memory exists via sqlite-vec/lembed in a separate
`embeddings.db`, graceful-absence, skippable); no output digestion (deterministic
truncation + spill-to-scratch file for oversized output); no `edit()`/`read()`;
no archive/deprecate/purge; no per-agent worktrees or file leases; no
benchmarking/metrics endpoints; no remote access; no web UI; no workflow nesting
or token budgets; no non-git snapshotting; **never auto-compact** ("HARD NO").

---

## 2. Public API

### Model-facing tool surface (the ENTIRE model API — exactly two tools)

```jsonc
{ "name": "run_steps",
  "input_schema": { "properties": {
      "code": {"type":"string"},          // the program; host fns are pre-injected globals
      "done": {"type":"boolean"} },       // advisory: work complete after this program
    "required": ["code"], "additionalProperties": false } }
{ "name": "stop",                          // end the turn; call after final text, same response
  "input_schema": { "type":"object", "properties": {}, "additionalProperties": false } }
```

`run_steps` returns the program's `console` output or the error that ended it.
Programs are syntax-checked pre-flight (malformed = fast round-trip, no worker).

### Host functions (closed list, `harness/protocol.ts` — declared once, both sides)

`bash(cmd[, tags])` · `sh(...cmds)` · `bashBg(name, cmd)` · `bashOutput(id)` ·
`bashWait(id)` · `bashKill(id)` · `view(path)` · `patch(input)` ·
`write(path, content)` · `agent(task,{name})` · `spawn(task,{name})` ·
`join(sessionId)` · `adopt(sessionId)` · `workflow.{start,status,stop,list,rerun}` ·
`ask(q,{options})` · `state.{get,set,list,delete}` ·
`schedule.{list,add,enable,disable,remove}` · `artifact(name, content)`.
Program parameters additionally bind `console` (streams `tool.log` + batches into
tool result) and `require`. `bash(cmd, tags)` records into the command-history
memory (tags REQUIRED in the current tree's prompt contract).

Semantics worth pinning: `sh` runs concurrently, returns `[{code, out}]` in
order, NEVER throws on non-zero exit. `bash` carries the turn's interrupt and
auto-backgrounds at ~60 s. `agent()` blocks, returns
`{sessionId, ok, report, changedFiles}`; `spawn()` returns `{sessionId, title}`
immediately; detached reports arrive as system notes. `ask` throws catchable
"user declined" on dismissal. `artifact` returns `{url, href}`.

### Workflow script surface (the workflow worker binds EXACTLY these)

`agent(prompt, {label, phase, model, schema})` (throws on failure) ·
`parallel(thunks)` (barrier; thrower → `null`; never rejects) ·
`pipeline(items, ...stages)` (NO barrier; stage gets `(prev, item, index)`;
thrower drops item to `null` and skips its remaining stages) · `phase(title)` ·
`log(msg)` (both fire-and-forget) · `args`. Plus `export const meta =
{name, description, phases}` as a pure literal, extracted host-side.

### HTTP surface (loopback JSON API + SSE; the route table is append-only)

Sessions: `GET /sessions` (derived visibility) · `GET /sessions?originId=`
(drill-in) · `GET /sessions/:id` → `{session, thread}` · `POST /sessions` ·
`POST /sessions/:id/messages` (202; queues if a turn is running) ·
`POST /sessions/:id/interrupt` (200 `{interrupted:false}` when idle — an answer,
not an error) · fork/compact/sections/extract/move/handoff/unsend history routes ·
`GET/POST /sessions/:id/changes[/revert]` · `GET /sessions/:id/jobs[/:jobId/output]`
(non-destructive read; job list is TRANSITIVE over subagents, rows carry owning
`sessionId`; kill is by id across sessions) · `GET /questions` +
`POST /sessions/:id/questions/:qid` · `GET /events[?sessionId=]` (SSE) ·
workflows list/start(201)/inspect/stop/pause/resume/rerun + saved workflows ·
MCP registry/enable/auth/oauth-callback/status · `GET /models`, model defaults
(`~/.bough/model.json`), `GET/PUT/DELETE /theme`, `GET /skills[/:name]`,
`GET /fs/files?dir=` (git ls-files-backed @-completion; non-repo → empty list),
`POST /attachments`, artifacts serve routes, `GET /search`.

### Event stream

`BoughEvent` envelope: `{seq, ts, type, data}`; closed name set:
`session.created` `session.updated` `session.activity` `message.started`
`message.delta` `message.part` `message.finished` `message.retry` `tool.log`
`turn.finished` `ask.question` `job.spawned` `job.exited` `workflow.updated`
`workflow.agent` `workflow.log`. (`tool.log` exists in the tree beyond spec §3's
list.) Envelope is parsed client-side; payloads are typed, not re-validated.

### CLI

`bough exec [flags] "prompt"` — stream opened BEFORE post; exit 0 done / 1 turn
error / 2 usage-or-connection. Unknown flag = error (also in the TUI's argv).
Other subcommands: `bough mcp <verb>` (incl. `doctor`, `call`), `bough tags`
(`show`/`sql`/`similar`/`stats`, read-only DB open), `bough patterns [FILE]`
(log compression), `bough sync-mcp`, `bough logs`.

---

## 3. Data structures

### SQLite tables (`db/schema.sql` — closed set, `CREATE IF NOT EXISTS` only)

`sessions` (id, title, created_at, kind: root|fork|compaction|subagent|
workflow_agent|schedule_run, parent_id, origin_id, origin_message_id, workspace,
origin_dir, model, effort, draft, base, usage/cost aggregates, context gauge) ·
`messages` (session_id, role: user|supervisor|system, pending flag, parts JSON,
created_at; ordered `(created_at, rowid)`) · `turns` (status: running|done|error|
interrupted|orphaned; `step` checkpoint; usage per turn) · `workflows` ·
`workflow_agents` (run_id, idx, key, status queued→running→done/error/stopped/
cached, label, phase, session_id, result) · `session_state` (lineage-root KV) ·
`schedules` (title, prompt, workspace, spec, enabled, next_run_at) ·
`command_history` + `command_tags` + `command_dirs` + `command_history_fts`
(tag memory) · `messages_fts` (FTS5). Vector layer in SEPARATE `embeddings.db`.
Schema version stamped; newer version refused. **Frozen**: a task needing a
column stops and asks (the schema has had exactly two sanctioned ALTERs; columns
go at the END).

### Parts (discriminated union on `type`)

`text` · `reasoning` (persisted; replay only via untouched provider `meta`, only
to the signing model) · `tool_call` · `tool_result` (carries `interrupted: true`
on interrupt) · `image` (stores a PATH under `~/.bough/attachments/`, never
bytes; lost file replays as placeholder text) · `ask` (replays as plain text).
Wire field names are the Zod schema's — the TUI, CLI, DB and router share ONE
definition; the Rust port must keep serde names byte-identical to the TS wire.

### Replay mapping (stored parts → provider messages)

user → one user message of text blocks (+ images); supervisor → assistant
(text + tool_use) then user of tool_result blocks; system messages render
distinctly and replay as user-side text; reasoning dropped (or signed-meta
echoed); ask → plain text.

### Patch grammar (custom, owned — do not substitute a diff library)

```
[path#TAG]            TAG optional; empty = "the version I just viewed"
SWAP a.=b: / DEL a.=b / INS.PRE n: / INS.POST n: / INS.HEAD: / INS.TAIL:
+new text lines       (bare + = blank line; no - rows)
```
All line numbers in VIEWED coordinates; compute all edits against the original,
materialize once. Multi-file all-or-nothing. `view()` returns `[path#TAG]` header
+ `N:text` numbered lines; `patch` echoes the new TAG for chaining.

### `~/.bough` layout (all via `paths.rs` accessors; `BOUGH_HOME` overrides root)

`bough.db` · `embeddings.db` · `artifacts/<sessionId>/` · `comments/<id>.json` ·
`attachments/` · `workflows/<id>.js` + `workflows/saved/<name>.js` · `skills/` ·
`theme.json` · `model.json` · `env` (server-sourced keys) · `mcp.json` registry ·
`mcp-auth.json` · `AGENTS.md` · scratch dirs per session.

---

## 4. Behaviors & edge cases (mined from tests; a naive port gets these wrong)

**Turn loop.** Queued messages drain into a FRESH turn (two rapid messages → two
ordered turns, no loss). Interrupt mid-program leaves a well-formed transcript.
Truncated tool call retries (bounded) instead of executing `{}`. Context overflow
fails the turn with an error naming the limit — no silent summarize. Token counts
recorded per turn from provider usage; reasoning and cache tokens tracked
separately. Stop-nudges never persisted.

**Patch engine** (61 tests; highlights): CRLF and BOM don't change a file's tag
identity; line-ending style and missing trailing newline survive a patch; a
trailing newline is not a line; Codex-style envelopes swallowed; pasting view()
output back is diagnosed; one path with two different tags in one patch refused;
op order in patch text irrelevant; INS gap-ordering at one anchor is fixed and
documented; INS anchored inside a replaced span rejected; SWAP with no body
rejected ("DEL is how you remove lines"); empty INS body is a no-op; empty file
rejects line-anchored ops by name; overlap and out-of-bounds rejected (bounds
judged in VIEWED coordinates); rebase shifts ops below an insert/delete;
explicit tag naming a superseded-but-known version still rebases; conflict when
patched line rewritten/deleted or lines inserted INSIDE the span; ALL conflicting
ranges listed; one touched range refuses the whole file's other clean ops;
multi-file: any conflict/stale-tag/out-of-range discards ALL files; HEAD/TAIL
never conflict (they name no line); `applyPatch` mutates neither argument.

**Files.** Patching a never-viewed path is refused. A stale tag error spells out
the empty-tag escape hatch. Patch chains: second patch written against the
first's echoed tag without re-viewing.

**Shell.** Oversized output: head+tail kept verbatim with explicit omission
marker; over ~20k chars additionally spills the TRUE full output to a scratch
file and tells the turn where (two shipped bugs: the spill file must be the
command's real output, not the truncated buffer, and no chunk may be dropped).
Auto-backgrounded command later readable via `bashOutput` (output since last
call + `[running]`/`[exited]` status line). `bashBg` is spawned WITHOUT the
turn's abort signal (survives stop); auto-promoted foregrounds keep running.

**DB.** `threadFor` = ancestors root→parent then own, three levels tested;
subagent's thread is its own messages only; `ancestorChain` root-first inclusive,
unknown id → empty; `busySessionIds` reads running TURNS, not pending messages;
`treeUsage` rolls up delegated branches and EXCLUDES forks; a fork/compaction
lists ancestors' workflow runs, a subagent does not; migration idempotent across
two opens; FTS rebuild from scratch ≡ incremental; malformed FTS query → 400
that says what to do.

**Subagents.** Caps: 8 spawns/turn, 4 concurrent tree-wide; allSettled of 12 →
8 fulfilled 4 rejected, the 8 intact. Depth 1 further delegation, blocking only.
Interrupting a spawner mid-`agent()` interrupts the child; a detached child
survives its spawner's turn ending. The four failure cases — child errored,
child interrupted, launch refused at cap, server restarted mid-flight — must
each reach the parent DISTINGUISHABLY.

**Workflows.** Pipeline has no barrier (B reaches stage 3 while A is in stage 1);
rerun of unchanged script issues ZERO live calls; editing one prompt re-runs
exactly that call and everything after; a failed call re-runs live plus
everything after; journal key changes with every field that changes what the
agent is asked; journal rows written BEFORE the semaphore is acquired (saturated
runs show `queued`); prefix comparison is by coordinate (nested parallel-inside-
pipeline keeps distinct coordinates; a swap classifies as MOVE not edit); pause
gates sequential scripts too (regression test); stopping a paused run settles
every row, no orphans; boot recovery orphans runs from a dead process; a
finished run notifies its owning session; script-returns-nothing still reports
what agents said; run semaphore 16 (fewer on small machines); 1,000-agent
lifetime backstop; subagent caps do NOT apply inside; workflow worker binds
exactly the documented names; scripts that don't parse are refused before a
worker spawns; schema-constrained `agent()` retries on mismatch, throws on
exhaustion, rejects unsupported schemas at SUBMIT time (no recursion, no
numeric/length constraints, `additionalProperties: false` required). Size
guideline small/medium/large advisory; large-run warning advisory.

**History ops.** Fork/compact operate only on the session's OWN messages —
ancestor picks are a 400 telling the user to operate on the ancestor. Both
branch a SIBLING (`parentId = target.parentId`) so `threadFor` re-supplies
shared ancestors. Compaction: non-contiguous selections collapse each maximal
run to one summary in place, unselected messages copied verbatim. Extract →
fresh ROOT, may pick anything visible incl. ancestors, may carry part indexes.
Move-into is a COPY (name lies). Handoff seeds NO messages — result lives in the
new session's `draft`. Compact + handoff are SCOUTED by a bash subagent reading
the checkout NOW (pinned cheap model `BOUGH_COMPACT_EXPLORE_MODEL`, default
`gpt-5.6-luna`); scout failure degrades to the unscouted prompt. TUI `/compact`
IS handoff. Sections is stateless: gists in thread order, reply index i = turn i,
no table/cache.

**Schedules.** Spec grammar `every:<N><m|h|d>` (N≥1) or `daily@HH:MM` local
wall clock; ~30 s ticker; each firing opens a fresh root session (kind
`schedule_run`, collapses under origin) and report-back notes wake the creator.

**Server/TUI oddments.** Interrupt when idle = 200 `{interrupted:false}`.
Skills: folder name wins over frontmatter `name:`; malformed SKILL.md listed
WITH its `error`, never half-parsed into the prompt. Theme preview reverts on
EVERY way of leaving the tab (revert lives in the panel reducer; `cancel()`
idempotent). Model picker sets BOTH the session pin and the new-session default,
touches no other session. `GET /models` is answered server-side (the key lives
in the server's env, not the TUI's). Display width is never `String.length`
(SGR/OSC-8/CJK); the transcript is pre-wrapped data (`VLine[]`) before any
rendering; store reducer idempotent under redelivery; terminal restored on every
exit path.

---

## 5. Dependencies (docs is the root: every subsystem implements it)

`spec.md`/`implementation-plan.md` are implemented by, and this spec constrains:
`schema` (wire) ← everything; `db` ← server/turn/history/agents/workflow;
`bus` ← server/turn/agents/workflow; `paths`/`errors`/`types` (ctx seams) ←
everything; `llm` ← turn/worker(cheap)/history(handoff,compact,sections);
`turn` ← server, agents, workflow/control; `harness` ← turn (program worker),
workflow (wf worker); `hostfn` ← harness bridge (never imports server/tui —
takes a ctx object; the module-boundary rule); `agents` ← hostfn/delegate,
workflow/control; `workflow` ← server/workflows, hostfn; `history` ← server
routes; `mcp` ← server, prompt, cli; `prompt`+`skills` ← turn; `tui`/`cli` ←
HTTP surface only. Shared frozen files in TS (deno.json / protocol.ts /
schema.sql / app.ts route table) map to: workspace `Cargo.toml`, one
`protocol.rs` const list, one `schema.sql`, one append-only router module.

---

## 6. External deps → Rust equivalents

| TS/Bun dependency | Rust replacement |
|---|---|
| Bun HTTP server + SSE | `axum` (+ `tokio`), SSE via `axum::response::sse` (emit NO `id:` field) |
| `node:sqlite` / Bun sqlite | `rusqlite` (bundled feature); FTS5 built in |
| Zod wire schemas | `serde` + `serde_json`; validation via `garde`/manual; keep field names identical |
| Bun `Worker` (program worker, inherit perms) | **No JS engine in-process**: spawn a sidecar runner as a child process. v1 options: (a) keep a tiny Bun/Node runner binary speaking the same JSON protocol over stdio, or (b) embed `rquickjs`/`deno_core`. The postMessage protocol (`run`/`host`/`host_result`/`log`/`abort`/`aborted`/`done`/`error`) becomes newline-JSON over stdio |
| Workflow worker (5-name world) | Same runner in "workflow" mode; determinism traps (Date.now etc. throw) implemented in the runner's JS prelude |
| Anthropic/OpenAI/OpenRouter SDKs | `reqwest` + hand-rolled SSE parsing (stall timeout per read is MANDATORY); one `LlmClient` trait |
| `zodOutputFormat`/`messages.parse` | JSON Schema via `schemars`/`jsonschema` crate; retry-on-mismatch at the tool-call layer |
| MCP SDK (stdio + Streamable HTTP + OAuth/PKCE) | `rmcp` (official Rust MCP SDK) or hand-rolled JSON-RPC over `tokio::process`; `oauth2` crate for PKCE |
| Ink/OpenTUI | `ratatui` + `crossterm` (mouse capture, bracketed paste, OSC 8/52, kitty keys need manual passthrough — crossterm covers most) |
| `string-width`/`slice-ansi`/`strip-ansi` | `unicode-width` + `ansi-width`/`vte`-based slicing (must handle SGR, OSC 8, CJK) |
| `Bun.spawn` / process groups | `tokio::process::Command` with `process_group(0)`; kill the GROUP (children first) |
| git subprocess (`ls-files`, `diff`, `checkout <base> --`) | keep shelling out to `git` (`std::process`); do not adopt libgit2 for v1 |
| `chokidar`-style none needed | — (skills/AGENTS.md are read fresh per request/turn; no watcher) |
| hash for journal keys | `sha2` or `blake3` (stability across runs is what matters, not speed) |
| sqlite-vec + lembed (optional vector layer) | SKIP in v1 (graceful absence is already the contract) |
| clipboard (`pbpaste` image-first order) | `pbpaste`/`osascript` shell-out on macOS; image before text |

Bun-specific hazards: `setCustomSQLite` before-first-open has no Rust analog
(rusqlite bundles); worker `permissions:"inherit"` semantics disappear —
document that the runner child has full user authority by design; `Bun.file`
relative-path-vs-workspace footgun (worker inherits SERVER cwd, `view("src/x")`
≠ raw-read `"src/x"`) must be preserved or deliberately fixed and re-documented
in the prompt.

---

## 7. Suggested Rust layout (cargo workspace at the repo root)

```
crates/
  bough-core/       schema (serde types, parts union), errors (Status-carrying
                    enum + IntoResponse in server only), paths (confine),
                    types (AppCtx/TurnCtx as trait objects: Db, Bus, LlmClient,
                    Clock, CheapTier)
  bough-db/         rusqlite wrapper; ONE module with the three ORDER BY rules;
                    schema.sql embedded; forward-only idempotent migrate
  bough-llm/        LlmClient trait + anthropic/openai/openrouter impls,
                    stream.rs (stall timeout, truncated-tool-call detection),
                    pricing.rs (vendored pricing.json via include_str!)
  bough-patch/      pure patch engine (parse/check/rebase/apply) — port FIRST,
                    port its 61 tests verbatim; stop-work on any failure
  bough-harness/    protocol.rs (closed host-fn list), runner supervisor
                    (spawn child, stdio JSON bridge, wind-down: kill process
                    group then reap), wf runner mode
  bough-hostfn/     shell/jobs/files/ask/state/schedule/artifact/delegate —
                    each takes &TurnCtx, no server imports (enforced by crate DAG)
  bough-turn/       runner (loop as async fn, scripted-fake-client tests),
                    replay, state (boot orphan recovery), queue (per-session
                    tokio::sync::Mutex + CancellationToken registry + cascade
                    hooks)
  bough-agents/     subagent launch, caps (atomic counters), notes (wake rule)
  bough-workflow/   meta scan, journal + prefix replay, control, schema, saved
  bough-history/    branch(Seeder)/fork/compact/sections/extract/move/handoff/
                    unsend; explore scout
  bough-server/     axum app: append-only router, sse, sessions, changes,
                    artifacts, comments, jobs, questions, skills, theme,
                    models, search, workflows, mcp routes; main.rs binds
                    127.0.0.1 only
  bough-mcp/        config/registry, stdio client, remote+oauth, status builder
  bough-tui/        ratatui: api.rs (only URL knower), events.rs (reconnect =
                    refetch), store.rs (pure reducer, replayable), lines.rs /
                    format.rs (pure string layer), forest.rs, components/,
                    keys.rs (bindings as data), term.rs (caps as pure fn of env)
  bough-cli/        exec (stream-before-post), mcp verbs, tags, patterns
```

Traits: `LlmClient` (the ONE provider seam), `Db`, `Bus`, `Clock`, `CheapTier`
(methods return `Option`, never `Err`), `AgentRunner` (workflow engine's hole).
Async boundaries: everything server-side on tokio; the patch engine, meta scan,
schedule math, thread assembly, prompt assembly, TUI reducer/format stay
**sync and pure** (heaviest test coverage, per plan §7). Interrupt = per-session
`CancellationToken` with cascade hooks; bus = `tokio::sync::broadcast` wrapped
so a lagging/dead receiver never stalls the sender (invariant 6).

Testing: every test offline and hermetic; fake `LlmClient` scripts; in-memory
sqlite; handler tests via `tower::ServiceExt::oneshot` (no socket); TUI reducers
over recorded event sequences.

---

## 8. v1 scope cut

**Core (the working vertical slice — model writes a program, patches a file,
streams output):** paths/errors/schema/db/bus · patch engine · files+shell
hostfns (with 60 s auto-background + truncation) · llm client (Anthropic first,
trait in place) + stream stall/truncation handling · turn runner/replay/state/
queue · program-runner bridge with exit trap + child wind-down · prompt assembly
(core sections + AGENTS.md) · sessions/messages/events/interrupt routes · TUI
chat+composer+store+events · `bough exec`.

**High (daily driver):** subagents (all four verbs, caps, notes, failure
matrix) · history fork/unsend/handoff · changes rail + revert · jobs API + rail ·
ask() · skills · model picker + defaults + pricing · schedules · search (FTS).

**Later:** workflows (engine, journal, prefix replay, control, saved, report) —
large but self-contained; compact/sections/extract/move · MCP (stdio first,
remote+OAuth after) · theme live-preview · attachments/images · artifacts +
comment layer · cheap tier (titles/ghost/blurbs — each fails silently; trivial
to stub as `None`) · command-tag memory + `bough tags`/`patterns`/`sync-mcp`.

**Stub in v1:** cheap tier returns `None` everywhere (contract already allows
it); vector/embeddings layer absent (graceful-absence is the contract); OpenAI/
OpenRouter clients behind the trait return "provider not configured"; sections
returns 501; `bough patterns` omitted; MCP status returns empty registry.

Cut nothing from the invariant list in §1 — the ordering, wind-down, replay,
and catch-up rules are load-bearing even in the minimal slice.
