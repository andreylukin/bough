# Port spec: `root` subsystem

Modules: `src/bus.ts`, `src/errors.ts`, `src/paths.ts`, `src/schedules.ts`, `src/scratch.ts`, `src/types.ts` (+ closely coupled: `src/hostfn/schedule.ts` grammar, `src/schema/events.ts` envelope — both quoted here because the root modules' contracts are unreadable without them).

---

## 1. Purpose & invariants

Process-wide wiring: the in-process event bus feeding SSE, the error taxonomy the router maps to HTTP, the `~/.bough` data-root layout, the schedule ticker (cron-like wakeups + `[schedule fired]` report-back), per-session scratch dirs, and the shared injection-seam types (`AppCtx`, `TurnCtx`, `Db`/`Bus` ports, LLM boundary).

Invariant comments, verbatim:

- **bus.ts**: "one bad subscriber cannot silence the others. A listener that throws is caught, reported, and stepped over; fan-out continues down the set." And: "the bus is display transport, never storage. It holds no history and replays nothing. `seq` is a process-monotonic counter that resets on restart, so it is a dedupe key and not a resume cursor — a reconnecting client re-fetches the session and reconciles by message id. Persist first, then publish." Also: "There is no module-level singleton. The bus travels in `AppCtx`."
- **errors.ts**: "Every domain error subclasses `HttpError` and carries the status it should become, so the router has exactly ONE try/catch that turns a thrown error into a response and no handler contains a per-error catch block." … "a domain module never constructs a `Response`." … "**Error text is a product surface**" — two audiences: the user (HTTP) and the MODEL (the message becomes the exception a program catches). "each message names *what failed*, *the state that caused it*, and *the move that resolves it* … A message that says only 'failed' is a defect."
- **paths.ts**: "**no module builds a `~/.bough` path by string concatenation.** Every subpath has a named accessor here." … "`BOUGH_HOME` overrides the root … it is what lets the rewrite run beside the live install without touching its database, its artifacts, or its schedules … setting the env var relocates the whole tree."
- **schedules.ts**: "**a schedule that missed N slots fires ONCE.**" Three load-bearing details: (1) `dueSchedules(now)` returns each enabled row **once**, no inner catch-up loop walking `next_run_at` forward slot by slot; (2) "The advance happens **before** the fire, not after" — a throwing fire must not leave the row due, or the next 30s tick refires forever; (3) "`now` is threaded in, never read inside" — one instant per tick for both the due test and the advance.
- **hostfn/schedule.ts** (the arithmetic half): "**`next_run_at` is always computed FROM NOW, never from the stale stored value** … a laptop closed overnight with an `every:30m` schedule wakes up 16 slots behind … Advancing from `now` means one run, then the cadence resumes."
- **scratch.ts**: scratch lives UNDER `~/.bough`, NOT `/tmp` ("macOS empties `/tmp` on reboot and systemd-tmpfiles reaps entries older than ten days"), PER SESSION (two conversations both writing `/tmp/build.log` clobber each other), and "SO IT MUST BE SWEPT BY US … Age is the honest criterion … Swept at boot, best-effort, never on the path of a turn."
- **types.ts**: "**the database, the clock and the LLM are parameters, not imports.**" Second invariant: "`hostfn/` must never import from `server/`. Host functions take a `TurnCtx` and nothing else." `Db` and `Bus` here are PORTS; `db/db.ts` and `bus.ts` export concrete implementations that may expose a wider surface.
- **schema/events.ts**: "**events are display transport, never the source of truth.** … any state a client cannot rebuild from a fresh fetch is a bug in the event design, not something to fix with replay." Event-name list is CLOSED (frozen) so stores can switch exhaustively.

---

## 2. Public API

### bus.ts
- `type Listener = (event: BoughEvent) => void` — called synchronously, in subscription order, for every event.
- `interface BusOptions { now?: () => number; onListenerError?: (error, event) => void }` — injected seams; defaults `Date.now` and a `console.error("bus listener threw:", err)` logger.
- `class Bus implements BusPort`
  - `publish<E extends EventInput>(e): E & {seq, ts}` — stamps `seq` (starts at 1, `++` per publish) and `ts`, delivers synchronously to every subscriber, returns the stamped event. Never throws for a listener's reason. Input is NOT mutated; the stamped event is a fresh object (`{...e, seq, ts}`). Listeners receive the exact same object publish returns (identity, not a copy — Rust can relax to a clone/Arc; nothing depends on mutating it).
  - `subscribe(fn): () => void` — returns an unsubscribe thunk; idempotent, safe to call from inside a listener.
  - `get size(): number` — live subscriber count; SSE leak check asserts it returns to 0.

### errors.ts — the whole hierarchy
- `class HttpError extends Error { constructor(status: number, message: string) }` — sets `this.name = new.target.name` (subclass name appears in logs and the JSON error body).
- Concrete subclasses (status fixed in ctor unless noted):
  - `BadRequestError` 400; `NotFoundError` 404; `ConflictError` 409 (e.g. adopting a finished subagent — posting into a busy session queues, never 409s).
  - `PathError` 400 — path escaped `confine` root. "Not a security boundary … but the *server's* own path handling must not be steerable by a name in a URL."
  - `TurnError` (status passed by caller); `ContextOverflowError` 413 (message NAMES the limit; compaction is user-initiated, never silent); `ProgramError` 400 (must distinguish timeout from interrupt and say what partial work survived); `PatchError` 400 (carries file + line range + "someone else changed those lines"); `AskDeclinedError` 400 (catchable "user declined").
  - `AgentError` (caller status); `SpawnCapError extends AgentError` 429 (message says WHICH cap: per-turn 8 or concurrent tree-wide 4).
  - `WorkflowError` (caller status); `WorkflowScriptError` 400 (raised at SUBMIT time, not mid-run).
  - `BranchError` and subclasses `ForkError`, `CompactError`, `SectionsError`, `ExtractError`, `MoveError`, `HandoffError` (all caller-status).
  - `ChangesError`, `ScheduleError`, `StateError` (16KB/key limit), `ArtifactError`, `McpError` (401 surfaces as "not authorized — open the mcp panel (^p) and press a", NEVER a hang), `LspError` (backend failed ≠ symbol missing; empty result is an answer), `NetError` (non-2xx response is DATA, not an exception), `SkillError` — all caller-status.
  - `LlmError extends HttpError { constructor(message, status = 502, retryAfterMs?: number) }` — `status` drives retry classification; no status = transport fault, always retryable.

### paths.ts
- `boughHome(): string` — `$BOUGH_HOME` if set and non-blank (whitespace-only counts as unset), else `join(homedir(), ".bough")`. `homedir()` falls back to passwd when `$HOME` unset, never throws.
- `boughPath(...segs): string` — join under the root.
- Layout accessors (all resolve through `boughHome()` **at call time** — do not cache the root; tests flip the env var per call):
  - `dbPath()` → `$BOUGH_DB` **outright** if set (`:memory:` legal), else `<home>/bough.db`
  - `artifactsDir()` → `artifacts`; `artifactsDirFor(sessionId)` → `artifacts/<sessionId>` (filesystem is source of truth; survives DB reset)
  - `commentsDir()` → `comments` (deliberately OUTSIDE `artifacts/`); `commentsPathFor(sessionId)` → `comments/<sessionId>.json`
  - `attachmentsDir()` → `attachments` (image bytes for `image` parts)
  - `scratchRoot()` → `scratch`; `scratchDirFor(sessionId)` → `scratch/<sessionId>`
  - `workflowsDir()` → `workflows`; `workflowScriptPath(runId)` → `workflows/<runId>.js`
  - `mapsDir()` → `maps`; `mapDirFor(effort)` → **`confine(mapsDir(), effort)`** — the ONE accessor that confines, because `effort` is model-authored (`../theme.json` must throw `PathError`)
  - `userSkillsDir()` → `skills`; `themePath()` → `theme.json`; `modelSettingsPath()` → `model.json`; `envPath()` → `env`; `mcpRegistryPath()` → `mcp.json`; `mcpAuthPath()` → `mcp-auth.json`; `logsDir()` → `logs`
- `confine(root, candidate): string` — resolve `candidate` against `root`; return absolute normalized path that is `root` or strictly beneath it, else throw `PathError`. Contract details in §4.

### schedules.ts
- `TICK_MS = 30_000`
- `interface FireDeps extends ScheduleDeps { reportError?: (error, schedule) => void }`
- `interface FiredSchedule { session: Session; message: Message }`
- `fireSchedule(ctx: AppCtx, schedule, deps = {}): FiredSchedule | null` — fresh session + prompt message + turn. **Never throws** (timer callback context; a throw = unhandled rejection = server down). Returns `null` on failure after calling `report(err, schedule)`.
- `SCHEDULE_NOTE_PREFIX = "[schedule fired]"` — stable marker text the creator's model and UI key off.
- `tickSchedules(db, now: number, fire: (schedule) => void): Schedule[]` — one pass; returns the due schedules in order. `fire` is a parameter for testability.
- `interface TickerDeps extends FireDeps { intervalMs?; fire?: (ctx, schedule) => unknown }`
- `startScheduleTicker(ctx: AppCtx, deps = {}): () => void` — interval loop, timer **unref'd**, returns a stopper. No immediate pass at boot (first tick lands one interval in — gives orphan-recovery a moment; worst wait 30s).
- REST handlers (hoisted `function` declarations to break the TS import cycle with `server/app.ts` — irrelevant in Rust): `listSchedulesH` (GET /schedules, creation order, includes disabled), `createScheduleH` (POST, 201 with stored row, `nextRunAt` computed), `patchScheduleH` (PATCH /schedules/:id, empty body = legal no-op), `deleteScheduleH` (DELETE, 404 on unknown id, returns `{ok: true, removed: id}`).

### hostfn/schedule.ts (grammar + CRUD the ticker depends on)
- `type ParsedSpec = { kind: "every"; ms } | { kind: "daily"; hh; mm }`
- `SPEC_HELP` = `"every:<N><m|h|d> with N ≥ 1 (every:30m, every:2h, every:1d) or daily@HH:MM in local wall-clock time (daily@09:00)"` — the exact string error messages embed; a REST test asserts `/every:<N><m\|h\|d>/` appears in the 400 body.
- `parseSpec(spec): ParsedSpec | null` — regexes `^every:(\d+)(m|h|d)$` (N ≥ 1; `every:0m` rejected — zero interval fires every tick forever) and `^daily@(\d{1,2}):(\d{2})$` (hh ≤ 23, mm ≤ 59). Units: m=60_000, h=3_600_000, d=86_400_000 ms.
- `nextRun(spec, from): number` — **strictly after** `from`, never equal. `every` = `from + ms`. `daily` resolves in LOCAL time via calendar arithmetic (set hh:mm:00.000 today local; if `<= from`, add one calendar day) — DST-absorbing: the run stays at HH:MM local on either side of a transition. Throws `ScheduleError(400, "invalid schedule spec: <spec> — use " + SPEC_HELP)` on a bad spec.
- `scheduleCreate(db, body, deps)` / `schedulePatch(db, id, patch, deps)` / `scheduleRemove(db, id)` — validated CRUD shared by REST and the `schedule.*` host fn (one validated path, deliberately). Patch recomputes `nextRunAt` from now in EXACTLY two cases: spec changed, or disabled→enabled. `scheduleVerb(db, verb, args, defaultWorkspace, deps)` dispatches list/add/enable/disable/remove; `createScheduleHostFn(ctx, deps)` adapts JSON strings and stamps `sessionId: ctx.sessionId` (report-back target is NEVER taken from the wire — a program must not point another conversation's wake at itself; REST path leaves it null).
- `resolveWorkspace(raw)` — trims, expands `~`/`~/x` against homedir, `resolve()`s absolute, requires an existing directory (stat), else `ScheduleError(400, "workspace does not exist: <abs>. Point the schedule at a checkout that is there now — every firing opens a session in it.")`.

### scratch.ts
- `MAX_AGE_MS = 14 * 24 * 60 * 60_000` (two weeks)
- `ensureScratchDir(sessionId): string` — `mkdir -p` the session dir; **never throws** (swallows mkdir failure; the next write path reports in its own terms); always returns the path.
- `interface SweepOptions { maxAgeMs?; now?; root? }`
- `sweepScratch(opts = {}): string[]` — deletes scratch DIRECTORIES (only directories — a loose file in the root is left alone) whose **dir mtime** is strictly older than `maxAge` (`now - mtime <= maxAge` keeps). Missing root → `[]`, not an error. Per-entry stat/rm failures silently skipped. Returns removed names.

### types.ts (ports & seams — becomes the shared crate)
- `interface Bus { publish(event: EventInput): BoughEvent; subscribe(listener): () => void; readonly size: number }`
- `interface Db` — the full typed persistence port; ~60 methods across sessions, messages, turns, root-scoped KV state, schedules, workflows, command-history memory, FTS search. Ordering contracts stated on the interface: `messagesFor` orders by `(created_at, rowid)`; `threadFor` = ancestors root→parent then own; `ancestorChain` walks to the lineage root; `listSessions` newest first with visibility as the CALLER's derivation (excludes `subagent`/`workflow_agent` kinds via `sessionsByOrigin`); `dueSchedules(now)` = enabled rows with `next_run_at` passed; `markScheduleRun(id, lastRunAt, nextRunAt)` — "Advances `next_run_at` FROM NOW, never from the stale value"; `deleteMessagesFrom` is the only destructive thread write; `indexMessage` idempotent; `rebuildSearchIndex` must equal incremental indexing. (Full signatures: see `src/types.ts:185-316` — the db subsystem's spec owns the implementations; root owns the port shape.)
- Support shapes: `SessionRuntime {workspace: string|null, base: string|null}`, `UsageTotals` (inputTokens, outputTokens, reasoningTokens, cacheReadTokens, cacheWriteTokens, costUsd), `SearchHit {messageId, sessionId, snippet, createdAt}`, `CommandRecord`, `PriorFailures`, `TagDiversityDay`, `TaggedCommand`, `CommandTagRow` (exact field names in source; camelCase in TS structs, snake_case in the DB).
- LLM boundary: `Effort = "low"|"medium"|"high"|"xhigh"|"max"`; `LlmBlock` = text | reasoning(`meta` opaque provider payload replayed VERBATIM within and across turns, never inspected outside the provider's own mapper) | tool_use; `LlmContentBlock` adds tool_result{toolUseId, content, isError} and image{data: base64, mediaType, name}; `LlmMessage {role: "user"|"assistant", content: LlmContentBlock[]}`; `LlmToolDef {name, description, inputSchema}`; `LlmParams {model, system?, systemVolatile?, maxTokens, messages, tools, toolChoice?: "none", effort?}` — `system` is the STABLE prefix (byte-identical across sessions per delegation tier, prompt-cache contract), `systemVolatile` the per-session suffix with its own cache breakpoint; `LlmResult {content, stopReason, usage?}`; `LlmClient.run(params, onText, signal?)` — the whole provider surface, provider-specific handling must not leak past it; `CheapTier {title, ghostText, activity}` — each resolves `null` on failure, NEVER rejects; one in-flight blurb per session, drop don't queue.
- `interface AppCtx { db; bus; llm?; model?; effort?; now?; cheap? }` — what every HTTP handler receives; built once at boot.
- `interface TurnCtx extends AppCtx { sessionId; turnId; messageId; workspace; model; signal: AbortSignal; exits?; record?; reads?; touched?; mcpGrant?; depth: number }` — the shared-across-construction-paths arrays (`exits`, `record`, `reads`, `touched`) are ON the ctx precisely because host fns are built from it in two places (`baseHostFns` and `delegationDeps`); a closure-local version was silently bypassed and shipped green tests that did nothing live.
- `interface HostFns` — string-in/string-out bridge (`bash(cmd, tags)` with REQUIRED tags enforced at runtime too, `sh`, `bashBg(name first, required)`, `bashOutput/Wait/Kill`, `view`, `patch`, `write`; optional = capability grant: `agent?/spawn?/join?/adopt?`, `workflow?`, `ask?`, `state?`, `schedule?`, `artifact?`). `bash` auto-backgrounds past 60s. `sh` never throws on non-zero exit. Compile-time proof (`UnboundHostFn`/`UnknownHostFn` must be `never`) that `HostFns` matches `HOST_FN_NAMES` in `harness/protocol.ts` — replicate as a Rust unit test or exhaustive match.
- `interface WorkflowHostFns { agent; phase; log }`.

### schema/events.ts (the envelope Bus stamps)
- `EVENT_TYPES` (closed, frozen): `session.created`, `session.updated`, `session.activity`, `message.started`, `message.delta`, `message.part`, `message.finished`, `message.retry`, `tool.log`, `turn.finished`, `ask.question`, `job.spawned`, `job.exited`, `workflow.updated`, `workflow.agent`, `workflow.log`.
- Envelope: `{type, sessionId?, seq: number, ts: number, data: unknown}`. `data` shape is per-type (`EventDataMap`); the envelope is parsed on the client socket, payloads are typed only.
- `EventInput` = the same minus `seq`/`ts` (bus assigns them).

---

## 3. Data structures

**Schedule** (wire + DB row, `schema/parts.ts:413`):
```
{ id: string, title: string, prompt: string, workspace: string|null,
  sessionId: string|null (default null — older rows must still parse),
  spec: string (stored VERBATIM), enabled: boolean,
  createdAt: number, lastRunAt: number|null, nextRunAt: number }
```
JSON field names exactly as above (camelCase on the wire; DB columns are snake_case: `last_run_at`, `next_run_at`).

**Request bodies** (`schema/requests.ts`):
- `CreateScheduleBody`: `{title: min1, prompt: min1, workspace?: min1, spec: string, enabled?: bool}` — spec grammar validated by the schedules module, not the schema.
- `PatchScheduleBody`: all optional; `workspace: string|null` — **null clears it** (tri-state: absent = keep, null = clear, string = resolve & set).

**DB tables touched by this subsystem**: `schedules` (via the Db port methods only — schema is FROZEN; `session_id` was the second sanctioned ALTER and new columns go at the END). Firing writes `sessions` + `messages` rows and FTS index rows via port methods.

**Event envelope**: `{type, sessionId?, seq, ts, data}` — see §2. `turn.finished` data: `{turnId, sessionId, status, error?}`. `message.delta`: `{messageId, delta}`. Others per `EventDataMap`.

**Fired-session row shape** (what `fireSchedule` writes): `kind: creator-exists ? "schedule_run" : "root"`; `parentId: null` ALWAYS (no thread inheritance); `originId: creator.id` only when creator exists (visibility lineage, carries no context); NO `originMessageId` ("the clock asked for it, not a turn"); `title = schedule.title`; workspace + `originDir` both set to `schedule.workspace` when present, absent otherwise. First message: `role: "user"`, `parts: [{type: "text", text: schedule.prompt}]`, `pending: false`.

**Report-back note text** (exact, three lines joined with `\n`):
```
[schedule fired] "<title>" <RAN_TEXT[status]> (session <firedSessionId>).
Report:\n<report>            — or the literal line: No report.
Act on it only if it needs something — the run is its own session, and this note is its outcome.
```
`RAN_TEXT`: done→`finished`; error→`FAILED — its turn errored, and the report below carries the error`; interrupted→`was STOPPED before it finished`; orphaned→`ended without a completed turn`.

---

## 4. Behaviors & edge cases (mined from tests + code)

### Bus (bus.test.ts)
- Thrower isolation tested with a thrower **before** healthy listeners (order: healthy, thrower, healthy, thrower, healthy → survivors all run in order; both throws reported).
- A listener throwing a **non-Error** value, or an `onListenerError` reporter that itself throws, still must not break fan-out (reporter call is wrapped in its own try/catch).
- `seq` starts at 1, increments by exactly 1 per publish, **per instance** (no global counter), advances with zero listeners and when every listener throws, same seq seen by all subscribers of one event, never repeats.
- `ts` from injected clock; the returned event and the delivered event are the same object; input object not mutated (no `seq`/`ts` keys added to it).
- Delivery strictly synchronous — listener has run before `publish` returns (a microtask hop would let two emits interleave out of seq order).
- Iteration is over the **live set**: a listener unsubscribed mid-fan-out by an earlier listener (or by itself) does NOT receive the in-flight event. Rust note: iterating a Vec snapshot delivers to it — wrong direction. Check liveness per-listener during iteration (e.g. keyed slab/HashMap, check membership before call, collect removals).
- Unsubscribe idempotent; 100 subscribe/unsubscribe cycles leak nothing (`size` back to 0).
- Payloads are NOT validated at publish (compile-time contract only).

### confine (paths.test.ts — check both directions)
- Accepts: relative under root; `./c`; candidates **containing** `..` that land back inside (`c/../d` → `/a/b/d`); absolute candidates already inside; candidate `""` or `"."` → the root itself; root with trailing/doubled separators or `.` segments normalized; relative root resolved against cwd; filesystem root `/` without doubling (`confine("/", "etc")` → `/etc`; `confine("/", "..")` → `/` — inside, not escape).
- Rejects (`PathError`, status 400): `..` out of root incl. landing exactly on the parent; chains of harmless-looking segments that resolve outward; absolute outside; **string-prefix siblings** (`/a/bc` vs root `/a/b` — guard is `base + sep` prefix, with `endsWith(sep)` check so `/` doesn't become `//`); NUL byte in root OR candidate (would truncate at the syscall boundary — reject before the OS sees it).
- Error message must contain the candidate, the resolved landing path, and the root (test asserts all three substrings), plus the resolving move ("Use a path that stays under <base> — \"..\" segments and absolute paths outside the root are rejected.").
- **Purely lexical**: nothing stat'd, no symlink followed. A symlink inside root pointing outward is ACCEPTED (`confine(root, "link")` ok); traversal routed *through* it (`link/../../outside/x`) still collapses lexically and is rejected. Pinned by test so a later move to fs-based resolution is deliberate.
- Symlinked root and its realpath are different namespaces (macOS `/tmp` vs `/private/tmp` test) — callers must build root and candidate from the same source (`boughPath()`). Rust hazard: `std::fs::canonicalize` follows symlinks and requires existence — do NOT use it; implement lexical normalization (components walk handling `.`, `..`, absolutes) yourself or via a lexical crate.
- `BOUGH_HOME` blank or whitespace-only → fall back to `~/.bough` ("a shell accident, not a request to put the data root at the cwd").
- Env vars are read **per call** — tests set/restore around each assertion.

### Schedule ticker (schedules.test.ts)
- THE test: schedule `every:1h` due at T0+1h, server returns at T0+6h → `tickSchedules` fires it exactly once; `lastRunAt = back`, `nextRunAt = back + 1h` (advanced FROM NOW — from the stale value it would be T0+2h, already past, and refire every tick). Ticks between then and the next slot are quiet; at back+1h it fires again exactly once.
- `daily@09:00` missed for a week fires once; next occurrence = tomorrow 09:00 **local** (today's already past at 09:30).
- Advance-before-fire: a `fire` that throws is swallowed (logged), the row is already advanced, next tick 30s later does NOT retry. One bad schedule must not abort the rest of the pass.
- One pass fires every due schedule, skips disabled, skips not-yet-due.
- Real-ticker test: clock frozen 5 slots past due, interval 2ms, many ticks — exactly one firing; advance the clock past the next slot with a fresh ticker — one more. Stopper genuinely stops (make the schedule due again after `stop()`, nothing fires).
- Timer is **unref'd** — the ticker alone never keeps the process alive. Tokio note: a spawned interval task doesn't block shutdown the way a Node timer does, but you still need the stopper (abort handle / CancellationToken) so tests and `bough exec` can tear it down.

### Firing (schedules.test.ts)
- Publishes `session.created` then `message.started`, in that order, BEFORE starting the turn (live TUI renders the session already carrying its prompt).
- Message is FTS-indexed via `indexQuietly` — an index failure is logged and swallowed ("degraded search, never a lost firing").
- Collapse rule: creator session exists → `kind: "schedule_run"`, `originId = creator.id`, top-level listing still shows only the creator, drill-in via `sessionsByOrigin`. Creator absent (REST-created schedule, or creator deleted/branched away) → plain `"root"`, no originId — "a collapsed session with no reachable origin would be invisible in every listing, which is a worse failure than an untidy one."
- Collapsed but NOT inherited: `parentId` null either way; thread length 1; no `originMessageId`.
- No workspace on the schedule → session unpinned (`getSessionRuntime(...).workspace === null`).
- Turn starter (`(ctx as AppCtx & WithTurnStarter).startTurn`) is read **structurally** off ctx; absent starter = pre-M2 shape, session + message still recorded, no error. Starter that throws synchronously → `fireSchedule` returns `null`, error reported, session + message SURVIVE in the DB ("the user can see what was supposed to run and post into it").
- Fire-and-forget: if `startTurn` returns a Promise, settle handler runs on resolve AND on reject (reject additionally reported); non-Promise return → settle immediately.
- Report-back (`noteFiringOutcome`): only when `schedule.sessionId` set. Outcome read from the **DATABASE** (last turn row of the fired session + `buildResult`), not from the settled promise — "the note and the transcript can never disagree" across restarts. No turn row → status "orphaned". Note posted via `postSystemNote` to the creator (role `system`), which **wakes an idle creator** (starts a turn there — test asserts started sessions = [fired, "creator"]). Missing creator session → `postSystemNote` answers `dropped`; correct outcome, not an error. A FAILED run still notes (text matches `/FAILED/`), and the starter's rejection is reported separately — the note does not swallow it.
- REST: POST computes `nextRunAt` with the **ctx clock** (test pins `T0 + 2h`); bad spec → 400 whose `error` field names the grammar; PATCH/DELETE unknown id → 404; PATCH with no body at all → 200 no-op (body default `{}`); DELETE returns `{ok: true, removed: id}`.
- Patch recompute: ONLY on spec change or disabled→enabled ("otherwise the disabled stretch reads as downtime and the schedule fires the instant it is switched back on"). Title/prompt/workspace edits leave `nextRunAt` alone.
- `every:0m` (or 0 anything) must be rejected at parse; `daily@24:00`, `daily@09:60` rejected. Old-style cron (`0 9 * * *`) rejected with the grammar help.

### Scratch (scratch.test.ts)
- Sweep: strictly-older-than-maxAge dirs removed (fresh 1min and 1-day-old dirs kept at 14-day threshold); comparison is `now - mtime <= maxAge` → keep (exact-boundary kept).
- Criterion is dir **mtime**, never the session row — "a conversation can be months old and still be the one you are working in".
- Missing root → `[]` (nothing has ever been written).
- A loose FILE in the root is never deleted (only directories) — "a recursive delete of anything it finds is how a bug here becomes data loss."
- Wiring: `sweepScratch()` at server boot only, whole call wrapped in try/catch ("a scratch root that cannot be read is not a reason to refuse to start"); `ensureScratchDir(sessionId)` called by the turn runner before the prompt names the path, and the dir is exported to every shell command as `$BOUGH_SCRATCH` (hostfn/shell.ts, hostfn/jobs.ts) and used as the output-spill target (hostfn/spill.ts).

### BOUGH_PORT
Not in these modules but part of root's env contract: server binds and TUI/`bough exec` connect via `BOUGH_PORT`, default `4321` (`DEFAULT_PORT` in `tui/api.ts` and `cli/exec.ts`; loopback only). Coexistence story: `BOUGH_PORT` moves the listener + `BOUGH_HOME` relocates the tree = rewrite runs beside the live install; defaults at cutover are the product's (4321, `~/.bough`).

---

## 5. Dependencies

Imports (non-test):
- `bus.ts` ← `schema/events.ts` (types), `types.ts` (port). Imported by `server/main.ts` (constructs the one Bus into `AppCtx`), `schedules.test`-style fixtures, SSE endpoint via ctx.
- `errors.ts` ← nothing. Imported by nearly everything (~30 modules): paths, llm/*, agents/*, server/*, mcp/*, turn/*, workflow/*, hostfn/*.
- `paths.ts` ← `errors.ts` (PathError), node:os, node:path. Imported by scratch, server (artifacts/comments/attachments/theme/skills/fs), db open, mcp config, workflows, maps skill.
- `schedules.ts` ← `schema/requests.ts`, `schema/parts.ts`, `types.ts`, `agents/notes.ts` (postSystemNote), `agents/subagent.ts` (buildResult, SubagentResult), `hostfn/schedule.ts` (nextRun + CRUD), `server/http.ts` (json, parseBody), `server/sessions.ts` (WithTurnStarter type). **Import CYCLE with `server/app.ts`** (app imports the four handlers; schedules imports app's helpers) — resolved in TS by hoisted function declarations; in Rust simply put the route table and handlers in separate modules, no cycle needed.
- `scratch.ts` ← `paths.ts`, node:fs. Imported by `server/main.ts` (boot sweep) and `turn/runner.ts` (ensureScratchDir, twice).
- `types.ts` ← `schema/parts.ts`, `schema/events.ts`, `harness/protocol.ts` (HostFnName). Imported by everything — it IS the shared-types module.

Wiring in `server/main.ts`: `startScheduleTicker(ctx)` started AFTER the turn starter is installed on ctx (so the first firing has a turn to run), stopper deliberately discarded (unref'd timer dies with the process); `sweepScratch()` after the listener is up.

---

## 6. External deps → Rust equivalents

| TS / Bun | Used for | Rust |
|---|---|---|
| `node:os homedir()` | data root | `dirs::home_dir()` (or `home` crate); falls back — treat `None` as an error at startup, it "never throws" in practice |
| `node:path join/resolve/sep` | lexical paths | `std::path::PathBuf` + hand-rolled lexical normalize (see §4 hazard: NOT `canonicalize`) |
| `process.env` per call | BOUGH_HOME/BOUGH_DB/BOUGH_PORT | `std::env::var` per call (tests mutate env: serialize env-touching tests or take the root as a parameter) |
| `node:fs mkdirSync/readdirSync/statSync/rmSync` | scratch | `std::fs` (`create_dir_all`, `read_dir`, `metadata().modified()`, `remove_dir_all`) |
| `setInterval` + `.unref()` | ticker | `tokio::time::interval` in a spawned task + `CancellationToken`/`JoinHandle::abort` as the stopper |
| `crypto.randomUUID()` | session/message/schedule ids | `uuid::Uuid::new_v4()` |
| `Date` local calendar math | `daily@HH:MM` nextRun, DST-absorbing | `chrono` with `Local`: `Local.with_ymd_and_hms`, handle `LocalResult::Ambiguous/None` at DST edges (spring-forward gap: pick the valid instant — TS `Date.setHours` resolves it silently; pin with a test) |
| `zod` (Schedule, request bodies, event envelope) | parse/validate | `serde` + `serde_json`, `#[serde(rename_all = "camelCase")]`, `#[serde(default)]` for `sessionId`; validation (min-length, spec grammar) as explicit fns returning the taxonomy errors |
| `Error` subclass hierarchy w/ `name` | error taxonomy | one `enum BoughError` (thiserror) with `status()` and `name()` accessors — see §7 |
| `console.error` defaults | listener/tick/fire reporting | `tracing::error!`; keep the injectable reporter seam for tests |
| Bun `Request`/`Response` handlers | REST | axum handlers over shared `AppCtx` state |

---

## 7. Suggested Rust layout

Workspace crate `bough-core` (no server, no TUI deps):

- `core/src/error.rs` — `#[derive(thiserror::Error)] enum BoughError` with one variant per class that carries distinct data (`Llm { status: u16, retry_after_ms: Option<u64>, message }`), plus a generic `Http { status, name, message }` for the caller-status families (Turn/Agent/Workflow/Branch/Changes/Schedule/State/Artifact/Mcp/Lsp/Net/Skill and the Branch subclasses can be a `kind` enum inside one variant — the only behavioral needs are `status()`, `name()` for the JSON body, and `Display` = message). Do NOT flatten messages: every constructor-site message string in TS is load-bearing model-facing text; port them verbatim.
- `core/src/paths.rs` — free functions mirroring the accessors; `confine(root: &Path, candidate: &Path) -> Result<PathBuf, BoughError>` with a private `lexical_resolve` (component walk: `CurDir` skip, `ParentDir` pop, absolute restarts). NUL check via `as_os_str().as_encoded_bytes().contains(&0)`.
- `core/src/bus.rs` — `struct Bus { seq: AtomicU64 or Mutex<u64>, listeners: Mutex<HashMap<u64, Listener>> }`. Two viable shapes: (a) callback-based like TS (`Listener = Box<dyn Fn(&BoughEvent) + Send + Sync>`, publish iterates ids sorted by insertion, re-checking membership so mid-fan-out unsubscribe skips delivery, `catch_unwind` around each call for isolation); (b) idiomatic `tokio::sync::broadcast`. **Prefer (a)**: broadcast is async-delivered (violates the synchronous, in-seq-order contract), drops on lag, and can't express "unsubscribed mid-fan-out doesn't receive". Subscribe returns a guard/closure that removes by id; `size()` = map len. Rust panics ≠ TS throws: real SSE senders will return `Err` on closed channels rather than panic, so isolation mostly becomes "ignore send errors" — keep `catch_unwind` anyway to honor the contract, and keep the injectable `on_listener_error`.
- `core/src/events.rs` — `enum EventType` (closed, `#[serde(rename = "message.delta")]`-style), `struct BoughEvent { r#type, session_id: Option<String>, seq, ts, data: serde_json::Value }` on the wire; internally a typed `enum EventData` per `EventDataMap` if the TUI store wants exhaustive matching (it does — that was the point of the closed set).
- `core/src/types.rs` — `trait Db` (big; `&self` methods, implemented by rusqlite behind a Mutex or a dedicated thread — db subsystem's decision), `trait LlmClient` (`async_trait`), `trait CheapTier`, `struct AppCtx { db: Arc<dyn Db>, bus: Arc<Bus>, now: Clock, ... }` where `Clock = Arc<dyn Fn() -> i64 + Send + Sync>` (or a `Clock` trait) to keep the injected-clock seam. `TurnCtx` wraps `Arc<AppCtx>` + turn fields; the shared `exits/record/reads/touched` vectors become `Arc<Mutex<Vec<...>>>` ON the ctx (the TS comment about the closure-local bug is the reason).
- `core/src/schedule/spec.rs` — `ParsedSpec`, `parse_spec`, `next_run(spec, from_ms) -> Result<i64>`, `SPEC_HELP` const. Pure; unit-test the DST edges and every grammar rejection from §4.
- `core/src/schedule/crud.rs` — create/patch/remove/verb over `&dyn Db`, `WorkspaceResolver` as an injectable `async fn` (trait or boxed closure); production resolver does `~` expansion + is_dir stat.
- `core/src/schedule/ticker.rs` — `tick_schedules(db, now, fire) -> Vec<Schedule>` pure-ish; `fire_schedule(ctx, schedule, deps) -> Option<FiredSchedule>` (never-panics contract: internal `Result` mapped to report+None); `start_ticker(ctx) -> CancellationToken` spawning `tokio::spawn(async { interval... })`. `startTurn` structural-read becomes an `Option<Arc<dyn TurnStarter>>` field on AppCtx (set after boot wiring — use `OnceLock`/`RwLock<Option<...>>` since ctx is built before the starter exists). The settle path (`note_firing_outcome`) is a spawned task on the turn future's completion, reading outcome from the DB.
- `core/src/scratch.rs` — `ensure_scratch_dir`, `sweep_scratch(opts)`; sync `std::fs` is fine (boot-time + per-turn mkdir; wrap in `spawn_blocking` only if the server crate cares).
- Server crate: the four REST handlers as axum routes; no import cycle to reproduce.

Async boundaries: bus publish is sync; ticker + fire + report-back are tokio tasks; schedule CRUD is async only because of the workspace stat (could be sync `std::fs` — simpler, acceptable).

## 8. v1 scope cut

Core (cannot cut — the loop breaks without them): `errors` taxonomy (everything returns it), `paths` incl. `confine` (db/artifacts/scratch all key off it), `types` ports (`Db`, `Bus`, `AppCtx`, `TurnCtx`, LLM boundary), `bus` (SSE = the TUI's whole feed), `scratch.ensure` (`$BOUGH_SCRATCH` is in the prompt contract).
- **Stub for v1**: the whole schedule subsystem — ship `parse_spec`/`next_run` + the `Schedule` type (cheap, pure, fully specified) but stub the ticker, firing, report-back, REST, and `schedule.*` host fn behind a `todo!`-free no-op (list returns `[]`, add returns 400 "not yet ported"). Nothing in the core turn loop depends on it; the DB rows are untouched so the live install loses nothing.
- **Stub**: `sweepScratch` (a root that grows for a few weeks is harmless; `ensure` is the load-bearing half).
- **Defer**: `CheapTier` (explicitly designed to degrade to nothing when absent), the command-history/tag portions of the `Db` port (`recordCommand` … `programForMessage` — 12 methods; make them default no-ops on the trait so the port compiles), workflow portions of the port likewise.
- **Do not cut**: bus listener-isolation + live-set semantics (tests exist for exactly the failure modes); `confine`'s full contract (every rejection case in §4 — a partial port is a served `/etc/passwd`); per-call env resolution (the beside-the-live-install story depends on it).
