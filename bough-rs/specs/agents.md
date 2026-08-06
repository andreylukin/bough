# Port spec: `src/agents/` — subagents-as-branches (caps, launch, notes)

Three modules, ~1,376 lines of source, ~1,956 lines of tests. This subsystem is the
whole delegation story: how a subagent session comes into being (`subagent.ts`), how
many may exist (`caps.ts`), and how a finished/failed/orphaned child's report reaches
its spawner (`notes.ts`).

---

## 1. Purpose & invariants

Each module opens with a stated invariant. Quoted verbatim (these are the contracts
the Rust port must preserve, not commentary):

### `caps.ts`
> THE INVARIANT THIS HOLDS: **a refused launch costs nothing.** Not the slot it
> asked for, not a sibling that already started, not the budget of the turn it was
> launched from. Fan-out is written as `Promise.allSettled` over N launches
> precisely because some of them are expected to be refused (plan §6.9), so the
> cap has to behave like a `Promise.reject` for exactly one element of that array
> — every other launch continues, and the ledger afterwards reflects the launches
> that actually happened and no others. A cap that unwound a sibling, or that
> charged a refused launch against the per-turn budget, would turn the harness's
> own recommended fan-out idiom into a lossy one.

Supporting design theses in the same header (each is load-bearing):
- **Ledger, not query**: counts derived from `db.listSessions()` are wrong under
  synchronous fan-out (check-then-create with an `await` between = twelve checks all
  see zero). The check and the take are ONE synchronous function (`reserve`) — atomic
  by construction on a single-threaded runtime. **Rust delta: this atomicity must be
  reproduced with a `Mutex` around the whole reserve (check both caps + take both
  slots under one lock), since tokio is not single-threaded.**
- **Concurrency keyed by TREE, not session**: "four running at once" is a property of
  a piece of work; the key is the tree root (top non-subagent session of the lineage).
  Nested and sibling launches share one budget; different trees hold independent budgets.
- **Slot taken at reservation, not when a turn is seen running**: held from `reserve()`
  until the lease is released; the bus attachment backstops dropped leases.
- **Depth cap is NOT here** — it lives in `subagent.ts` with the code that writes
  lineage. What caps.ts owns of nesting: `assertMayDelegate` refuses a *detached*
  `spawn()` from inside a subagent turn (nested delegation is blocking-only).

### `subagent.ts`
> THE INVARIANT THIS HOLDS: **a subagent starts from nothing but its task.** It is
> a real session (`kind: "subagent"`) with `parentId: null`, and that null is the
> whole feature. `db.threadFor` is "every ancestor's messages, then my own", so a
> parent pointer would silently hand the child the spawner's entire conversation —
> every earlier turn, every tool dump, every abandoned plan. With `parentId: null`
> the child's thread is exactly the one message this module seeds, which is why the
> task string has to carry every path, constraint and acceptance criterion: there is
> no earlier conversation to consult and nobody to ask (spec §7).

Three things that DO deliberately cross the spawner→child boundary:
1. **The lineage edge** — `originId` / `originMessageId` (spawning session + the
   supervisor message in flight). The only record the branch exists; visibility is
   derived from it.
2. **The checkout** — the child works in the SAME workspace. No per-agent worktree,
   nothing to merge; the spawner owns giving concurrent children disjoint files.
3. **The MCP grant** — `ctx.mcpGrant` is copied into the child ctx at spawn time, so
   a later manual continuation of the branch does NOT inherit it.

### `notes.ts`
> THE INVARIANT THIS HOLDS: **a note reaches the session exactly once, and never as
> a second concurrent turn.** A session runs at most one turn at a time (spec §5),
> and the things that post here — a detached subagent finishing, a background shell
> exiting, an artifact comment batch — all arrive from *outside* any turn, at a
> moment nobody chose.

Every post lands in one of two states, no third: spawner idle → note starts a fresh
turn; turn in flight → note is persisted + announced immediately, and rides the queued
drain (`hasUnansweredInput` in `turn/queue.ts` finds it — the note is **persisted
before it is decided upon**, so the queue derivation reads it from the DB, restart-safe).

Two deliberate non-wakes:
1. **A stop stays stopped** — if the session's own last finished turn ended
   `interrupted`, record without waking (a user stop cascades into detached children;
   their completion notes must not restart the stopped work).
2. **Boot recovery** — orphaned-subagent notes are recorded, never woken (`wake: "never"`);
   a restarting server must not spend tokens on sessions nobody has returned to.

---

## 2. Public API

### `caps.ts`
| Export | Signature | Semantics |
|---|---|---|
| `MAX_SPAWNS_PER_TURN` | `= 8` | Total launches (blocking + detached) per turn. Never decremented; waiting does not clear it. Bounds sequential loops. |
| `MAX_TREE_CONCURRENT` | `= 4` | Subagent turns in flight at once across one tree. |
| `DelegationMode` | `"blocking" \| "detached"` | `"blocking"` = `agent()`, `"detached"` = `spawn()`. |
| `treeRootOf(db, sessionId) → string` | pure over DB | Walk `originId` up while `kind === "subagent"`; stop at first non-subagent. Fork/compaction = own tree. Dangling origin → stop where you are (never throw). Hop cap 16 (private `MAX_LINEAGE_HOPS`) guards against a bad lineage write cycling. |
| `SpawnLease` | interface | One taken slot: `treeId`, `turnId`, `released` (getter), `sessionId` (getter, null until bound), `bind(sessionId)`, `release()` (idempotent — load-bearing, both release paths fire for a normal child). |
| `exemptLease() → SpawnLease` | | No-op lease for cap-exempt launches (workflows). A distinct object, NOT null/Option — call sites bind/release unconditionally. Still tracks `released`/`sessionId` state. `bind` after release is a no-op. |
| `CapLimits` | `{ perTurn?, concurrent? }` | Test seam; defaults = the constants. |
| `SpawnCaps` | class | The ledger. `reserve({turnId, treeId}) → SpawnLease` (throws `SpawnCapError` taking NEITHER slot); `running(treeId?) → number` (no arg = total across trees); `spawnedInTurn(turnId) → number`; `attachBus(bus) → unsubscribe` (replaces any previous attachment — calls old detach first); `reset()` (tests only). |
| `spawnCaps` | `= new SpawnCaps()` | Process-wide singleton; boot attaches the bus; tests construct their own. |
| `assertMayDelegate(ctx: {depth}, mode, verb?)` | throws `AgentError(400)` | Refuses `detached` when `ctx.depth >= 1`. `depth` is `TurnCtx.depth`, a **tier flag** (runner sets 1 for any subagent/workflow-agent turn), not a hop count. Default verb: `"spawn()"` / `"agent()"` from mode. |
| `ReserveOptions` | `{ mode, verb?, exempt?, caps? }` | `exempt` = workflows: skip both width caps, nesting rule STILL applies. |
| `reserveSpawn(ctx: Pick<TurnCtx,"db"\|"sessionId"\|"turnId"\|"depth">, opts) → SpawnLease` | | `assertMayDelegate` → (if exempt: `exemptLease()`) → `caps.reserve({turnId, treeId: treeRootOf(db, sessionId)})`. |
| `LeasedLaunch` | `{ sessionId: string; result: Promise<unknown> }` | Structural, so caps.ts never imports the launch module. |
| `underLease(lease, launch: () => T) → T` | | Run `launch()`; on throw: `lease.release()` + rethrow; on success: `lease.bind(started.sessionId)`, then `result.then(release, release)`. |
| `cappedLaunch(ctx, opts, launch) → T` | | `underLease(reserveSpawn(ctx, opts), launch)` — what a delegation host fn calls. |

`SpawnCaps.reserve` internals (order matters):
1. Read per-turn count; if `>= perTurn` throw `SpawnCapError` — message MUST name the
   cap (`per-turn limit (8)`), say waiting won't clear it, and name the move
   (`workflow.start` has no per-turn cap; launches already started are unaffected).
2. Read tree count; if `>= concurrent` throw `SpawnCapError` — message names
   `tree-wide limit (4)`, "counts every branch, not just this session's own children",
   the move (`Await or join() the ones in flight, then launch the rest as a second
   batch`), and that only this launch was refused.
3. Only on the path that takes BOTH: increment both maps, return a lease. (A launch
   refused for concurrency has spent no per-turn budget.)
4. Tests grep the messages: `/per-turn limit \(8\)/`, `/workflow/`,
   `/concurrency cap reached/`, `/tree-wide limit \(4\)/`, `/join\(\)/`. Preserve these
   substrings.

`SpawnCaps` internal state: `#spawns: Map<turnId, count>` (never decremented while
turn lives), `#running: Map<treeId, count>`, `#bound: Map<childSessionId, Set<SpawnLease>>`.
In memory ON PURPOSE — a persisted count is a lie after restart (running turns are
recovered as `orphaned`, so an empty ledger at boot is the truth).

Bus handling (`#onEvent`): on `turn.finished` only —
- `data.sessionId` present → release every lease bound to that child session (backstop).
- `data.turnId` present → `#spawns.delete(turnId)` (the spawning turn is over; without
  this the per-turn map grows by one entry per delegating turn forever).

Lease `release()` details: idempotent; decrement guarded (`held <= 1` → delete the
map entry — a count must never go negative or the cap is silently unenforceable);
unbinds from `#bound`. `bind()`: no-op if released / empty id / same id; rebinding
unbinds the old id first.

### `subagent.ts`
| Export | Signature | Semantics |
|---|---|---|
| `MAX_SUBAGENT_DEPTH` | `= 2` | Root (0) → subagent (1) → subagent (2); depth 2 is terminal. Checked against the **LINEAGE** (`subagentDepth`), never `TurnCtx.depth`. |
| `subagentDepth(db, sessionId) → number` | pure | Count hops: while `cur.kind === "subagent"` and `depth < 16`, `depth++`, follow `originId` (missing origin ends the walk via `getSession` returning undefined). 0 for root/fork/compaction. |
| `UNTITLED` | `= "untitled"` | Fallback title. |
| `TASK_STUB_CHARS` | `= 40` | Task-derived title budget. |
| `cleanSubagentName(name: unknown) → string \| undefined` | throws `AgentError(400)` on non-string non-nullish | Strip control chars (`[\x00-\x1f\x7f]` → space), collapse whitespace, trim. Empty → `undefined` (caller falls back). Length cap 48: `slice(0,47).trimEnd() + "…"`. |
| `taskStubTitle(task) → string` | | First line, whitespace-collapsed. Empty → `UNTITLED`. If > 40 chars: cut at 40, back up to last space **only if** `at > 20` (half the budget), trimEnd, append `…`. A 60-char single word → hard cut at 40 + `…`. |
| `SubagentOptions` | `{ name?, model?, effort? }` | The `{name}` bag of `agent(task, {name})`. |
| `SubagentResult` | `{ sessionId, title, ok, status, report, changedFiles }` | `status: "done"\|"error"\|"interrupted"\|"orphaned"`; `ok` = `status === "done"`; `report` never empty. |
| `SubagentHandle` | `{ sessionId, title }` | Available before the turn does anything. |
| `SubagentLaunch` | `SubagentHandle & { session, taskMessage, messageId, result: Promise<SubagentResult> }` | Handle now, result later. Three consumers: blocking `agent()` awaits `result`; detached `spawn()` returns the handle; workflow engine needs the id pre-completion. |
| `BeginTurn` | `(ctx: AppCtx, sessionId, deps?) → { message, done: Promise<TurnOutcome> }` | Seam; `beginTurn` from `turn/runner.ts` satisfies it. |
| `LaunchDeps` | `{ now?, turn?, begin?, timeoutMs?, changedFiles? }` | `timeoutMs` default: env `BOUGH_SUBAGENT_TIMEOUT_MS` (finite, > 0) else 15 min. `changedFiles: (session) → Promise<string[]> \| string[]` — a seam because a git diff at end would report the union of every concurrent sibling's work; the write verbs know what THEY wrote (`hostfn/files.ts: takeSessionWrites`, wired in `hostfn/delegate.ts`). |
| `launchSubagent(ctx: TurnCtx, task, opts?, deps?) → SubagentLaunch` | | See §4 ordering. |
| `buildResult(ctx: Pick<TurnCtx,"db">, sessionId, messageId, deps?, cause?) → Promise<SubagentResult>` | | Assemble from what the child **persisted** (DB, not the in-memory outcome) — a child whose server died mid-turn has no outcome object, and this still yields a truthful `orphaned`. `cause: { timedOut?, capMs? }`. |

### `notes.ts`
| Export | Signature | Semantics |
|---|---|---|
| `NoteStarter` | `(ctx: AppCtx, session, message) → unknown` | Structural restatement of `server/sessions.ts`'s `TurnStarter` — deliberately NOT imported (`agents/` must not depend on `server/`; sessions↔app is a load-order cycle). |
| `WakeOutcome` | `"started" \| "queued" \| "recorded" \| "dropped"` | `dropped` = no such session, nothing written. `recorded` = written + announced, woke nothing (interrupt / boot / no starter wired). |
| `NoteDelivery` | `{ message: Message \| null, wake }` | Tests assert on it; production ignores it. |
| `NoteDeps` | `{ registry?, now?, start?, wake?: "auto"\|"never", extra?: Part[], reportError? }` | `extra`: parts riding with the note (`image()` attaches pictures). `wake:"never"` for boot recovery. |
| `postSystemNote(ctx: AppCtx, sessionId, text, deps?) → NoteDelivery` | **never throws** | Every caller is a completion callback; a throw = unhandled rejection = process down. Missing session → `{message: null, wake: "dropped"}`, nothing written (FK would fail anyway). |
| `SUBAGENT_NOTE_PREFIX` | `= "[subagent finished]"` | Stable text — the TUI (`tui/lines.ts parseSubagentNote`) and the model both key off it. |
| `formatSubagentNote(result) → string` | | See §4 for exact shape. |
| `deliverSubagentNote(ctx: TurnCtx, result, deps?) → NoteDelivery` | | `postSystemNote(ctx, ctx.sessionId, formatSubagentNote(result), deps)` — ctx is the SPAWNING turn's, so `ctx.sessionId` is the spawner. Claimed (`join()`ed) results never reach here — `hostfn/delegate.ts` checks first. |
| `createNoteDeliverer(deps?) → (ctx, result) => void` | | The `deliver` seam `hostfn/delegate.ts` takes, deps bound. |
| `createJobNotifier(ctx: AppCtx, deps?) → (sessionId, text) => void` | | Background-shell exits (`hostfn/jobs.ts`) post through the same one wake rule. Registry formats its own text. |
| `noteOrphanedSubagent(ctx, orphan: OrphanedTurn, deps?) → Promise<NoteDelivery \| null>` | | `null` when the orphan owes nobody: not found / `kind !== "subagent"` / no `originId`. Otherwise `buildResult(...)` → post `formatSubagentNote` to `child.originId` with `wake: "never"`. |
| `noteOrphanedSubagents(ctx, orphans, deps?) → Promise<NoteDelivery[]>` | | Per-orphan try/catch: one failure must not abandon the rest; errors go to `reportError` (default `console.error`). |

---

## 3. Data structures

### Session row (fields this subsystem reads/writes — exact names, `schema/parts.ts`)
```
id, title, kind ("subagent" here; full enum: root|fork|compaction|subagent|
workflow_agent|schedule_run|shell), createdAt (ms number), parentId (nullable),
originId?, originMessageId?, workspace?, originDir?, base?, model?, effort?, draft?
```
`COLLAPSED_KINDS = [subagent, workflow_agent, schedule_run]`,
`DELEGATED_KINDS = [subagent, workflow_agent]` — canonical in the schema module
(three consumers had drifted into three literals). The launch writes:
`kind:"subagent"`, `parentId:null`, `originId:ctx.sessionId`,
`originMessageId:ctx.messageId`, `workspace` (from `db.getSessionRuntime(spawner).workspace ?? ctx.workspace` — the stored fact, not a re-lookup), `originDir: spawner.originDir ?? workspace`
(project identity survives a moved workspace), `base:null` (inheriting the spawner's
would report the spawner's work as the child's), `model:null`, `effort:null`,
`draft:null` (inherited model reaches the child via ctx only — pinning it would make
a later manual continuation stick to it).

### Message rows
Task message: `{ id: uuid, sessionId: child, role: "user", parts: [{type:"text",text:task}], pending: false, createdAt }`.
System note: `{ id: uuid, sessionId, role: "system", parts: [{type:"text",text}, ...extra], pending: false, createdAt }`.
`pending: false` is load-bearing both places — `pending` is the supervisor streaming
flag; a note left pending renders the session busy forever. `role: "system"` replays
to the model as user-side text; it is not a provider role.

### Turn row (read by `finalStatus`)
`db.turnForMessage(messageId).status`: `done|error|interrupted` pass through;
anything else — including `running` (row outlived its process) and a missing row —
is `orphaned`.

### Events published (Bus)
- `{ type: "session.created", sessionId: child, data: session }`
- `{ type: "message.started", sessionId, data: message }` (task message AND system notes)
- `{ type: "session.updated", sessionId: child, data: updatedSession }` — after the
  child's result assembles, so the rail retires the branch and the tree learns it failed.
- Consumed: `turn.finished` with `data: TurnFinishedData = { turnId, sessionId, status, error? }`.

### Errors
- `AgentError extends HttpError` — `(status, message)`; 400 for nesting/validation,
  404 for missing spawner.
- `SpawnCapError extends AgentError` — always status **429**. The distinction matters:
  tests assert a nesting refusal is `AgentError` but NOT `SpawnCapError` ("not a cap
  to retry later").

### `TurnCtx` / `AppCtx` (slices used here)
`AppCtx = { db, bus, llm?, model?, effort?, now?, cheap? }`.
`TurnCtx extends AppCtx + { sessionId, turnId, messageId, workspace, model, signal, depth, mcpGrant?, ... }`.
Boot stamps `startTurn?: NoteStarter` onto the AppCtx (declared locally in notes.ts as
`WithStarter` because AppCtx is frozen). The child ctx built by `launchSubagent` is
NARROW: `{ db, bus, llm?, model: opts.model ?? ctx.model, effort?, now?, cheap?, mcpGrant? }` —
nothing tying the child to the spawner's turn (`runner.drive` overwrites
sessionId/messageId/workspace/signal/depth; anything else would be a stale abort signal).

---

## 4. Behaviors & edge cases (mined from tests + code)

### Launch ordering (`launchSubagent`) — load-bearing, in this exact order
1. Validate task: non-empty string after trim, else `AgentError(400)` whose message
   contains "entire briefing" (nothing created — test asserts session count unchanged).
2. Spawner must exist: `AgentError(404)` otherwise.
3. Depth check: `subagentDepth(db, ctx.sessionId) >= 2` → `AgentError(400)` matching
   `/depth limit \(2\)/`, tells the model to do the work here.
4. Resolve workspace, title (`cleanSubagentName(opts.name) ?? taskStubTitle(task)`).
5. `createSession` → publish `session.created` **before** the task message (a client
   reconciling by id must never see a message for an unknown session).
6. `createMessage` (task) → `indexQuietly` (FTS index failure is logged, never thrown
   — degraded search, not a failed launch) → publish `message.started`.
7. Build the narrow `childCtx`; `begin(childCtx, session.id, deps.turn)` — the task
   lands BEFORE the turn begins because `beginTurn` reads the thread synchronously.
8. Arm the wall-clock timer: on fire set `timedOut = true` and
   `interruptTurn(session.id, deps.turn?.registry)`. `done.finally(clearTimeout)`.
9. `result = done → buildResult(ctx, id, message.id, deps, {timedOut, capMs}) →
   re-read session, publish session.updated → r`.
10. Return `{ sessionId, title, session, taskMessage, messageId, result }` — handle
    returned before the turn finishes (that IS the detached/blocking difference).

### Isolation (the headline test, asserted in BOTH directions)
- Child's stored thread at launch = `["user" task, "supervisor" empty placeholder]`,
  every message's `sessionId` is the child's own.
- The provider payload for the child's first round = exactly one user message, the
  task; a sentinel string present only in the spawner's transcript must never appear
  anywhere in the child's `LlmParams` JSON. A "helpful context" regression or a
  reintroduced parent pointer fails here.

### `buildResult` / `reportOf`
- `report` = concatenated `text` parts of the child's supervisor message, trimmed.
- Interrupt REASON is **appended, not a fallback**: a stopped child that wrote
  `"⏹ Stopped."` still gets the cause attached (`text + "\n\n" + reason`). Measured
  failure without this: the spawner guessed the cause and retried a deliberate stop.
- Reasons: timed out → `` It ran past its {round(capMs/1000)}s cap and was stopped. Give the next one less to do, or split it. ``; otherwise → `` It was stopped deliberately — by you, or by someone stopping this turn. Do not just retry it; the reason was a decision, not a fault. ``
- Empty-text fallbacks per status (never return ""): done → "The subagent finished
  without writing a report."; error → "The subagent errored before reporting.";
  interrupted → reason-prefixed variant or "The subagent was interrupted before
  reporting."; orphaned → "The subagent was orphaned (the server restarted) before
  reporting."
- `changedFiles` callback: exceptions swallowed (best-effort diff; the report must
  survive a git hiccup). Result array is copied (`[...]`).
- `title` falls back to `UNTITLED` when the session row is gone.
- `ok === (status === "done")`; the error report carries the actual error text (the
  runner appends it to the message; test asserts `/on fire/`).

### `formatSubagentNote` — exact shape (TUI parses it; keep verbatim)
```
[subagent finished] "{title}" ({sessionId}) — {STATUS_TEXT[status]}.
Changed files: {files.join(", ") | "not reported"}.
Report:\n{report}          (or `No report.`)
It worked in THIS session's checkout, so its edits are already here — read them before building on top; there is nothing to merge.
```
`STATUS_TEXT`: done → `finished`; error → `FAILED — its turn errored, and the report
below carries the error. Nothing retried it. Whatever it had already written is in the
checkout`; interrupted → `STOPPED — it was interrupted (a user stop, or it hit its
wall-clock limit). Whatever it had already written is in the checkout`; orphaned →
`ORPHANED — the server restarted before it finished. Whatever it had already written
is in the checkout`. Requirements the tests pin: four DISTINCT first lines; every
failure names what survived (`/already written is in the checkout/`); empty
changedFiles reads `not reported` (never "none" — the harness can't back that claim);
the no-merge line present.

### Wake rule (`wakeFor`) — order is the contract
1. `deps.wake === "never"` → `recorded`.
2. **Busy check FIRST**, against the `TurnRegistry` (not the DB — a turn claims the
   session synchronously in `beginTurn` before its row exists). Running →
   `registry.enqueue(session.id)` (belt-and-braces nudge for a turn that hasn't
   written its supervisor placeholder yet) → `queued`.
3. `endedOnAnInterrupt(db, id)` (= `turnsForSession(id).at(-1)?.status === "interrupted"`)
   → `recorded`. KNOWN WINDOW (accepted): a note landing while the interrupted turn
   is still winding down takes the `queued` path and drains once — closing it is
   `turn/queue.ts`'s call, not this module's.
4. No starter wired (`deps.start ?? ctx.startTurn` absent) → `recorded` — an unwired
   seam degrades to "read next turn", never a lost note.
5. Call the starter. Async rejection → `reportError` only. **Synchronous throw** =
   a turn claimed the session between check and call: `registry.enqueue` + report +
   `queued` (the note is persisted; the running turn's drain finds it).

### Wake-rule behaviors the tests pin
- Idle spawner + finishing detached child → `started`, exactly one fresh turn; the
  note text reaches the provider payload of the woken turn (the only reason to wake).
- Burst of N notes on a BUSY session → all `queued`, drain into exactly ONE turn.
- Burst on an IDLE session → `["started","queued","queued"]`: the first claims the
  registry synchronously inside `beginTurn`, so same-tick siblings see busy.
- Global invariant watched across every test: no session ever has two supervisor
  `message.started` open before `turn.finished` — port the `watchTurns` harness idea.
- Interrupted session → `recorded`, message still written, zero turns started.
- Missing session → `{ message: null, wake: "dropped" }`, no throw.
- Background job exit (`[background] bg_1 "name" finished (exit 3…`) wakes an idle
  session exactly once — same rule, same function. (Silent clean exits don't post.)
- Orphan recovery: note posted to the SPAWNER's session, text contains the child's
  session id (so the user can open the branch), `wake === "recorded"`, no turn started.
- Cap-refused launch: in-band error only — no session created, no note owed.

### Caps behaviors the tests pin
- 12 sequential-child launches under one `allSettled`: first 8 fulfil with their own
  child's result, last 4 reject `SpawnCapError` (429), `spawnedInTurn == 8`,
  `running() == 0` after.
- 12 launches in ONE synchronous burst, children pending: first 4 fulfil, 8 reject;
  refused launches **never ran the launch closure at all**; `running(tree) == 4`,
  `spawnedInTurn == 4`; slots return as children finish.
- Refusals free nothing and charge nothing; per-turn budget is 8 LAUNCHES, not 8
  attempts — after refusals + releases, 4 more still fit, the 9th hits per-turn.
- Tree budget: root + child + grandchild (3 sessions, 3 turns) share ONE budget of 4;
  the 5th is refused from anywhere in the tree, including a session that launched
  nothing. A different root = independent budget; releasing one tree's slot never
  touches another's.
- `treeRootOf`: fork with an originId is its OWN tree; subagent with dangling origin
  is its own tree; a 10-deep chain resolves to the top.
- Triple `release()` frees one slot; the sibling lease keeps its.
- A launch closure that THROWS: slot released, but the per-turn budget **IS charged**
  (`spawnedInTurn == 1`) — the model's bad call, not a cap.
- Bus backstop: an unrelated session's `turn.finished` must not release; the bound
  child's does, whatever the status. The spawning turn's own `turn.finished` clears
  its per-turn tally to 0. `attachBus` returns a working unsubscribe (test asserts
  `bus.size == 0` after detach).
- Backstop + result path both firing for one child free exactly one slot (idempotence).
- Nesting: depth 0 both modes OK; depth 1 blocking OK; depth 1 detached →
  `AgentError(400)` (NOT `SpawnCapError`), message matches
  `/spawn\(\) is not available inside a subagent/` and names `agent(task, {name})`;
  takes no slot and no per-turn budget.
- Exempt (workflow) launches: 20 reservations leave the ledger untouched; the exempt
  lease is still bindable/releasable/idempotent.
- Naming: see §2 exact rules; `cleanSubagentName(42)` throws `AgentError`;
  `taskStubTitle` word-boundary example: 67-char sentence →
  `"Review every request handler in the…"`.
- Depth: `TurnCtx.depth` answers "may this turn spawn detached?"; `subagentDepth`
  answers "how deep am I?". Both exist because `depth` is 1 for ANY subagent however
  nested. `depth == 1` for a child (asserted), `model` flows into child ctx while the
  child session row keeps `model: null`, `mcpGrant` array flows through.

---

## 5. Dependencies

Imports (production code):
- `../errors.ts` — `AgentError`, `SpawnCapError`.
- `../schema/events.ts` — `BoughEvent`, `TurnFinishedData` (types only).
- `../schema/parts.ts` — `Message`, `Part`, `Session`, `Turn` (types only).
- `../types.ts` — `AppCtx`, `Bus`, `Db`, `Effort`, `TurnCtx` (types only).
- `../turn/runner.ts` — `beginTurn`, `interruptTurn`, `TurnDeps`, `TurnOutcome` (subagent.ts).
- `../turn/queue.ts` — `TurnRegistry`, `turns` singleton (notes.ts).
- `../turn/state.ts` — `OrphanedTurn` type (notes.ts).
- notes.ts → subagent.ts (`buildResult`, `SubagentResult`). caps.ts imports neither
  sibling (structural `LeasedLaunch` keeps it counting-only).
- **Never imports `server/`** — `NoteStarter` and the collapsed-kind predicate are
  restated structurally; preserve this direction in the crate graph.

Imported by:
- `hostfn/delegate.ts` — `cappedLaunch`, `SpawnCaps` type, launch exports; owns
  modes, `join()`, the claimed-check before `deliver`.
- `server/main.ts` — `spawnCaps` singleton (bus attach at boot), `createNoteDeliverer`,
  `noteOrphanedSubagents`, `postSystemNote`.
- `workflow/control.ts` — `cappedLaunch` (with `exempt`), `postSystemNote`,
  `launchSubagent`, `LaunchDeps`.
- `server/comments.ts`, `server/changes.ts`, `schedules.ts` — `postSystemNote`
  (+ `buildResult`/`SubagentResult` in schedules).
- `mcp/manager.ts` (grant semantics), `tui/lines.ts` (parses the note format),
  `history/fork.ts` (cites the structural-restatement pattern).

---

## 6. External deps → Rust equivalents

| TS/Bun API | Where | Rust |
|---|---|---|
| `crypto.randomUUID()` | session/message ids | `uuid::Uuid::new_v4()` |
| `setTimeout`/`clearTimeout` (wall clock) | launch timeout | `tokio::time::timeout` or a `tokio::select!` over `sleep` + the turn future; must record `timedOut` distinctly, then call `interrupt_turn` |
| `process.env["BOUGH_SUBAGENT_TIMEOUT_MS"]` | default timeout | `std::env::var` parsed `f64/u64`, finite & > 0 guard |
| `Promise` (`then`, `finally`, `.then(done, done)`) | result pipeline, lease release | `async fn` + `tokio::spawn`; release-on-both-paths = a guard type whose `Drop` releases, or an explicit `join` arm |
| `Map`/`Set` in `SpawnCaps` | ledger | `HashMap`/`HashSet` under `parking_lot::Mutex` (whole `reserve` under one lock to keep check+take atomic) |
| JS single-thread atomicity | `reserve` | does NOT come free — the Mutex above is the port of "synchronous from first read to last write" |
| `Date.now` / injected `now` | timestamps | `fn() -> i64` closure or a `Clock` trait; keep injectable |
| Regexes (`[\x00-\x1f\x7f]`, `\s+`) | name/title cleaning | plain `char` filters — no regex crate needed; beware `slice(0,40)` is UTF-16 code units in JS, use `char` boundaries in Rust |
| `console.error` | quiet paths (`indexQuietly`, default `reportError`) | `tracing::error!` |
| `structuredClone` | tests only | `serde_json::Value` snapshots |
| Bus (`bus.subscribe/publish`) | backstop, announcements | the port's event bus (likely `tokio::sync::broadcast`); `attachBus` = a spawned listener task returning an abort handle; replace-previous-attachment semantics kept |

No Bun-specific APIs (no `Bun.*`, no FFI) in this subsystem. SQLite access is entirely
behind the `Db` trait.

---

## 7. Suggested Rust layout

```
crates/bough-agents/
  src/lib.rs
  src/caps.rs      // SpawnCaps, SpawnLease, treeRootOf, assertMayDelegate,
                   // reserve_spawn, under_lease, capped_launch, exempt lease
  src/subagent.rs  // launch_subagent, build_result, naming, depth
  src/notes.rs     // post_system_note, wake rule, formatters, orphan recovery
```

- **`SpawnCaps`**: `struct SpawnCaps { inner: Mutex<CapsState>, per_turn: u32, concurrent: u32 }`.
  `reserve()` locks once, checks both, takes both or errors. `SpawnLease` = a struct
  holding `Arc<SpawnCaps>` (or `Weak`) + `AtomicBool released` + `Mutex<Option<String>> bound`;
  give it `release(&self)` (idempotent via `swap`) AND implement `Drop` calling
  `release` as extra insurance — but keep the explicit release + bus backstop, since
  the TS contract is behavioral, not RAII. `exempt_lease()` = same type with a no-op
  ledger handle (an enum variant `Lease::Exempt{..}` beats a trait object).
- **Errors**: `enum AgentsError { Agent { status: u16, msg: String }, SpawnCap { msg: String } }`
  with `SpawnCap` always mapping to HTTP 429; keep the message strings byte-for-byte
  (tests and the model's retry behavior key on them).
- **Traits (async boundaries)**: `Db` (sync, rusqlite behind a mutex — every call
  here is sync in TS too), `Bus` (publish sync, subscribe → broadcast receiver),
  `BeginTurn` (async: returns `(Message, JoinHandle/BoxFuture<TurnOutcome>)`),
  `NoteStarter` (`Fn(&AppCtx, &Session, &Message) -> BoxFuture<Result<()>>` — the
  sync-throw-vs-async-reject distinction in `wakeFor` must be preserved: model it as
  the starter returning `Result<Future>` so "claimed between check and call" is the
  `Err` arm), `ChangedFiles` seam (`Fn(&Session) -> BoxFuture<Vec<String>>`, errors
  swallowed).
- **`launch_subagent`** is async only where the child turn is; everything up to
  `begin()` is sync DB writes + bus publishes and must stay ordered. Return a
  `SubagentLaunch { handle fields, result: JoinHandle<SubagentResult> }`; the timeout
  is a spawned `select!`.
- **`post_system_note`** returns `NoteDelivery`, never `Err` — signature
  `fn(...) -> NoteDelivery`, swallow internally.
- Keep `notes → subagent` and `notes → turn-queue` crate/module edges; caps depends
  on neither (structural `LeasedLaunch` → a tiny trait
  `trait LeasedLaunch { fn session_id(&self) -> &str; fn result(&self) -> ... }` or a
  plain struct both sides use).
- Pin the test suite behaviors in §4 as Rust tests; the `watchTurns` two-turns
  invariant watcher is worth porting as a shared test util.

## 8. v1 scope cut

**Core (cannot cut — the agent loop and TUI depend on them):**
- `launch_subagent` + `build_result` + the four-status matrix (the blocking `agent()`
  host fn is a daily-driver primitive; the report/isolation invariants are the feature).
- `post_system_note` + the wake rule (background-shell exits also go through it —
  bash-in-background is core UX).
- `format_subagent_note` verbatim (the TUI parses it).
- Depth cap + task/name validation (cheap, and without the depth cap a self-delegating
  model recurses unbounded).

**Can simplify in v1:**
- `SpawnCaps` bus backstop + per-turn map GC: keep `reserve`/`release` and both caps
  (small and the tests are written), but the `attachBus` backstop can land with the
  bus wiring wave; until then a dropped lease leaks a slot only for detached spawns
  that error between reserve and bind — acceptable for a first boot.
- `noteOrphanedSubagent(s)`: stub until boot-recovery (`turn/state.ts` port) exists —
  it is dead code without `recoverOrphanedTurns`.
- Workflow exemption (`exempt`): keep the flag (one enum arm), but nothing exercises
  it until the workflow engine ports.
- `createJobNotifier`: trivial closure — port with jobs, not before.
- `changedFiles` seam: return `[]` ("not reported") until `hostfn/files.ts` ports;
  the note format already covers this truthfully.
- FTS `indexQuietly`: no-op until the search module ports (failure is already
  spec'd as silent).
