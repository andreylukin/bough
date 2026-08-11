# Port spec: `db` subsystem (SQLite persistence + zod wire schemas)

Source files (all under `src`):
- `db/db.ts` (1512 ln) — the one place in the tree that speaks SQL; `SqliteDb` implements the `Db` port.
- `db/schema.sql` (329 ln) — the frozen schema, one `CREATE ... IF NOT EXISTS` block.
- `db/migrate.ts` (157 ln) — idempotent forward-only migration + 3 sanctioned reshapes.
- `db/extensions.ts` (50 ln) — process-wide SQLite loadable-extension capability decision.
- `history/embed.ts` (181 ln) — optional vector layer (sqlite-vec + sqlite-lembed) in a **separate** `embeddings.db`; read here because it is the only extension consumer and half its contract lives in the db layer's design.
- `schema/parts.ts` (516 ln) — zod wire contract: Part union, Message, Session, Turn, Usage, Schedule, Workflow*, BackgroundJob, AskQuestion.
- `schema/events.ts` (195 ln) — SSE envelope + closed event-name set.
- `schema/requests.ts` (360 ln) — one zod schema per REST request body.
- `types.ts` (lines 40–310) — the `Db` port interface + row-adjacent structs (`CommandRecord`, `UsageTotals`, `SearchHit`, `PriorFailures`, `TagDiversityDay`, `TaggedCommand`, `CommandTagRow`, `SessionRuntime`).
- `paths.ts` — `boughHome()` = `$BOUGH_HOME` else `~/.bough`; `dbPath()` = `$BOUGH_DB` else `<home>/bough.db`.

---

## 1. Purpose & invariants

This subsystem is bough's entire persistence layer plus the typed wire contract every other module codes against. Invariant comments, verbatim:

`db/db.ts` header:
> The only place in the tree that speaks SQL.
> The invariant: **no raw SQL exists outside `db/`.** Every read and write in the system goes through a typed method here, which is what makes the ordering rules below enforceable at all — they are properties of three `ORDER BY` clauses in one file, not a convention every caller has to remember.
>
> The three ordering rules, in order of how much depends on them:
> 1. `messagesFor` orders by `(created_at, rowid)`, never `created_at` alone. Branch seeding writes with a real clock rather than an advanced artificial one (plan §6.1), so a turn started immediately after a seed lands in the *same millisecond* — `rowid`, the insertion order, is the only thing that keeps it after the seed. Sorting by timestamp alone reorders history under the user.
> 2. `threadFor` is every ancestor's messages root→parent, then the session's own. This is what makes fork and compaction cheap: a branch parented at the target's parent inherits the shared ancestors for free and seeds only the rest (spec §14).
> 3. `ancestorChain` walks `parent_id` to the lineage root and returns it root first, inclusive of the session itself. `session_state` is scoped to `chain[0]`, so a fork and its parent read one store (spec §6).
>
> What this layer is NOT: a place for policy. `listSessions` returns every session and the *caller* derives visibility from `kind` + `origin_id` — there is no archive, deprecate or purge column to filter on, because there is no such action (spec §4, §17). Likewise there are no embeddings: cross-session search is keyword FTS over a text projection of `parts`.
>
> Injection: the database path and the clock are constructor arguments. `updateTurn` is the one method that stamps a time of its own, and it stamps `#now()` — so a test drives checkpoint ordering without sleeping. Everything else takes its timestamps from the caller.
>
> Storage conventions, matching `schema.sql`: timestamps are epoch ms integers, booleans are 0/1, and anything structured is JSON text.

`db/schema.sql` header:
> Invariant this file holds: **the table set is closed.** Every column any task through M10 needs is created here, in one statement block, applied once at open. There is no migration ladder and no `ALTER TABLE ... ADD COLUMN` swallowing a duplicate error … A later task that needs a column stops and asks (plan §4).
> Second invariant: message ordering is `(created_at, rowid)` everywhere, never `created_at` alone.
> Conventions: timestamps are epoch ms INTEGER, booleans are 0/1 INTEGER, and anything structured is a JSON TEXT column. Foreign keys are declared and `PRAGMA foreign_keys = ON` is set at open.
> What is deliberately absent: `archived_at`/`deprecated_at` on sessions (visibility DERIVED from kind + origin_id); `message_embeddings` (message search is keyword FTS, never vectors — a vector index may exist over the command-history tables only, as an optional runtime layer); a jobs table (background shells die with the server; persisted rows would lie after restart, spec §9); artifacts/skills tables (filesystem-backed, survive a db reset, spec §4).

`db/migrate.ts` header:
> The invariant: **migration is forward-only and idempotent.** Applying it to a fresh file and applying it to a file that already has every table must leave the same database and must never fail. … Nothing in this file swallows an error.
> `user_version` is the forward-only guard … a database written by a future bough is refused loudly at open rather than silently half-read.

`db/extensions.ts` header:
> SQLite loadable-extension capability, decided once per process. Bun's `Database.setCustomSQLite` must be called BEFORE the first `Database` is opened, exactly once … On macOS it is the only way to get extension loading at all (Apple's system SQLite compiles it out) … Everything is graceful-absence … Nothing here throws.

`schema/parts.ts` header:
> The wire contract. Every shape that crosses server↔client, server↔db, or server↔worker is declared here once … The invariant this module holds is *derived visibility*: a Session carries its lineage (`kind`, `parentId`, `originId`) and nothing else. … Second invariant: parts are a discriminated union on `type`. … Third: image bytes never live in the parts JSON.

`schema/events.ts` header:
> The invariant: **events are display transport, never the source of truth.** `seq` is a process-monotonic counter that resets on server restart, so it is a dedupe key and NOT a resume cursor (plan §6.16). A reconnecting client re-fetches `GET /sessions/:id` and reconciles by message id.

`schema/requests.ts` header:
> One Zod schema per body, parsed at the router edge and nowhere else … these schemas are the ONLY place a 400 is decided. Semantic 400s belong to the domain module.

`history/embed.ts` header:
> The optional vector layer over the command-history memory: local embeddings, generated INSIDE SQLite. … Vectors live in their OWN database file (`~/.bough/embeddings.db`), never in bough.db: every other connection in the system … lacks the vec0 module, and a virtual table they cannot even parse must not sit in a file they walk. The embed connection ATTACHes bough.db and treats it as read-only by discipline; embeddings.db is fully derived state and can be deleted freely.

---

## 2. Public API

### `db/db.ts`
- `openDb(path = dbPath(), opts?: DbOptions) -> SqliteDb` — opens (creating parent dirs unless path is `":memory:"` or starts with `"file:"`), sets `PRAGMA foreign_keys = ON`, runs `migrate()`.
- `interface DbOptions { now?: () => number }` — injected clock; only `updateTurn` reads it; default `Date.now`.
- `class SqliteDb implements Db` — the concrete port. `close(): void`.

All methods are **synchronous** (bun:sqlite is sync). The `Db` port (`types.ts:185`) is the contract Rust must implement; grouped:

**Sessions**
- `createSession(s: Session) -> Session` — INSERT then read-back (returned row is *as stored*, so caller can never hold a value the db didn't keep). Only the 13 identity/pin columns are inserted; usage columns start NULL.
- `getSession(id) -> Session | undefined`
- `getSessionRuntime(id) -> {workspace: string|null, base: string|null}` — non-wire runtime facts; unknown id → both null.
- `listSessions() -> Session[]` — `ORDER BY created_at DESC, rowid DESC`; **no visibility filter**.
- `sessionsByOrigin(originId) -> Session[]` — `ORDER BY created_at, rowid` (drill-in).
- `ancestorChain(id) -> Session[]` — walk `parent_id` iteratively with a `seen` set (cycle from a bad write must not hang the server); returns root-first inclusive; `[]` for unknown id.
- `setSessionTitle/Workspace/Base(id, v)`, `setSessionDraft(id, v|null)`, `setSessionModel(id, v|null)`, `setSessionEffort(id, v|null)` — plain UPDATEs; `null` clears a pin.
- `setSessionOutcome(id, ok: bool)` — stores 0/1; "whether the delegated TURN errored, not an acceptance gate".
- `addSessionUsage(id, usage: Usage, at: number)` — **cost columns ACCUMULATE** (`COALESCE(col,0) + ?` for input/output/reasoning/cache_read_total/cache_write_total/cost_usd), while the **gauge OVERWRITES**: `context_tokens = inputTokens + cacheRead + cacheWrite`, `cached_tokens = cacheRead + cacheWrite`, `last_llm_at = at`.
- `sessionUsage(id) -> UsageTotals` — session row totals, NULLs → 0; unknown id → all zeros.
- `treeUsage(id) -> UsageTotals` — recursive CTE following `origin_id` **only through kind IN ('subagent','workflow_agent')** (forks/compactions are siblings; charging them double-counts); `UNION` not `UNION ALL` so a cyclic origin_id terminates; SUM with COALESCE 0.
- `busySessionIds() -> Set<string>` — `SELECT DISTINCT session_id FROM turns WHERE status='running'`. Read from turns, **not** pending messages (orphaned turn can leave a message pending after crash).

**Messages**
- `createMessage(m: Message) -> Message` — parts stored as `JSON.stringify(parts)`; read-back.
- `getMessage(id) -> Message | undefined`
- `messagesFor(sessionId) -> Message[]` — `ORDER BY created_at, rowid`.
- `threadFor(sessionId) -> Message[]` — `ancestorChain(id).flatMap(messagesFor)`.
- `updateMessage(id, parts: Part[], pending: bool)` — wholesale overwrite; the turn runner streams into this every round.
- `deleteMessagesFrom(sessionId, messageId) -> string[]` — THE one destructive thread write (unsend backend). Finds anchor `(created_at, rowid)`; selects ids where `(created_at, rowid) >= (?, ?)` (SQLite row-value comparison) ordered the same; deletes in ONE transaction, per id: `turns` rows first (`turns.message_id` is a real FK), then `messages_fts` row, then the message. Returns deleted ids; `[]` if anchor not in that session.

**Turns**
- `createTurn(t: Turn) -> Turn` — inserts id/session/message/status/step/created/updated/error; usage columns NULL.
- `getTurn(id) -> Turn | undefined`
- `turnForMessage(messageId)` — `ORDER BY updated_at DESC, rowid DESC LIMIT 1` (most recently touched wins).
- `turnsForSession(sessionId)` — `ORDER BY created_at, rowid`.
- `turnsByStatus(status)` — same order; boot recovery reads `running` and orphans every row.
- `latestTurnStatuses() -> Map<sessionId, TurnStatus>` — correlated subquery `WHERE rowid = (SELECT rowid ... ORDER BY updated_at DESC, rowid DESC LIMIT 1)`, deliberately NOT bare-column GROUP BY/MAX (arbitrary among same-ms ties, which are the normal case).
- `updateTurn(id, patch: {status?, step?, error?: string|null, usage?: Usage})` — read-current-then-write; missing id → silent no-op. Every call stamps `updated_at = #now()` (the injected clock). `error` merged by **key membership** (`"error" in patch`), so `{error: null}` clears. `usage` **REPLACES** the six usage columns wholesale (runner carries the turn's running total; adding would double-count every round after the first).

**Durable KV (`session_state`, scoped to lineage root)**
- `getState(rootId, key) -> string | undefined`
- `setState(rootId, key, value, now)` — `INSERT ... ON CONFLICT(root_id,key) DO UPDATE SET value, updated_at`.
- `listState(rootId) -> {key, bytes, updatedAt}[]` — `length(value)` only, ordered by key; "a listing must never drag whole values into context".
- `deleteState(rootId, key) -> bool` — true iff a row existed (implemented as get-then-delete).

**Schedules**
- `createSchedule / getSchedule / listSchedules` (`ORDER BY created_at, rowid`) / `deleteSchedule`.
- `dueSchedules(now)` — `enabled = 1 AND next_run_at <= now ORDER BY next_run_at, rowid`.
- `updateSchedule(s: Schedule)` — overwrites title/prompt/workspace/spec/enabled/last_run_at/next_run_at (NOT session_id, NOT created_at); caller merges PATCH into full row.
- `markScheduleRun(id, lastRunAt, nextRunAt)` — caller computes `nextRunAt` FROM NOW (missed slots fire once, no burst; plan §6.8).

**Workflows**
- `createWorkflow(w: WorkflowRun) -> WorkflowRun` — phases JSON-stringified; result/args via `json()` helper (undefined|null → NULL).
- `getWorkflow(id)`
- `listWorkflows(sessionId?)` — no arg: all, `created_at DESC, rowid DESC`. With arg: BFS over the session graph collecting ids — follow `parent_id` always, follow `origin_id` **only when kind is `fork` or `compaction`** (on a delegate, origin_id means the SPAWNER, whose runs are not the delegate's); then `WHERE session_id IN (...)`. This is what makes a fork's inherited workflow cards resolve their run rows.
- `unfinishedWorkflows()` — `status IN ('running','paused') ORDER BY created_at, rowid` (boot orphaning).
- `updateWorkflow(id, patch: Partial<WorkflowRun>)` — key-membership merge (`"x" in patch`), so `{result: undefined}` ≠ `{}` — result/args are `unknown` and `undefined` is a legitimate script return. Identity fields (id, sessionId, script, createdAt, resumeOf) are NOT patchable: "the script text is the record of what actually ran".
- `createWorkflowAgent(a) -> WorkflowAgent` — note: `schema` column is always inserted **NULL**; the wire `WorkflowAgent` has no schema field (the JSON Schema is part of what `key` hashes).
- `updateWorkflowAgent(id, patch)` — key-membership merge over label/phase/status/result/error/sessionId/startedAt/finishedAt. `startedAt` patchable on purpose: a queued agent's clock resets when it leaves the run's semaphore.
- `listWorkflowAgents(runId)` — `ORDER BY idx, rowid`.
- `findWorkflowAgent(runId, key)` — `ORDER BY idx, rowid LIMIT 1` (first call wins).

**Keyword search (messages_fts)**
- `indexMessage(m: Message)` — delete-then-insert by message_id (idempotent under streaming re-index; FTS table has no unique constraint). Indexed text = `indexableText(parts)`: only `text` and `reasoning` parts, joined `"\n"`, trimmed; empty → no row at all. Tool calls/results/image paths deliberately excluded.
- `searchMessages(query, {sessionId?, limit?=20}) -> SearchHit[]` — `messages_fts MATCH ?` joined to messages; `snippet(messages_fts, 0, '', '', '…', 24)`; `ORDER BY rank, m.created_at DESC, messages_fts.message_id` (deterministic across rebuild vs incremental, plan T8.9). An FTS5 syntax error → `BadRequestError` (HTTP 400) whose message contains the quoted query, the driver error, "FTS5", and the hint `Quote a phrase as "like this"; bare ", *, ^, : and NEAR are operators.`
- `rebuildSearchIndex()` — `DELETE FROM messages_fts`, then `indexMessage` per message in `(created_at, rowid)` order. Deliberately not bulk INSERT..SELECT: sharing the one projection fn is what guarantees rebuild == incremental.

**Command-history memory**
- `recordCommand(r: CommandRecord)` — one transaction: INSERT command_history (11 cols, `messageId ?? null`), take `lastInsertRowid`, INSERT one command_tags row per `tagList` entry, one command_dirs row per `dirs` entry, one command_history_fts row (cmd, tags, output_head, command_id). A half-recorded command would skew every popularity join.
- `commandTagRows(repo, {dir?, sinceTs?}) -> {tag, ts, exitCode}[]` — join history×tags scoped by repo; `sinceTs` = `h.ts >= ?`; `dir` = EXISTS over command_dirs with `rel_dir = ? OR rel_dir LIKE ? || '/%'` (descendants, not name prefixes — `src` matches `src/tui` but not `src2`).
- `tagSpread(sinceTs?) -> {repos: number, byTag: Map<tag, distinct-repo-count>}` — NOTE: `sinceTs` is interpolated via `Number(sinceTs)` (not bound), guarded by numeric coercion.
- `tagDiversityByDay(sinceTs, repo?) -> TagDiversityDay[]` — one CTE-based query grouping by `date(ts/1000,'unixepoch','localtime')` (**local time** on purpose); per day: sessions, commands, tagged (tags <> ''), distinctTags (`instr(tag,'.') = 0` — coined words), distinctRefs (`instr(tag,'.') > 0` — `linear.eng-1234`-style keys), tagUses, singletons (coined tags used exactly once that day). Ordered day DESC.
- `commandsForTag(tag, {repo?, limit?=20}) -> TaggedCommand[]` — newest first; limit interpolated via `Math.max(1, Math.trunc(limit))`.
- `repoTagCounts(repo, sinceTs) -> Map<tag, uses>` — coined only (`instr(tag,'.') = 0`); references are keys, never vocabulary.
- `priorFailures(repo, cmd, sinceTs, sessionId) -> PriorFailures | null` — one aggregate row over failing runs (`exit_code IS NOT NULL AND exit_code <> 0`) of this EXACT cmd; uses SQLite's bare-column-with-`max(ts)` rule to pull exit_code/output_head from the latest failure; `sum(session_id = ?)` for the in-session count; null when `n == 0 || last_ts IS NULL`. **Rust hazard: bare-column-with-MAX is a SQLite-specific behavior; port it as a window fn or an ORDER BY ts DESC LIMIT 1 second query — but it must return the same row.**
- `recentFailures(repo, sinceTs, limit) -> {cmd, outputHead, ts, sessionId}[]` — newest first; outputHead NULL→"".
- `lastSuccessLike(repo, prefix, notCmd, sinceTs) -> string | null` — `exit_code = 0 AND cmd LIKE ? ESCAPE '\' AND cmd <> ?` newest first; `prefix` arrives pre-escaped for LIKE (`history/echo.ts` builds it), `\` is the escape char.

### `db/migrate.ts`
- `SCHEMA_VERSION = 1` (const).
- `schemaSql() -> string` — reads `schema.sql` from disk beside the module (Rust: `include_str!` is fine and better; the TS rationale — "the file cannot drift from what is applied" — is preserved either way since Cargo rebuilds).
- `migrate(db) -> number` — returns the `user_version` found BEFORE (0 = first open). Order of operations: (1) read user_version; (2) **throw if found > SCHEMA_VERSION** with a message naming both versions ("this database was written by a newer bough (schema v{found}, this build understands v{SCHEMA_VERSION})… Upgrade bough, or point BOUGH_DB at a different file."); (3) `rebuildDayOneCommandHistory`; (4) `addScheduleSessionId`; (5) `addCommandMessageId`; (6) `exec(schemaSql())`; (7) stamp `PRAGMA user_version = SCHEMA_VERSION` iff found < SCHEMA_VERSION.
- `userVersion(db) -> number` — `PRAGMA user_version`, 0 for untouched file.
- `setUserVersion` (private) — value is interpolated (PRAGMA takes no bind), guarded by `Number.isSafeInteger(v) && v >= 0` else throw.

**The three sanctioned reshapes** (each: check table exists via sqlite_master, check column via `PRAGMA table_info`, else act; run BEFORE the schema exec; a fresh file no-ops through all three because the tables don't exist yet):
1. `rebuildDayOneCommandHistory` — if `command_history` exists WITHOUT `output_head`: `DROP` command_history_fts, command_dirs, command_tags, command_history (data loss accepted: "the memory is an accumulating cache, not a record"). Orphaned embeddings rowids simply never return from a rebuilt history.
2. `addScheduleSessionId` — if `schedules` exists without `session_id`: `ALTER TABLE schedules ADD COLUMN session_id TEXT` (user records; rows kept; NULL = reports to nobody = pre-change behavior).
3. `addCommandMessageId` — if `command_history` exists without `message_id`: `ALTER TABLE command_history ADD COLUMN message_id TEXT`.

**Frozen-schema rule for the port:** new columns require a named, PRAGMA-guarded reshape function with a prose paragraph; ALTER TABLE appends, so **columns go at the END** of the table (a migrated file and a fresh one must agree on column order — this is why `schedules.session_id` and `command_history.message_id` are the last columns in schema.sql). Never `ALTER` inside a swallowed error. "Three of these is not yet a ladder; a fourth without a paragraph like this one would be."

### `db/extensions.ts`
- `enableSqliteExtensions() -> bool` — idempotent, first call decides for the process. `BOUGH_NO_EMBED` set → false. Non-darwin → true (Bun's bundled SQLite loads extensions). Darwin: find Homebrew libsqlite3 at `/opt/homebrew/opt/sqlite/lib/libsqlite3.dylib` or `/usr/local/opt/sqlite/lib/libsqlite3.dylib`; missing → false; found → `Database.setCustomSQLite(lib)`; that call throwing (a Database already opened; the swap window has passed) → false. Server entry calls this **as its first act, before anything opens bough.db**.
- `extensionsEnabled() -> bool` — reports the decision; never triggers the swap.
- **Rust note:** this whole dance exists because Bun links Apple's extension-less system SQLite on macOS. `rusqlite` with the `bundled` + `loadable_extension` features compiles its own SQLite with extension loading available, so the Homebrew-dylib swap disappears; keep only the `BOUGH_NO_EMBED` gate and the once-per-process decision.

### `history/embed.ts` (extension consumer — sqlite-vec + lembed)
- `createEmbedLayer(opts?: {boughDb?, embedDb?, modelPath?}) -> EmbedLayer | null` — null when `!extensionsEnabled()` (the everyday macOS-without-Homebrew answer; callers treat as "feature does not exist").
- `interface EmbedLayer { drain(): Promise<number>; similar(text): Promise<unknown[]>; close(): void }`.
- Defaults: boughDb = `dbPath()`; embedDb = `~/.bough/embeddings.db`; model = `$BOUGH_EMBED_MODEL` else `~/.bough/models/all-MiniLM-L6-v2.e4ce9877.q8_0.gguf` (auto-downloaded from huggingface `asg017/sqlite-lembed-model-examples`, ~25MB, 384 dims; download to `<path>.download` then rename so a killed download never leaves a trusted half-file; a user-supplied model path that's missing throws instead of downloading).
- Open (lazy, memoized-including-failure; failed init retried next drain): open embeddings.db → `loadExtension(vec)` → `loadExtension(lembed)` → `ATTACH DATABASE ? AS src` (bough.db) → register model into `temp.lembed_models` via `lembed_model_from_file` → **probe dims** = `length(lembed('embed','probe'))/4` (never hardcode; env models of any width work) → `embed_meta(key,value)` table; if stored `model` ≠ `basename(model):dims` → `DROP TABLE vec_index` + upsert meta (different model's vectors aren't comparable; store is derived; rebuild from zero) → `CREATE VIRTUAL TABLE IF NOT EXISTS vec_index USING vec0(embedding float[dims])`.
- `drain()` — batch 64: `INSERT INTO vec_index (rowid, embedding) SELECT h.id, lembed('embed', h.tags || ' ' || substr(h.cmd,1,500)) FROM src.command_history h WHERE h.id NOT IN (SELECT rowid FROM vec_index) ORDER BY h.id LIMIT 64`. Returns count-delta, **not `changes`** — an insert into a vec0 virtual table reports shadow-table writes (4 rows came back as 14). Any error (locked writer, torn attach, model unavailable offline) → return 0, lose one tick, never the layer.
- `similar(text)` — KNN 10: subselect `WHERE embedding MATCH lembed('embed', ?) ORDER BY distance LIMIT 10` joined back to `src.command_history` on rowid; returns cmd/tags/repo/exit_code/ts/round(distance,4). Failure is a catchable, explanatory rejection (only reachable when the layer exists; the model deserves to know why recall failed).
- Embedding is CPU-bound and synchronous inside SQLite — batch kept small so it never blocks the event loop long. In Rust: run drains on `spawn_blocking`.

### `schema/parts.ts` — wire types (serde structs + validation in Rust)
- `Role` = `user | supervisor | system` (`system` = harness-injected notes; replays to the model as user-side text; not a provider role).
- `SessionKind` = `root | fork | compaction | subagent | workflow_agent | schedule_run | shell`.
- `COLLAPSED_KINDS = [subagent, workflow_agent, schedule_run]`, `isCollapsedKind()`; `DELEGATED_KINDS = [subagent, workflow_agent]`, `isDelegatedKind()`. Canonical HERE (three modules had drifted into three literals). A schedule_run collapses but is NOT delegation (own countdown row; posts its own system note).
- Part union, discriminated on `type` (7 kinds):
  - `text {type,text}`
  - `reasoning {type,text, meta?: unknown, model?: string}` — `meta` is the provider's signed thinking block **verbatim, never rendered**; must be passed back EXACTLY as received on the same model; `model` gates replay (signatures are model-scoped). Both absent on old rows.
  - `tool_call {type,id,name,input: unknown}`
  - `tool_result {type,callId,output: unknown,isError: bool, interrupted?: bool}` — interrupted ≠ error, rendered distinctly.
  - `image {type,path,mediaType,name,size}` — bytes at path under `~/.bough/attachments/`, never inline; lost file replays as placeholder text.
  - `ask {type,id,question,options?: string[],status: answered|declined|interrupted, answer?}` — appended only once SETTLED, never pending, so replay can't re-block.
  - `workflow {type,id,name,description,rerunOf?: string|null}` — no status field on purpose (a frozen status would lie within seconds; the card reads the run row live by id).
- `Message {id, sessionId, role, parts: Part[], pending: bool, createdAt}`.
- `Session` — wire shape (note: **no `workspace`/`base` secrecy — they ARE nullish wire fields here, but `SessionRuntime` exists for callers that only need those two**): id, title, kind, createdAt, parentId (nullable), originId/originMessageId/workspace/originDir/base/model/effort/draft/contextTokens/cachedTokens/lastLlmAt/outcomeOk all `.nullish()`. No archivedAt/deprecatedAt — zod strips unknown keys, and the freeze test asserts they don't survive parsing.
- `TurnStatus` = `running | done | error | interrupted | orphaned`.
- `Usage {inputTokens, outputTokens, reasoningTokens?, cacheReadTokens?, cacheWriteTokens?, costUsd?}` (nullish optionals).
- `Turn {id, sessionId, messageId, status, step, createdAt, updatedAt, error?, usage?: Usage|null}`.
- `AskQuestion` — memory-only server-side ("the hold dies with the turn"); durable record is the settled AskPart.
- `BackgroundJob {id, name (default "" for old-server rows), sessionId, pid, command, status: running|exited, exitCode?, signal?, startedAt, exitedAt?}` — NOT persisted; `signal` exists because exitCode is null for a signalled process and `(exitCode ?? 0) === 0` misread a user-killed shell as `✓ done`.
- `Schedule {id, title, prompt, workspace: nullable, sessionId: nullable default null, spec, enabled, createdAt, lastRunAt: nullable, nextRunAt}` — spec grammar `every:<N><m|h|d>` or `daily@HH:MM`, stored verbatim, parsed by the schedules module.
- `WorkflowStatus` = `running|paused|done|error|stopped|orphaned`; `WorkflowPhase {title, detail?}`; `WorkflowRun` (result/args `unknown`); `WorkflowAgentStatus` = `queued|running|done|error|stopped|cached`; `WorkflowAgent` (see §2 db methods; no `schema` field).

### `schema/events.ts`
- `EVENT_TYPES` — closed 16-name list: session.created, session.updated, session.activity, message.started, message.delta, message.part, message.finished, message.retry, tool.log, turn.finished, ask.question, job.spawned, job.exited, workflow.updated, workflow.agent, workflow.log. (`tool.log` deliberately included though not in spec §3 — the list is frozen.)
- `BoughEvent {type, sessionId?, seq, ts, data: unknown}` — envelope IS parsed (comes off a socket); payloads are typed but NOT wire-validated (in-process bus hands the emitter's object through). `EventDataMap` maps each name to its payload type; typed views `BoughEventOf<T>`, `AnyBoughEvent`, `EventInput` (event minus bus-stamped seq/ts).
- Payload structs: `MessageDeltaData{messageId,delta}`, `MessagePartData{messageId,part}`, `MessageFinishedData{messageId}`, `MessageRetryData{messageId,attempt(1-based),reason}`, `ToolLogData{messageId,callId,line}`, `SessionActivityData{sessionId,activity: string|null}`, `TurnFinishedData{turnId,sessionId,status,error?}`, `WorkflowLogData{runId,line}`; job.* carry BackgroundJob; session.* carry Session; ask.question carries AskQuestion; workflow.updated/agent carry WorkflowRun/WorkflowAgent.

### `schema/requests.ts` — one struct per body, `<Verb><Noun>Body`
`PartPick {messageId, parts?: int[] nonneg, min 1 when present}`; `CreateSessionBody` (all optional — `{}` is legal); `PostMessageBody {text, images?: [{path,mediaType,name,size}]}`; `SetDraftBody {draft: string|null}`; `PatchSessionBody {model?: string(min1)|null, effort?: enum(low|medium|high|xhigh|max)|null}` — **absent = leave alone, explicit null = clear**; `PutModelSettingsBody` (same shape, global scope); `AnswerQuestionBody {answer?, decline?}`; `ForkBody {atMessageId, atPart?, editedText?, exclusive?, summarizeAbandoned?}`; `UnsendBody {atMessageId}` (single id on purpose — no "and everything after" flag); `CompactBody {picks: PartPick[] min1, instructions?}`; `SectionsBody {turns: [{gist: max500}] min1 max500}`; `ExtractBody {picks min1}`; `MoveBody {sourceId, picks min1}`; `HandoffBody {goal min1}`; `RunShellBody {command min1 max4000}`; `RevertChangesBody {paths?: string[]}`; `CreateScheduleBody {title min1, prompt min1, workspace? min1, spec, enabled?}`; `PatchScheduleBody` (all optional; `workspace: null` clears); `CreateWorkflowBody {sessionId min1, script min1, args?: unknown}`; `RerunWorkflowBody {script? min1, args?}`; `PostCommentBody {artifact min1, text min1, anchor?: unknown}`; `SendCommentsBody {ids?}`; `PatchConfigBody {model?, effort?, sessionId?}`; `PutKeysBody = record<string,string>`; `PutThemeBody {name trim min1 max80, colors: record}`; `PutMcpServerBody = union(local {command min1, args?, env?} | remote {url: url, headers?})`; `McpActivationBody {sessionId, ttl?}`; `SearchQuery {q min1, sessionId?, limit?: int 1..200}`.

---

## 3. Data structures — table catalog

All timestamps epoch-ms INTEGER; booleans 0/1 INTEGER; structured data JSON TEXT. FKs declared; `PRAGMA foreign_keys = ON` per connection (off by default in SQLite — must be set at EVERY open).

**sessions** — one conversation; forest by parent_id.
| column | meaning |
|---|---|
| id TEXT PK | |
| parent_id TEXT REFERENCES sessions(id) | thread inheritance; NULL for root AND for subagent (fresh task-only thread, spec §7) |
| title TEXT NOT NULL | |
| kind TEXT NOT NULL | root/fork/compaction/subagent/workflow_agent/schedule_run/shell — IS the visibility rule |
| created_at INTEGER NOT NULL | |
| workspace TEXT | checkout operated on, edited in place; NULL = process default; subagents share it |
| origin_dir TEXT | project dir at creation, never rewritten — stable record of WHICH project |
| base TEXT | git sha started from; `git diff <base>` + untracked = change set; NULL = non-git = no revert |
| origin_id TEXT, origin_message_id TEXT | lineage edge: what this branched from, at which message. On a delegate = the SPAWNER |
| model TEXT, effort TEXT | per-session pins; NULL = global default |
| draft TEXT | prefilled composer text (handoff); cleared by first post |
| context_tokens, cached_tokens, last_llm_at INTEGER | GAUGE — last round only; client derives cache warmth from last_llm_at + TTL |
| input_tokens, output_tokens, reasoning_tokens, cache_read_total, cache_write_total INTEGER; cost_usd REAL | CUMULATIVE across session |
| outcome_ok INTEGER | 0/1/NULL; whether the delegated TURN errored; no acceptance gate |

Indexes: `sessions_parent(parent_id)`, `sessions_origin(origin_id)`.

**messages** — id TEXT PK; session_id TEXT NOT NULL FK; role TEXT NOT NULL (user/supervisor/system); parts TEXT NOT NULL (JSON Part[]; image bytes NEVER in it); pending INTEGER NOT NULL (streaming flag); created_at INTEGER NOT NULL. Index `messages_session(session_id, created_at)`; reads always add rowid tie-break.

**turns** — id TEXT PK; session_id FK; message_id TEXT NOT NULL FK→messages (the pending supervisor message it produces); status TEXT (running/done/error/interrupted/orphaned); step TEXT NOT NULL (human-readable last checkpoint); created_at; updated_at; error TEXT; input_tokens/output_tokens/reasoning_tokens/cache_read_tokens/cache_write_tokens INTEGER; cost_usd REAL. Indexes `turns_status(status)`, `turns_session(session_id, updated_at)`. Boot: every `running` → `orphaned`, session unblocks.

**workflows** — id TEXT PK; session_id FK; name, description, script TEXT NOT NULL (script verbatim, mirrored to `~/.bough/workflows/<id>.js`); phases TEXT NOT NULL (JSON [{title,detail?}]); status TEXT; current_phase TEXT; result TEXT (JSON); error TEXT; args TEXT (JSON); resume_of TEXT FK→workflows; created_at; finished_at. Index `workflows_session(session_id, created_at)`.

**workflow_agents** — id TEXT PK; run_id FK→workflows; idx INTEGER (call order); key TEXT (hash(prompt+opts) — the replay-journal key; why scripts are forbidden Date.now/Math.random); label; phase; prompt; model; schema TEXT (JSON Schema of a {schema} call; currently always written NULL by the code); status (queued/running/done/error/stopped/cached); result TEXT; error; session_id TEXT FK→sessions (backing subagent; NULL for cached replays — **real FK: create the backing session before patching sessionId in**); started_at NOT NULL; finished_at. Indexes `(run_id, idx)` and `(run_id, key)`.

**session_state** — root_id TEXT, key TEXT, value TEXT NOT NULL (JSON), updated_at NOT NULL; PK (root_id, key). Scoped to LINEAGE ROOT. Advisory 16KB/key ("notes, not storage" — limit enforced by caller, not schema).

**schedules** — id TEXT PK; title, prompt NOT NULL; workspace TEXT (NULL = chat-only); spec TEXT NOT NULL (verbatim); enabled INTEGER NOT NULL; created_at; last_run_at; next_run_at NOT NULL; **session_id TEXT LAST** (creator conversation for report-back; deliberately NO FK — the creator may be deleted, the note then just drops; LAST because ALTER appends). Index `schedules_due(enabled, next_run_at)`.

**command_history** — id **INTEGER PRIMARY KEY** (rowid alias; high-volume append-only log; join key for junctions AND the vec_index rowid); session_id TEXT NOT NULL FK; ts; repo TEXT NOT NULL (git origin URL else workspace root — scope key surviving re-clones); cmd; tags TEXT NOT NULL (normalized colon-separated, '' for none); exit_code INTEGER (NULL = still running when the turn moved on); duration_ms; output_head TEXT NOT NULL DEFAULT '' (first ~2k chars as the program saw it, spill marker included); spill_path TEXT (pointer, not a guarantee); source TEXT NOT NULL DEFAULT 'live' (live|backfill); **message_id TEXT LAST** (supervisor message whose run_steps program ran it; deliberately NO FK — the memory outlives its transcript). Index `(repo, ts)`.

**command_tags** — command_id INTEGER FK, tag TEXT. Indexes `(tag, command_id)`, `(command_id)`.
**command_dirs** — command_id INTEGER FK, rel_dir TEXT (workspace-relative dirs the command was ABOUT, from path-looking tokens — not cwd). Indexes `(rel_dir, command_id)`, `(command_id)`.

**command_history_fts** — FTS5(cmd, tags, output_head, command_id UNINDEXED) `tokenize = 'unicode61 remove_diacritics 2'`.
**messages_fts** — FTS5(text, message_id UNINDEXED, session_id UNINDEXED), same tokenizer. Standalone (NOT external-content): indexed text is a projection of the parts JSON, not a mirrorable column.

**In embeddings.db only** (separate file): `embed_meta(key TEXT PK, value TEXT)`; `vec_index` = `vec0(embedding float[dims])` with rowid = command_history.id; `temp.lembed_models` (per-connection).

Wire shapes: camelCase everywhere on the wire; snake_case in storage; the row→domain mappers in db.ts are the ONLY translation. Absent optionals come back as `null`, never `undefined` — one shape per row. (Rust: `Option<T>` + serde covers both; the null/undefined distinction only matters at the *patch* boundaries, see §4.)

---

## 4. Behaviors & edge cases (mined from db.test.ts, parts.test.ts, and code)

Ordering / threading:
1. Three messages in one millisecond come back in insertion order (`messagesFor` tie-break); dropping the rowid tie-break "silently reorders history and nothing else in the system notices".
2. `created_at` still dominates rowid when timestamps differ (a later-inserted, earlier-stamped message sorts earlier).
3. `threadFor` groups by SESSION root-first, ordering only WITHIN a session — interleaved timestamps across levels must NOT be globally sorted (the test uses mid-session timestamps below root ones to catch this).
4. A subagent's thread is its own messages only (parentId null, originId = spawner); `sessionsByOrigin(spawner)` finds it.
5. `ancestorChain("nope") == []`; chain is inclusive; a parent_id cycle terminates via the seen-set instead of hanging.

Migration:
6. Idempotent across three opens of the same file: schema introspection (`sqlite_master` type/name/sql + user_version) byte-identical, zero rows touched.
7. `migrate` returns the version found (0 fresh), stamps SCHEMA_VERSION; second run finds and returns the stamp.
8. Newer-version file: throw; message MUST name both versions, not just "failed".
9. Dropped columns stay dropped: sessions has no archived_at/deprecated_at, turns no first_output_at, no message_embeddings table (test pins this).
10. Pre-session_id schedules table: ALTERed in place, rows KEPT, old rows read sessionId null, new shape round-trips.
11. Pre-output_head command_history: whole 4-table group rebuilt EMPTY at open, once; new shape then accepts full records.

Sessions / usage:
12. `createSession` returns the row as stored (deepEqual with getSession); outcomeOk starts null.
13. `listSessions` newest-first and hides nothing (subagents included).
14. Setter round-trips; `setSessionModel(id,null)` / `setSessionDraft(id,null)` clear to null.
15. `addSessionUsage`: totals accumulate across calls; gauge = last round: after rounds (100in/900read) then (50in/2000read/100write): contextTokens=2150, cachedTokens=2100, lastLlmAt=the second `at`.
16. `treeUsage`: root+sub+nested-sub+workflow_agent counted; a `fork` under the same origin excluded. `treeUsage("sub")` counts sub+nested.
17. `busySessionIds` from turns not messages: an orphaned turn with its message still pending must not read busy.

Turns:
18. A turn with no reported round has `usage: null` (reported = input_tokens OR output_tokens non-null); zeros would be "a claim we cannot make". Once reported, missing subfields read as `inputTokens/outputTokens ?? 0`, others stay null.
19. `updateTurn` stamps updated_at from the injected clock on EVERY call; unpatched fields preserved; usage REPLACES (25 overwrites 10, never 35); `{error: null}` clears error; unknown id no-ops.
20. `latestTurnStatuses` with two same-millisecond checkpoints picks the later rowid, deterministically.

KV / schedules:
21. `setState` upserts; `listState` ordered by key with byte lengths; `deleteState` true-then-false; scoping strictly per root_id.
22. `dueSchedules(100)`: enabled + next_run_at<=100, soonest first; disabled past-due rows excluded; `markScheduleRun` removes from due set.

Workflows:
23. Patch-by-membership: `{currentPhase:"Review"}` leaves args intact; `result: [1,2,3]` round-trips as JSON; `script` is never patched.
24. `listWorkflows` lineage walk: fork lists own + ancestors' runs (2 levels deep proven); a copy-seeded fork (parentId NULL, originId set) reaches origin's runs; a subagent does NOT list its spawner's runs; unrelated sessions never leak in.
25. Journal: `listWorkflowAgents` ordered by idx regardless of insert order; `findWorkflowAgent` exact (runId,key), undefined otherwise; the backing session must exist before `updateWorkflowAgent` sets sessionId (FK).

Search:
26. Only text+reasoning parts index; tool_call input text ("patch patch patch") and tool_result output do NOT match; re-indexing never duplicates; snippet contains the term; createdAt joins through; per-session scope and limit work; rebuild output deepEquals incremental output.
27. Malformed FTS query (`"unterminated`) throws an error with `.status === 400`, message contains "FTS5" and "Quote a phrase".

Integrity / files:
28. FK violations throw (message into nonexistent session → FOREIGN KEY error) — proves foreign_keys pragma is on for every connection.
29. `openDb` creates nested parent directories.

Command history:
30. `recordCommand` round-trips tags per-repo; `commandTagRows` dir scope: `dir: "src"` matches rel_dir `src` and `src/tui` but NOT `src2` (descendant semantics via `= ? OR LIKE ?||'/%'`, not name-prefix); sinceTs floors lookback.
31. `programForMessage(messageId) -> string|null` — reads the FIRST part with `type=="tool_call"` and string `input.code` from the message's parts JSON ("one program per round is the whole design, so first is the one"); null for missing row, no such part, or **unparseable parts JSON (corrupt row is not a crash for a reader — swallow the parse error)**.

Wire-contract freeze (parts.test.ts):
32. Part union closed: `prose` and `worker` rejected. Role `worker` rejected. Session kind `worker` rejected.
33. Session.parse STRIPS unknown keys (archivedAt/deprecatedAt must not survive). Rust: `#[serde(deny_unknown_fields)]` is WRONG here — the contract is strip-and-accept, not reject.
34. EVENT_TYPES has exactly 16 names; unknown event type rejected on the envelope.
35. `PartPick.parse({messageId, parts: []})` rejects (min 1 when present); absent parts fine. `CompactBody` rejects empty picks. `CreateSessionBody.parse({}) == {}`.

Naive-port hazards not covered above:
36. `#get` normalizes driver null → undefined at exactly one place; the rest of the codebase treats absence as undefined. In Rust, `Option` handles this — but the **patch semantics** need care: TS distinguishes "key absent" (leave alone) from "key = undefined" (for result/args: a legitimate value) from "key = null" (clear). Rust patch structs need a tri-state, e.g. `Option<Option<T>>` or a `Patch<T> { Keep, Set(T) }` enum, on: updateTurn.error, updateWorkflow.{everything}, updateWorkflowAgent.{everything}, PatchSessionBody, PatchScheduleBody.
37. Interpolated (not bound) SQL fragments: `tagSpread` sinceTs (`Number()` guard), `commandsForTag`/`recentFailures` LIMIT (`Math.max(1, trunc)`), `setUserVersion` (integer guard). In Rust bind them all properly — but keep the `max(1,…)` clamp behavior.
38. `deleteMessagesFrom` uses SQLite row-value comparison `(created_at, rowid) >= (?, ?)` — supported by rusqlite/SQLite ≥ 3.15, keep it.
39. `updateSchedule` does NOT write session_id or created_at; `markScheduleRun` writes only last/next. `createSchedule` DOES write sessionId.
40. `recordCommand` must be one transaction; `deleteMessagesFrom` must be one transaction; everything else is single-statement.
41. `json()` helper: undefined AND null both store NULL so reads round-trip; `nul()` maps undefined→null for binding.

---

## 5. Dependencies

Imports (db side): `bun:sqlite` (Database), `node:fs` (mkdirSync/readFileSync/existsSync), `node:path`, `../paths.ts` (dbPath/boughPath), `../errors.ts` (`BadRequestError` — an Error with `.status = 400`), `../types.ts` (the Db port + row structs), `../schema/parts.ts` (domain types). `history/embed.ts` additionally imports npm `sqlite-vec` and `sqlite-lembed` (each exposing `getLoadablePath()` to a prebuilt native extension), `db/extensions.ts`, and uses `fetch` + `Bun.write` for the model download.

Imported by: `server/main.ts` (constructs the one live SqliteDb; calls `enableSqliteExtensions()` first), `cli/tags.ts` (read paths + embed layer for `bough tags similar/sql`; opens read-only handles), `history/*` (record/echo/hygiene/priming via the Db port), and ~53 modules import `schema/parts|events|requests` (router, TUI store, turn runner, workflows, schedules, CLI). The `Db` **port** lives in `types.ts` — consumers depend on the port, not on SqliteDb; tests inject `:memory:` and a fake clock.

---

## 6. External deps → Rust equivalents

| TS/Bun | Rust |
|---|---|
| `bun:sqlite` (sync API, prepared stmts, `lastInsertRowid`, `transaction()`, `loadExtension`, `ATTACH`) | `rusqlite` (features: `bundled`, `functions` if needed, `load_extension`, `backup` unneeded). Sync like bun:sqlite. Row-value comparisons + FTS5 need the bundled build (FTS5 is on by default in `bundled`). |
| `Database.setCustomSQLite` (Homebrew dylib swap on macOS) | Not needed: `bundled` compiles extension-capable SQLite everywhere. Keep `BOUGH_NO_EMBED` env gate + once-per-process `OnceLock<bool>`. |
| zod schemas | `serde` derive + `serde_json`; enums as Rust enums with `#[serde(rename_all)]`; validation constraints (min/max/url) via `validator` crate or hand-rolled checks in `TryFrom` at the router edge; discriminated union → `#[serde(tag = "type", rename_all = "snake_case")]` enum for Part |
| `JSON.parse/stringify` for parts/phases/result/args | `serde_json::to_string` / `from_str`; `unknown` → `serde_json::Value` |
| `node:fs` mkdirSync/readFileSync | `std::fs::create_dir_all`; `include_str!("schema.sql")` for the schema text |
| npm `sqlite-vec`, `sqlite-lembed` (prebuilt loadable dylibs) | `sqlite-vec` crate ships a static-linkable extension (`sqlite_vec::sqlite3_vec_init` + `unsafe` auto_extension) — prefer static registration over dylib loading. `sqlite-lembed` has NO Rust crate: either load the prebuilt dylib via `Connection::load_extension` (path from config/env) or defer (see §8). |
| `fetch` + `Bun.write` (model download) | `reqwest` (blocking or tokio) + `std::fs::write` to `.download` temp + `rename` |
| `Date.now` injected clock | a `Clock` trait or `Box<dyn Fn() -> i64 + Send + Sync>` in the Db constructor |
| `BadRequestError` | a `DbError` enum with a `BadRequest(String)` variant the HTTP layer maps to 400 |
| `Map`/`Set` returns | `HashMap`/`HashSet` (order of Map iteration is never relied on; ordered results come as Vec) |

---

## 7. Suggested Rust layout

```
crates/
  bough-types/            # the schema/ crates — zero I/O, pure serde
    src/parts.rs          # Role, SessionKind, COLLAPSED/DELEGATED_KINDS, Part enum,
                          # Message, Session, Turn, TurnStatus, Usage, AskQuestion,
                          # BackgroundJob, Schedule, Workflow*, per-type unit tests
                          # from parts.test.ts (closed unions, stripped unknowns)
    src/events.rs         # EventType enum (16), BoughEvent envelope, payload structs,
                          # EventData enum keyed by type
    src/requests.rs       # request-body structs + TryFrom validation (the 400 layer)
    src/db_port.rs        # trait Db (the port), CommandRecord, UsageTotals, SearchHit,
                          # PriorFailures, TagDiversityDay, TaggedCommand, CommandTagRow,
                          # SessionRuntime, Patch<T> tri-state enum
  bough-db/
    src/lib.rs            # pub use; open_db()
    src/schema.sql        # copied verbatim; include_str!
    src/migrate.rs        # SCHEMA_VERSION, migrate(), user_version(), the 3 reshapes
    src/sqlite_db.rs      # struct SqliteDb { conn: rusqlite::Connection, now: Clock }
                          # impl Db for SqliteDb — mappers row->domain private here
    src/extensions.rs     # OnceLock<bool> embeddings-capability decision
    src/embed.rs          # EmbedLayer over a second Connection to embeddings.db
```

- **Trait**: `Db` (from types.ts:185) is the one trait; SqliteDb the one impl; tests + higher layers take `&dyn Db` or generic `D: Db`. Methods stay synchronous.
- **Async boundary**: rusqlite Connections are `!Sync`. The server (tokio) should own the SqliteDb inside a dedicated actor task or `tokio::task::spawn_blocking` wrapper — recommended: a thin `DbHandle` (mpsc to a blocking thread) OR simply `Mutex<SqliteDb>` + `spawn_blocking` per call; bough is a single-user local server, contention is negligible, `Mutex` is fine for v1. `EmbedLayer::drain/similar` run on `spawn_blocking` (CPU-bound lembed inside SQLite).
- **Clock**: `pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>`; only `update_turn` reads it.
- **Patch structs**: explicit `TurnPatch { status: Option<TurnStatus>, step: Option<String>, error: Patch<Option<String>>, usage: Option<Usage> }`, `WorkflowPatch`, `WorkflowAgentPatch` with `Patch<T> { Keep, Set(T) }` — this reifies TS's key-membership semantics instead of guessing from Option.
- Keep the mappers' rule: one row struct per table (private), one translation point snake→camel via serde rename on the wire types; internal Rust code just uses the domain structs.
- Port the db.test.ts suite nearly 1:1 (`:memory:`, tempdir for reopen/reshape tests, fake clock); the three "load-bearing" tests (same-ms ordering, three-level threadFor, migration idempotence across opens) are the acceptance gate.

---

## 8. v1 scope cut

**Core (must exist for a working agent loop + TUI):**
- `bough-types` in full — everything imports it; cutting fields breaks the wire.
- open/migrate (including the newer-version refusal and user_version stamping), foreign_keys pragma, sessions, messages, turns, session_state, `deleteMessagesFrom`, `busySessionIds`, `latestTurnStatuses`, usage accumulation + treeUsage.
- messages_fts indexing + search (cheap — the FTS table is created by the schema anyway, and `^s` transcript search is a daily-driver surface).

**Can ship v1 but tolerable to defer days:**
- schedules (table must exist; the ticker consuming dueSchedules can land with the schedules feature).
- workflows + workflow_agents (table + CRUD needed only when the workflow engine ports; keep `listWorkflows` lineage-walk semantics when it lands — it is the subtle one).
- command-history memory: `recordCommand` + `commandTagRows` + `priorFailures`/`recentFailures`/`lastSuccessLike` come with the history subsystem, not before. Tables cost nothing (schema creates them regardless).

**Stub / drop in v1:**
- The three migrate reshapes: **keep `addScheduleSessionId` + `addCommandMessageId`** (trivial, and the user's live bough.db on two installs depends on them at first open by the Rust migrate); `rebuildDayOneCommandHistory` can be kept too (10 lines) — dropping it risks a wedge on any file older than 2026-08, so just port all three.
- `history/embed.rs` (sqlite-vec + lembed vector layer): **stub to `None`** — the layer's whole contract is graceful absence (`createEmbedLayer` returning null is the everyday answer already); tags + FTS recall are unaffected. Port later; when ported, prefer statically registered sqlite-vec and dylib-loaded lembed, keep separate embeddings.db + ATTACH + count-delta drain + model-id rebuild.
- `extensions.rs` shrinks to the `BOUGH_NO_EMBED` check until embed lands.
- `tagSpread` / `tagDiversityByDay` / `commandsForTag` / `repoTagCounts` / `programForMessage` — CLI (`bough tags`) and hygiene-layer consumers only; stub with `todo!()`-free empty returns or omit from the trait until the history/CLI wave.
- `rebuildSearchIndex` — only the reindex CLI calls it; trivial, but deferrable.
- schema/requests.rs bodies for routes not yet ported (workflows, MCP, theme, comments) can land with their routes; parse-at-edge discipline says each router wave brings its bodies.

**Never cut:** `PRAGMA foreign_keys = ON` at every open; `(created_at, rowid)` tie-breaks; the read-back-after-insert contract; the usage accumulate-vs-overwrite split; the frozen schema file with reshape-function discipline (columns append at END).
