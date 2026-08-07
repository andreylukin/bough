# Port spec: `src/workflow/` — the workflow engine

Scope: `run.ts`, `journal.ts`, `meta.ts`, `control.ts`, `relaunch.ts`, `report.ts`,
`saved.ts`, `schema.ts` plus their inseparable satellites: the script-side worker
`src/harness/wf_worker.ts`, the workflow half of `src/harness/protocol.ts`, the REST
handlers `src/server/workflows.ts`, and the DB tables `workflows` / `workflow_agents`.

A workflow is a **detached JS orchestration script** that fans out subagents past the
per-turn caps. The script runs in a worker; the host journals every `agent()` call and
makes reruns cheap by replaying an unchanged prefix. The turn that started it is free to
end; progress flows over bus events; completion posts a system note.

---

## 1. Purpose & invariants (quoted verbatim from module headers)

**run.ts** — the engine (worker bridge, journal writes, semaphore, pause gate, replay):

> THE INVARIANT THIS HOLDS: **every `agent()` call is journaled by key before it runs,
> and a relaunch replays the longest UNCHANGED PREFIX of those calls instead of paying
> for it.** `key` is `hash(prompt + label + phase + model + schema)` — everything that
> decides what the subagent will be asked.

Sub-invariants stated in the same header, all load-bearing:

- "**Replay stops at the first changed call and never resumes**… A key covers a call's
  PROMPT, not the filesystem that prompt runs against, and workflow agents all share one
  checkout… A miss costs money; a stale hit is a wrong answer presented as a fresh one,
  so the engine buys the cheap failure. `replayPlan`… is therefore indexed by call
  POSITION, not a key→result map: position is part of the identity of a call."
- "**Position comes from the script's STRUCTURE, never from arrival order**" — the
  worker sends a structural coordinate (`"0.1.1.0"`, dot-joined slot indexes) with every
  `agent()` call; a bare call falls back to the enclosing frame's counter. "The journal
  key is `<pos>|<contentHash>`, so a call that MOVED and a call that was EDITED are
  different facts."
- "The journal row is written BEFORE the semaphore is acquired, so the run view can show
  a queued agent, and `startedAt` is reset when the call actually starts."
- "**Pause gates ADMISSION, not issuance, and a stopped run leaves nothing
  non-terminal.**" (both checked in `admit()`, after a semaphore slot is taken, with the
  row still `queued`).
- "Only successful calls replay. A failed call re-runs live… and, under the prefix rule,
  so does everything after it."

**journal.ts** — script mirror on disk:

> THE INVARIANT THIS HOLDS: **the mirror is a working copy that may differ from the
> row, and the difference is what the next relaunch consumes.**

(Existing mirrors are never overwritten; `resolveRerunScript` prefers the mirror; the DB
row stays canonical. Path confinement: the RELATIVE name is confined, never the joined
path, because `join()` swallows a leading slash.)

**meta.ts** — `export const meta = {…}` read without running the script:

> THE INVARIANT THIS HOLDS: **`meta` is a pure literal, located by a scan that cannot
> be derailed by the script's own text, and evaluated by a parser that cannot execute
> anything.**

**control.ts** — lifecycle verbs + production runner:

> THE INVARIANT THIS HOLDS: **a control verb is not a status write.** `stop` is only
> honest if the fan-out actually stops — the worker dies AND every subagent turn the run
> started is interrupted… `pause` is the mirror image: it must NOT reach a running
> agent, because a paused run is one that stops *admitting* work, not one that discards
> work already paid for.

**relaunch.ts** — stop-edit-relaunch + its accounting:

> THE INVARIANT THIS HOLDS: **replay never crosses the first changed call, and what it
> did cross is reported.** Both halves, because either one alone is a defect.

**report.ts** — replay counts, cost, large-run flag:

> THE INVARIANT THIS HOLDS: **every journaled call is counted exactly once, in exactly
> one bucket, and the buckets sum to the total.** `replayed + ranLive + pending ===
> total`, always, for a run in any state.

(Also: "THE LARGE-RUN FLAG IS ADVICE, AND SO IS THE SIZE GUIDELINE. Neither pauses,
throttles or refuses anything." Everything in report.ts is a fold over rows the engine
wrote — no second replay computation.)

**saved.ts** — named workflows:

> THE INVARIANT THIS HOLDS: **a name can only ever address a file inside
> `~/.bough/workflows/saved/`.**

**schema.ts** — structured agent output:

> THE INVARIANT THIS HOLDS: **a schema mismatch retries; an exhausted retry fails the
> call.** `agent()` either resolves with an object that validates against the supplied
> schema or it throws — it never resolves with junk.

**harness/wf_worker.ts** — the script-side worker:

> THE INVARIANT THIS HOLDS: **a workflow script is deterministic, and its combinators
> have exactly the concurrency semantics the spec states.**

Plus: "every `agent()` call carries a STRUCTURAL COORDINATE, computed from the script's
shape rather than from the order calls happen to reach the host." NOTE the header's
honesty clause: under Bun the worker **inherits the server's capabilities** — the
five-name world is "a CONTRACT, not a cage"; the traps are about replay correctness,
not confinement. A Rust port embedding a JS engine can restore real confinement.

---

## 2. Public API

### run.ts (engine)

| Export | Signature | Semantics |
|---|---|---|
| `workflowConcurrency()` | `() -> number` | `BOUGH_WORKFLOW_CONCURRENCY` if finite > 0, else default. |
| `defaultWorkflowConcurrency()` | `() -> number` | `clamp(1, 16, cores - 2)`; falls back to **4** when core count is unusable. |
| `workflowTimeoutMs()` | `() -> number` | `BOUGH_WORKFLOW_TIMEOUT_MS`, else **60 min**. Liveness backstop, not a budget. |
| `MAX_AGENTS_PER_RUN` | `= 1000` | Lifetime cap per run; runaway-loop backstop (was 200 — that was wrong: fired on legit audits). |
| `WORKFLOW_PROGRAM_PARAMS` | `["agent","phase","log","args","parallel","pipeline","console"]` | Names a script compiles against (= `WORKFLOW_SCRIPT_PARAMS` + worker-built names). Duplicated in wf_worker.ts by design (cannot import a worker entry point into the host); drift pinned by a test that probes a real worker for every name. |
| `workflowBody(script)` | `string -> string` | Regex-demotes `export const meta =` → `const meta =` (keeps line numbers; leaves a harmless binding). |
| `checkWorkflowSyntax(body)` | `string -> string \| null` | Compile-check via `new AsyncFunction(...params, body)`. `null` = parses. Enriches two SyntaxErrors: shadowing a bound param ("`agent` is bound in every workflow's scope…rename your variable") and an unterminated quoted string (via `harness/vm.ts`'s `unterminatedString`, names line + quote kind). Non-SyntaxError rethrows. |
| `AgentCall` | `{prompt, label, phase?, model?, schema?}` | One `agent()` call. `label` never empty (defaulted from prompt first line, clipped to 40). |
| `AgentRunner` | `(call, signal: AbortSignal, onSpawned: (sessionId) => void) -> Promise<string>` | Runs one call to completion. Resolves with the report VERBATIM; MUST reject on failure. The injection seam that makes the engine drivable offline. |
| `WorkflowCtx` | `{db, bus, runner, notify?, now?}` | Engine context. `notify(sessionId, text)` delivers the finished-run note; absent = nobody woken. `now` = injected clock. |
| `WorkflowMetaInput` | `{name, description, phases?}` | The validated meta literal. |
| `StartOpts` | `{sessionId, script, meta?, args?, resumeOf?, concurrency?, timeoutMs?, effectiveModel?}` | `resumeOf` = journal-replay source; absent meta on a resume inherits the source's. `effectiveModel` = resolved model for calls that name none — folded into keys. |
| `callKey(call, effectiveModel?)` | `-> string` (16 hex chars) | Content hash. See §4 for exact algorithm. |
| `CallPos` | `= string` | Dot-joined slot indexes, `"0.1.1.0"`. |
| `comparePos(a, b)` | `-> -1\|0\|1` | Component-wise NUMERIC compare; missing components read as −1 so a prefix sorts before its extensions ("0.10" > "0.9"). |
| `journalKey(pos, contentKey)` | `-> "<pos>\|<hash>"` | Stored key; `\|` cannot occur in either half. Recoverable on purpose (edited vs moved diagnosis). |
| `splitJournalKey(key)` | `-> {pos: CallPos\|null, content}` | Inverse. No separator = pre-coordinate row, `pos = null`. |
| `distinctLabel(prompt, taken)` | `-> string` | First prompt line (clip 40) no sibling has claimed; if all collide, `"<base(36)> #N"`. Display only — `callKey` hashes the deterministic first-line label. |
| `ReplayStep` | `{pos, content, key, idx, result: string\|null, prompt}` | One source-journal call. `result` non-null only for answers. |
| `ReplayPlan` | `{steps: ReplayStep[] (sorted by comparePos), byPos: Map, byContent: Map<hash, CallPos[]>}` | Source journal, addressable by coordinate. |
| `emptyReplayPlan()` | | First run's plan. |
| `replayPlan(db, sourceRunId)` | | Reads `listWorkflowAgents(sourceRunId)`. Answer = status `done`/`cached` AND `result !== null`. Pre-coordinate rows get `String(idx)` as pos. |
| `replayablePrefix(plan)` | `-> number` | Length of the leading (structural-order) run of answered steps. |
| `DivergenceKind` | `"changed" \| "moved" \| "added" \| "unanswered"` | Four reasons a call can't replay; four different fixes. |
| `Divergence` | `{pos, kind, sourcePos?, reason}` | `reason` = the one sentence every surface prints. |
| `classifyDivergence(plan, pos, content)` | | See §4 — `moved` is tested BEFORE `changed`. |
| `ReplayAudit` | `{diverged, divergedAt, forced}` | One fold used by the note, the replay endpoint and views. |
| `replayAudit(plan, rows)` | | Empty plan → `{null, null, 0}` (a first run has nothing to diverge FROM). Diverged = STRUCTURALLY first non-cached row the plan couldn't serve; `forced` = live rows whose pos+content still matched an answer. |
| `isWorkflowLive(id)` | `-> bool` | Present in the process-wide live registry. |
| `startWorkflow(ctx, opts)` | `-> Promise<WorkflowRun>` | The engine. Returns the run row IMMEDIATELY (detached). See §4 for the full lifecycle. |
| `stopWorkflow(ctx, id)` | `-> WorkflowRun` | Kill worker + abort run controller + open gate + sweep rows + status `stopped`. On a non-live run: `running`/`paused` → `orphaned`; otherwise returns the row as-is (idempotent). |
| `pauseWorkflow(ctx, id)` | `-> WorkflowRun` | 409 if not live in this process. Sets `paused = true`, status `paused`. |
| `resumeWorkflow(ctx, id)` | `-> WorkflowRun` | 409 if not live. Opens gate FIFO, status `running`. |
| `RerunOpts` | `{script?, args?, meta?, effectiveModel?}` | |
| `rerunWorkflow(ctx, id, opts)` | | 404 unknown; 409 if still live. Resolves script via `resolveRerunScript`, then `startWorkflow` with `resumeOf: id`. A NEW run — never edits the old. |
| `recoverOrphanedWorkflows(db, bus?, now?)` | `-> string[]` | Boot: every `unfinishedWorkflows()` not live → sweep `running`/`queued` rows to `stopped`, run → `orphaned` with error "the server restarted before this workflow finished". Restart is SURFACED, not resumed. |
| `workflowSummary(db, run)` | `-> object` | Run trimmed for list/verb responses; **omits `script`**. Shape in §3. |

### journal.ts

| Export | Semantics |
|---|---|
| `mirrorPath(runId)` | `~/.bough/workflows/<id>.js`; confines the RELATIVE name `<id>.js` first (throws PathError on escape). |
| `mirrorScript(runId, script) -> Promise<bool>` | Best-effort write (mkdir -p + write); false on any error, never throws. |
| `readMirror(runId) -> Promise<string\|null>` | null when unreadable/absent. |
| `syncScriptMirrors(db, {limit=50}) -> Promise<string[]>` | Boot: recreate MISSING mirrors for the `limit` newest runs; existing files never read/compared/rewritten. Unconfinable ids skipped. Returns ids written. |
| `ScriptSource` | `"explicit" \| "mirror" \| "stored"` |
| `resolveRerunScript(run, override?) -> {script, from}` | Explicit (non-blank after trim) wins → mirror (non-blank) → stored row. A blank override is not an override. |

### meta.ts

| Export | Semantics |
|---|---|
| `WorkflowMeta` (zod) | `{name: 1..80, description: 1..500, phases?: [{title, detail?}]}` — `.strict()` on both objects (unknown keys REJECTED, e.g. `phasez` typo). |
| `MetaSpan` | `{start, literalStart, end, literal}` — offsets of the declaration and `{…}` text. |
| `scanBalanced(src, start) -> end` | Balanced-brace scan from the opening `{`; skips `'`/`"` strings (raw newline = unterminated = error), template bodies with nested `${…}` code frames, `//` and `/* */` comments. Throws on never-closed. |
| `metaSpan(script) -> MetaSpan\|null` | Locates `export const meta = {` with the same skipping scan (a commented-out/quoted declaration is not the real one). Sticky regex `DECL` tested only where scanner knows it's code and the previous char is not an identifier char. |
| `metaLiteral(script) -> string\|null` | `metaSpan()?.literal`. |
| `parseLiteral(src, start, end) -> unknown` | Recursive-descent over object/array/string/number/boolean/null ONLY. Rejects with `computed(...)` (variables, calls, `a+b`, `${…}` templates, spreads, shorthand props, computed keys, `undefined`, methods) or `malformed(...)` (holes `[a,,b]`, unclosed things, trailing text, depth > 16). Messages carry 1-based line numbers. `__proto__` keys set via `defineProperty` (data, never a prototype swap). Full JS string-escape decoding (`\n \t \r \b \f \v \0`, line continuation, `\xHH`, `\uHHHH`, `\u{…}`; unknown escape = the char itself). |
| `extractMeta(script) -> WorkflowMeta` | Throws `WorkflowScriptError` (400) — missing / computed / unparseable / wrong shape (per-field zod issues in the message). |
| `stripMeta(script)` | Blanks the statement char-for-char with spaces, keeps every newline → line/col of later errors still match the authored file. |
| `readWorkflowMeta(script)` | `{meta: extractMeta(), body: stripMeta()}`. |

Note: `startWorkflow` uses `workflowBody` (demote), not `stripMeta` (blank) — both keep
line numbers; `stripMeta` is for building a body with no `meta` binding at all.

### control.ts

| Export | Semantics |
|---|---|
| `WorkflowAgentHandle` | `{runId, agentId, ctrl: AbortController, restart: bool, sessionId}` — one in-flight call as control verbs see it. `ctrl` replaced per attempt. |
| `WorkflowAgentRegistry` | `claim(db, runId)` binds the starting call to the **lowest-idx unclaimed `running` row** (works because the engine flips the row to running and invokes the runner with no await between — and MUST NOT use the call key: the structured-output decorator rewrites the prompt before the runner sees it). `release(handle)` idempotent. `get(runId, agentId)`. `forRun(runId)`. |
| `workflowAgents` | Process-wide instance. |
| `WorkflowControlDeps` | Injection seams: `{registry?, agents?, launch?, child?, notify?, decorate?, now?, card?}`. |
| `WithWorkflowControl` / `workflowControlOf(ctx)` | Optional `ctx.workflowControl` (AppCtx is frozen); absent = `{}` = production defaults. |
| `createSubagentRunner(turnCtx, deps)` | The production `AgentRunner`. Launches via `cappedLaunch(turnCtx, {mode:"blocking", verb:"workflow agent()", exempt:true}, ...)` → `launchSubagent(turnCtx, prompt, {name: label, model?}, childDeps)`. Registers an abort cascade: on run-signal abort, `interruptTurn(child.sessionId)` **only if still running** (never flips a finished session). Non-ok result → `WorkflowError(409 if interrupted else 424, "workflow agent \"<label>\" <status>: <report clip 400>")`. Already-aborted signal at entry → 409 "never launched". |
| `controlledRunner(ctx, binding, inner, deps)` (private) | Outermost wrapper: claims a handle, loops attempts; per attempt makes an own `AbortController` relayed from the run signal; on catch, if `handle.restart && !runSignal.aborted` → clear `sessionId`/`error` on the row, publish, `continue` (script stays parked on the same promise); else rethrow. Releases handle in `finally`. |
| `controlWorkflowAgent(ctx, runId, agentId, "stop"\|"restart", deps)` | 404s; Conflict if row not `running` ("Rerun the workflow to re-issue a finished call") or not live in this process ("the server restarted… stop the run and rerun it"). Sets `handle.restart`, aborts `handle.ctrl`. NOTE (accepted delta): a single-agent stop lands the row as `error`, not `stopped` — only a RUN-level abort maps to `stopped` in run.ts. |
| `workflowAnchor(db, sessionId)` | Lineage anchor = latest message id, else synthetic `workflow:<sessionId>`. |
| `workflowLaunchCtx(ctx, sessionId, anchor?)` | Fabricated `TurnCtx`: `turnId: "workflow:<sid>"`, workspace from session runtime (else cwd), model = session pin ?? ctx.model ?? DEFAULT_MODEL, **inert signal** (run abort travels per call), depth 0. |
| `workflowCtxFor(ctx, sessionId, deps, anchor?)` | Builds `{workflowCtx, bind}`. Wrapper order IS the design: `createSubagentRunner` (launch) → `decorate` (structured output, from `ctx.workflowCtx` seam) → `controlledRunner` (outermost: one claim + restart loop spans schema retries). `bind(runId)` must be called the instant `startWorkflow` returns (worker cannot host-call in the same tick); `bind(null)` on start failure so the binding promise settles. |
| `workflowCtxModel(ctx, sessionId)` | session.model ?? ctx.model ?? DEFAULT_MODEL — the `effectiveModel` resolution; MUST mirror the subagent launch resolution. |
| `startWorkflowRun(ctx, {sessionId, script, args?, anchorMessageId?, concurrency?, timeoutMs?}, deps)` | Submit boundary: `extractMeta` FIRST (400 before any row/worker), then ctx build, `startWorkflow`, `bind`. |
| `rerunWorkflowRun(ctx, id, {script?, args?}, deps)` | 404; Conflict if live. Resolves script HERE (meta travels with the script — a renamed mirror renames the run), extracts meta, calls `rerunWorkflow` with same-resolution `effectiveModel`. |
| `WorkflowAgentView` | `WorkflowAgent & {tokens, toolCalls, activity: string[], live}` — activity = last 4 tool-call gists `name(firstLine(input.code ?? input) clip 48)`. |
| `workflowAgentViews(db, runId, registry)` | Rows in STRUCTURAL order (`sortByPosition`: comparePos on pos-halves, idx fallback for pre-coordinate rows, stable). |
| `workflowDetail(db, run, registry, accounting?)` | `{workflow, agents, scriptFile, live, replay, cost, warning, guideline}` — GET /workflows/:id and workflow.status body. |
| `appendWorkflowPart(ctx, sessionId, messageId, run)` | Transcript card as a message Part `{type:"workflow", id, name, description, rerunOf?}`. Idempotent on run id; preserves `pending`. Identity only — status is read live by the renderer. |
| `workflowVerb(ctx, sessionId, verb, args, deps, anchorMessageId?)` | Program-side dispatcher for `workflow.start\|rerun\|stop\|pause\|resume\|status\|list`. Zod-parses args (`{script, args?}` / `{id, script?, args?}` / `{id}`); every verb answers the SUMMARY (never the raw row). `status` adds `agentRows` + replay/cost/warning. `rerun` adds `replay: summarize(...)` (at that instant live counts are 0; `available` is the signal). Unknown verb → 400 listing the verbs. |
| `workflowConfirmEnabled()` | `BOUGH_WORKFLOW_CONFIRM != "0"` (default ON). |
| `confirmWorkflowText(meta)` | Approval card text from meta only (agent count deliberately NOT guessed). |
| `createWorkflowHostFn(turnCtx, deps, confirmGate)` | The bridged `workflow(verb, argsJson)` host fn. JSON-parses args (400 on bad JSON). Confirm gate parks on `raiseAsk` for `start`/`rerun` only (options `["run it","no"]`; only "run it"/"yes"/"y" proceed; anything else → catchable `AskDeclinedError` "NOTHING was started"; a rerun with no inline script gates on a placeholder meta). Cards are BUFFERED until `message.finished`(this message) or `turn.finished`(this session) — the turn runner rewrites message parts wholesale, so a directly-appended card is erased. Returns `JSON.stringify(value ?? null)`. |

### relaunch.ts

| Export | Semantics |
|---|---|
| `RelaunchEngine` | `{workflowCtx, bind}`. |
| `RelaunchDeps` | `{ctxFor(ctx, sessionId), effectiveModel?(ctx, sessionId)}` — injected (importing control.ts would close a cycle through server/app.ts). |
| `WithRelaunch` / `relaunchDepsOf(ctx)` | `ctx.relaunch`, and **deliberately NOT a silent default**: unwired → `WorkflowError(500, "…boot-wiring bug in server/main.ts")`. A default would run agentless and blame the script. |
| `RelaunchPreview` | `{sourceId, journaled, answers, replayablePrefix}` — a ceiling, never a promise. |
| `relaunchPreview(db, sourceId)` | Fold over `replayPlan`. |
| `RelaunchOpts` / `RelaunchResult` | `{script?, args?}` / `{run, source, script: ScriptSource, replay: RelaunchPreview}`. |
| `relaunchWorkflow(ctx, sourceId, opts, deps)` | 404; ConflictError if source live ("Pause before you stop: agents already dispatched finish and are journaled"). Resolve script → extractMeta → preview → start with `resumeOf`. `args === undefined` means keep the source's (never pass null). `bind(null)` on failure. |
| `RelaunchReport` | `{runId, sourceId, total, replayed, ranLive, pending, succeeded, failed, stopped, available (= answered steps count — NOTE: here it is ALL answers, not the prefix), divergedAt, diverged, divergedPos, forced, final, livePrompts}`. Derived entirely from rows; divergence/forced from the engine's `replayAudit` (never a second derivation). |
| `relaunchReport(db, runId)` | 404 on unknown. |
| `relaunchLine(r)` | One-line human form; "replayed NOTHING: <reason>" when `available>0 && replayed==0`. |
| `relaunchWorkflowH` | `POST /workflows/:id/relaunch` → 201 `{workflow, source, script, replay}` (receipt, not result). |
| `workflowReplayH` | `GET /workflows/:id/replay` → `{...relaunchReport, line}`. Own endpoint: answers a MONEY question, readable mid-run. |

(Both handlers are `function` declarations, not `const` — hoisting breaks the
import cycle with `server/app.ts`. Irrelevant in Rust.)

### report.ts

| Export | Semantics |
|---|---|
| `ReplaySummary` | `{runId, sourceId, replayed, ranLive, total, pending, succeeded, failed, stopped, available (= `replayablePrefix(plan)` — the PREFIX, unlike RelaunchReport.available), final, diverged?, divergedPos?, livePrompts, line}`. |
| `replaySummary(db, runId)` | 404 on unknown ("nothing replayed" and "no such run" are opposite problems). |
| `summarize(db, run)` | Same, row already in hand. |
| `replayLine(s)` | Human line. Says **"stopped at slot 0.0.0.0"** — the word "slot" is load-bearing: a 4-deep CallPos parses as an IPv4 address otherwise. |
| `AgentCost` / `PhaseCost` / `RunCost` | Per-agent `{tokens (session in+out, 0 for replay), elapsedMs (finishedAt ?? now − startedAt, min 0), replayed}`; per-phase sums (elapsed = AGENT time, not wall); run `{tokens, agentMs, wallMs, byPhase, byAgent}`. |
| `runCost(db, run, now?)` | Fold; tokens grow live while watched. |
| `SizeGuideline` | `"small"(5) \| "medium"(15) \| "large"(50) \| "unrestricted"(∞)`; `DEFAULT_GUIDELINE = "medium"`. |
| `parseGuideline` / `requireGuideline` | Case-insensitive trim; require throws BadRequest listing the values. |
| `guidelinePath()` | `~/.bough/workflows/size-guideline` (one word + newline). |
| `activeGuideline()` | File (read SYNCHRONOUSLY per call — no cache, deliberate) → `BOUGH_WORKFLOW_SIZE` → medium. |
| `setGuideline(v)` | Validates, mkdir -p, writes `"<g>\n"`. |
| `guidelineAdvice(g)` | The sentence for the script author ("advice, not a cap"). |
| `tokenWarnThreshold()` | `BOUGH_WORKFLOW_TOKEN_WARN`, else 1,000,000. |
| `projectTokens(cost)` | avg tokens of finished non-replayed calls × remaining (queued+running non-replayed) + spent; nothing finished → spent (a floor, never a guess). |
| `largeRunFlag(cost, guideline, threshold)` | `null` when unflagged; else `{flagged: true, advisory: true, guideline, target, scheduled, tokens, projectedTokens, tokenThreshold, reasons[], stop: "POST /workflows/<id>/stop"}`. |
| `RunAccounting` / `runAccounting(db, run, opts?)` | `{replay, cost, warning, guideline}` — the view-time block. |

### saved.ts

| Export | Semantics |
|---|---|
| `savedDir()` | `confine(workflowsDir(), "saved")`. |
| `normalizeName(raw)` | trim, drop ONE trailing `.js` (case-insensitive). |
| `savedPath(raw)` | Validate (nonempty; ≤64 chars; `/^[A-Za-z0-9][A-Za-z0-9._-]*$/`) with specific 400s, then `confine(savedDir(), "<name>.js")` as backstop. |
| `SavedWorkflow` / `SavedWorkflowDetail` | `{name, path, description, bytes, updatedAt}` (+ `script`). `description` = `extractMeta().description` or `""` (listing never fails on a bad meta). |
| `saveWorkflow(name, script)` | 400 on blank script; overwrite (a name is a command, not a version). |
| `saveRunAs(db, runId, name)` | 404; validates name BEFORE reading; saves **mirror ?? stored row** (the script that produced the result the user liked). |
| `listSavedWorkflows()` | Absent dir → `[]`; skips unaddressable and vanished files; sorted by name (localeCompare). |
| `readSavedWorkflow(name)` | 404 naming the name — never an empty script. |
| `deleteSavedWorkflow(name)` | `false` when nothing there (rm without force). |
| `ensureSavedDir()` | Boot: mkdir -p; returns count for the boot line; 0 on read-only. |

### schema.ts (structured output)

| Export | Semantics |
|---|---|
| `DEFAULT_ATTEMPTS = 3` / `structuredAttempts()` | `BOUGH_SCHEMA_ATTEMPTS` (≥1, floored), else 3. Attempts INCLUDE the first. |
| `MAX_ERRORS = 12` | Validation errors carried back to the model. |
| `checkOutputSchema(schema) -> string \| null` | The submit gate. Root must be `type:"object"`. Supported keywords: `type properties required additionalProperties items enum const anyOf allOf $ref $defs definitions`; annotations passed through: `title description default examples format $comment $schema $id`. REJECTED **by name with the move** (never silently stripped — deliberate divergence from the SDKs): numeric/length/regex bounds, `minItems/maxItems/uniqueItems/contains/prefixItems/oneOf/not/if/then/else/patternProperties/propertyNames/dependent*/unevaluated*`. Also rejected: recursion via `$ref` (cycle named), dangling `$ref` (must be `#/$defs/Name` or `#/definitions/Name`), `type` arrays ("use anyOf"), unconstrained subschemas (no type and no combinator/enum/const), objects without `additionalProperties: false`, objects with no properties, arrays without a single `items` schema, empty `enum`/`anyOf`/`allOf`. Seven types: object array string integer number boolean null. |
| `assertOutputSchema(schema)` | Throws `WorkflowError(400)`. |
| `validateInstance(schema, value) -> string[]` | Instance check against an accepted schema; JSON-pointer paths (`/findings/0/title`); capped at 12; handles `$ref`, `allOf`, `anyOf` ("matched none of the N allowed shapes"), `const`, `enum`, type checks (`integer` = `Number.isInteger`), required, closed-object extra-property naming, per-property and per-item recursion. |
| `extractJson(report)` | Finds the JSON value in a prose report: whole-text parse → fenced ` ```json ` blocks LAST first → balanced top-level `{…}`/`[…]` spans (string/escape-aware) LAST first. The LAST value is the conclusion; earlier ones are usually quoted examples. |
| `schemaContract(schema)` | Appended to a schema-bearing prompt (the agent has no other way to know a contract exists): "Finish your report with exactly one JSON value… A ```json fenced block is fine… every object is closed…" + pretty-printed schema. |
| `repairContract(previous, errors, attempt)` | Retry appendix: names the errors, quotes up to 800 chars of the prior report (a retry is a FRESH session with no memory). |
| `structuredRunner(inner, {attempts?})` | The decorator. No schema → pass through untouched. Schema → assert at submit; loop attempts: abort check (409 "workflow stopped"), run inner with `prompt + contract [+ repair]`, `extractJson`, `validateInstance`; success returns **`JSON.stringify(found.value)`** — CANONICAL text, because the worker parses it and the journal replays it (live and replayed must be byte-identical). No JSON → retry with "the report contained no JSON value at all". Exhausted → `WorkflowError(422)` with attempts, last errors, report head, and advice. Inner rejection (child errored/interrupted/stopped) is NOT retried — different failure. |
| `WithStructuredWorkflow` | `ctx.workflowCtx?: (base) => WorkflowCtx` — the boot seam control.ts reads as `decorate`. |
| `structuredWorkflowCtx(base, opts?)` | `{...base, runner: structuredRunner(base.runner, opts)}`. |

### harness/wf_worker.ts (script-side; a Rust port must reproduce all of this in its embedded JS runtime)

Script scope = exactly `agent, phase, log, args, parallel, pipeline, console`:

- `agent(prompt, opts?) -> Promise<string | parsed JSON>`: claims a structural slot
  **synchronously before the first await**, bridges
  `{type:"host", id, fn:"agent", args:[String(prompt), JSON.stringify(opts ?? {})], pos}`.
  With `opts.schema` set, `JSON.parse`s the returned report (throws naming the first
  200 chars if unparseable). MUST throw on subagent failure.
- `phase(title)` / `log(message)`: fire-and-forget (`void hostCall(...).catch(() => {})`)
  — return `void`, never block, swallow transport failures. `console.log/error/warn/
  info/debug` all alias `log`. Non-string log payloads are JSON.stringified.
- `parallel(thunks)`: barrier that NEVER rejects; non-array → rejected TypeError with
  the "() =>" hint. Each thunk runs via `Promise.resolve().then(...)` (sync throw lands
  in the same catch) in a child frame `[...base, slotIndex]`; a thrown/non-callable
  thunk resolves its slot to `null`; a non-function element resolves to itself.
- `pipeline(items, ...stages)`: NO barrier between stages; each item flows through all
  stages independently; a throwing stage drops THAT item to `null` and skips its
  remaining stages; result resolves in input order once every item settles. Stage
  callbacks receive `(prev, originalItem, index)`. Frames are **STAGE-MAJOR**:
  `[...base, stageIdx, itemIdx]` — structural order must imply CAUSAL order (item-major
  would let a later-dispatched cell sort before an already-announced divergence and
  replay against a tree a live agent was rewriting). Non-function stage → that item
  nulls (TypeError caught by the per-item catch).
- Frames: `{path: number[], next: 0}`; root frame counter is what bare `agent()` calls
  draw from (sequential script ⇒ positions `0, 1, 2…` = the old monotonic numbering).
  Propagated via `AsyncLocalStorage` — a plain "current frame" variable breaks the
  moment a stage callback awaits before calling `agent()`.
- Determinism traps (throw with actionable messages "pass timestamps through `args`" /
  "vary prompts by index"): `Date.now()`, argless `new Date()` (Proxy — `new Date(ms)`
  and `new Date(iso)` keep working), `Math.random()`, `performance.now()`,
  `crypto.randomUUID()`, `crypto.getRandomValues()`. Wrapped in try — frozen globals
  degrade to "rerun re-runs everything", not to a crash.
- Exit trap: `process.exit()` throws a **catchable** Error ("a script ends by returning
  its result").
- The body runs as `new AsyncFunction(...SCRIPT_PARAMS, code)(...)`; result posts
  `{type:"done", resultJson: JSON.stringify(result ?? null) ?? "null"}`; a throw posts
  `{type:"error", message: String(err.stack ?? err), logs: []}`. Unparseable `argsJson`
  → script runs with `args = null` (the script's own guards beat a dead worker).
  `{type:"abort"}` inbound → reject all pending host calls ("workflow stopped"), ack
  `{type:"aborted"}`.

### Wire protocol (harness/protocol.ts)

```
WORKFLOW_HOST_FN_NAMES = ["agent", "phase", "log"]           // only these cross the wire
WORKFLOW_SCRIPT_PARAMS = [...names, "args"]
ToWorkflowWorker   = {type:"run", code, argsJson}
                   | {type:"host_result", id, ok: bool, value: string}
                   | {type:"abort"}
FromWorkflowWorker = {type:"host", id, fn, args: unknown[], pos?: string}   // pos: agent only, `\d+(\.\d+)*`
                   | {type:"done", resultJson}
                   | {type:"error", message, logs}
                   | {type:"aborted"}
```
Host `hostCall` dispatch validates `msg.fn` against the canonical list before indexing
(the worker global is script-reachable, so `fn` is untrusted), appends `msg.pos` to the
arg list for `agent` only, replies `{type:"host_result", id, ok, value: String(v)}`;
errors reply `ok:false` with the message.

### REST surface (server/workflows.ts + relaunch.ts + app.ts routes)

```
GET  /workflows?session=<id>            {workflows: [summary]}         (newest first)
POST /workflows                         {sessionId, script, args?} → 201 run row
GET  /workflows/:id                     workflowDetail (reconnect path)
POST /workflows/:id/stop|pause|resume   run row (stop idempotent; pause/resume 409 if not live here)
POST /workflows/:id/rerun               {script?, args?} → 201 {...run, replay: ReplaySummary}
POST /workflows/:id/relaunch            {script?, args?} → 201 {workflow, source, script, replay: RelaunchPreview}
GET  /workflows/:id/replay              {...RelaunchReport, line}
POST /workflows/:id/agents/:agentId/:action   action ∈ stop|restart (validated, never defaulted)
POST /workflows/:id/save                {name} → 201 SavedWorkflow
GET  /saved-workflows                   {saved: [...]}
GET  /saved-workflows/:name             SavedWorkflowDetail
PUT  /saved-workflows/:name             {script} XOR {runId} → 201
POST /saved-workflows/:name/runs        {sessionId, args?} → 201 {...run, savedAs}  (no resumeOf — nothing replays)
GET  /workflow-settings                 {sizeGuideline, target, advice, tokenWarnThreshold, concurrency, maxAgentsPerRun, advisory: true}
PUT  /workflow-settings                 {sizeGuideline}
```
`/saved-workflows` is top-level because `/workflows/saved` would be swallowed by
`/workflows/:id`. All handlers are thin: parse → one control-layer call → json; domain
errors are HttpError subclasses rendered by the app-level catch.

---

## 3. Data structures

### DB tables (schema is FROZEN — see memory: columns go at END, ALTERs are sanctioned one at a time)

`workflows`: `id PK, session_id → sessions, name, description, script, phases (JSON
[{title, detail?}]), status, current_phase, result (JSON), error, args (JSON),
resume_of → workflows, created_at, finished_at`. Index `(session_id, created_at)`.

`workflow_agents`: `id PK, run_id → workflows, idx (call order), key, label, phase,
prompt, model, schema (JSON — present in SQL, NOT surfaced on the zod WorkflowAgent
type), status, result, error, session_id → sessions, started_at, finished_at`.
Indexes `(run_id, idx)`, `(run_id, key)`.

Statuses: run `running | paused | done | error | stopped | orphaned`; agent
`queued | running | done | error | stopped | cached`.

### Db trait surface used

`createWorkflow, getWorkflow, listWorkflows(sessionId?) (newest first),
unfinishedWorkflows, updateWorkflow(id, patch), createWorkflowAgent,
updateWorkflowAgent(id, patch), listWorkflowAgents(runId) (ORDER BY idx, rowid),
findWorkflowAgent(runId, key)` plus `getSession, getSessionRuntime, threadFor,
messagesFor, getMessage, updateMessage, sessionUsage, sessionsByOrigin`.

### Bus events

`workflow.updated {sessionId, data: WorkflowRun}` — on create, phase change, pause/
resume/stop/finish/orphan. `workflow.agent {sessionId, data: WorkflowAgent}` — on every
row transition. `workflow.log {sessionId, data: {runId, line}}` — script `log()`/
`console.*` and the engine's own "replay ends at …" announcement. Completion also posts
a system note (`notify` seam → `postSystemNote`).

### `workflowSummary` wire shape (exact field names)

```json
{"id","name","description","status","currentPhase","phases",
 "agents":{"total","done","cached","running","queued","failed"},
 "result","error","resumeOf","createdAt","finishedAt","scriptFile"}
```
(`done` counts done+cached; `failed` counts `error` rows; script omitted.)

### Completion note (assembled in `finish()`, `run.ts`)

```
[workflow <status>] "<name>" (<id>) — <ok>/<n> agents succeeded.
Replay: <replayed> replayed from run <resumeOf>, <live> ran live[, from <pos> (call <idx>) on — <reason>. | (the whole prefix matched).]
   — or, first run: "Replay: not a relaunch — this run started fresh and journalled as it went, so a rerun can replay its unchanged prefix."
done:    Result:\n<JSON clip 4000>  [+ if result empty ({} / null / undefined) and agents reported:
         "The script returned nothing, so here is what each agent reported — do NOT call workflow.status to fetch these again:" + "- <label>: <clip 600>" lines, clip 4000]
error:   Error: <clip 2000>
stopped: Stopped by the user.
```

---

## 4. Behaviors & edge cases (a naive port gets these wrong)

### callKey — exact algorithm (port bit-for-bit if old journals must replay)

`s = JSON.stringify([prompt, label, phase ?? "", model ?? effectiveModel ?? "",
canonicalJson(schema ?? null)])`, then two FNV-1a-style passes over the **UTF-16 code
units** of `s`:

```
a = 0x811c9dc5; b = 0x01000193
for each code unit c:  a = (a ^ c) * 0x01000193 (wrapping u32)
                       b = (b ^ ((c + 7) & 0xffff)) * 0x01000193 (wrapping u32)
key = hex(a).pad8 + hex(b).pad8       // zero-padding is load-bearing: without it ~12% of keys collide across the half boundary
```
`canonicalJson` sorts object keys recursively; arrays keep order. Rust hazard: iterate
`str.encode_utf16()`, not bytes/chars, and use `wrapping_mul` (= `Math.imul`). The
hashed label is the DETERMINISTIC first-line default (or the explicit label), never the
sibling-aware display label. Hashing the RESOLVED model is the fix for a real bug:
repinning a session and rerunning replayed the old model's answers.

### The prefix decision is SYNCHRONOUS

In `agent()`, `(at, callPos, content, key, cached-or-diverged)` are all computed in one
uninterrupted block before any await. Deciding after an await lets a later call's hit
land before an earlier call's miss moved the frontier. The frontier `divergedPos` is a
**coordinate, not a boolean** (dispatch order ≠ structural order under `pipeline`); a
call replays only when `comparePos(callPos, divergedPos) < 0`. Already-replayed calls
are never retracted (only possible between script-declared-concurrent calls, which had
no mutual order in the source either). The divergence is announced ONCE on
`workflow.log` ("replay ends at <pos> (call <n>, <label>): <reason> — it and everything
after it in the script run live, including calls whose own key is unchanged (agents
share one checkout)"); calls behind it say nothing more.

### classifyDivergence order — `moved` BEFORE `changed`

Ask "did the source run this exact content ANYWHERE" before "is this slot occupied":
any count-preserving reorder leaves every slot full, so slot-first reported a pure swap
as "the call at 0 was edited" — the misdiagnosis that hid the pipeline transposition
defect. Kinds: same pos+content but no answer → `unanswered` (runs live: the failure
may be what the author fixed); content found elsewhere → `moved` (sourcePos = first
occurrence); slot occupied by different content → `changed`; neither → `added`.

### agent() call lifecycle in the engine (order matters at every step)

1. Parse opts defensively (bad JSON → `{}`); empty prompt → WorkflowError 400.
2. `at = idx++`; `at >= 1000` → WorkflowError 429 (runaway backstop).
3. `callPos = pos ?? String(at)`; compute content/key; make the replay decision (above).
4. `await awaitGate()` — gate check #1, BEFORE journaling (a paused sequential run must
   not show a session-less "running" agent).
5. If aborted after the gate: throw 409 "workflow stopped — this call was never
   journaled" — **no row may be created after the stop sweep** (that was the leak; the
   pause→stop sequence spec §8 recommends is how you hit it).
6. Display label; **create the journal row** (status `cached` w/ result+finishedAt, or
   `queued`); publish. Row.model = resolved model (else the run view shows blank model
   on every ordinary call). Row.phase = call.phase ?? run.currentPhase.
7. Cache hit → return the stored result. No semaphore, no cost.
8. `admit()`: loop { aborted → false; paused → park on gate, then re-check abort
   (stop opens the gate too); acquire semaphore; if aborted/paused after acquiring →
   release and loop (slot RELEASED while parked, or resume-order ≠ arrival-order) }.
9. ONE try/catch (not nested — a nested abort-check once stepped over the row-settling
   handler and left `queued` forever): not admitted or aborted → 409 "…was queued and
   never started"; else set `running` + **reset startedAt** (elapsed excludes
   parked/paused time), run `ctx.runner(call, ctrl.signal, onSpawned)` (onSpawned
   stamps row.sessionId), settle `done`+result. Catch: settle
   `stopped`-if-run-aborted-else-`error`, **rethrow** (combinators depend on rejection).
   Finally: release iff admitted.

### finish() / stopWorkflow() wind-down ordering

delete from live registry (finish is idempotent via this) → clearTimeout →
`worker.terminate()` → `ctrl.abort()` (aborting is what interrupts subagent TURNS;
killing the worker only stops the script) → `paused = false` + drain gate (ABORT FIRST,
deliberately: everything unparked wakes to an already-aborted signal by construction,
not by microtask timing) → sweep every `running`/`queued` row to `stopped` → update
run → publish → notify. Rows can still settle after the sweep (unparked calls write
their own terminal status) but none can be created after it.

### startWorkflow sequence

session must exist (404); script non-blank (WorkflowScriptError); `workflowBody` +
`checkWorkflowSyntax` (refuse before a worker spawns); on `resumeOf`: source must exist,
`args === undefined` inherits source args, meta ??= source meta, build plan; create run
row (name ?? "workflow", description ?? "", phases ?? []); `mirrorScript` (best-effort —
read-only ~/.bough must not stop a run); publish; spawn worker; arm wall-clock timeout
(`finish("error", …, "workflow timed out after Nms")`); wire message loop; post
`{type:"run"}`; **return the row immediately**. Worker `done` → JSON.parse result
(unserializable → null) → finish("done"); `error` → finish("error", msg); `aborted` →
ignore; `onerror` → preventDefault + finish("error", "workflow worker error: …").

### Pause/resume/stop semantics (mined from tests)

- Pause holds a `parallel()` fan-out at the **semaphore** (all its calls are past the
  pre-journal gate within the first tick; a single pre-dispatch check is a no-op for
  exactly the shape workflows exist for). Rows stay `queued`.
- Pause still gates a strictly sequential script (regression-pinned).
- Running agents finish while paused and are journaled — so they replay on the next
  relaunch ("pause before you stop preserves the most work").
- Stopping a paused run (fan-out or sequential) leaves **no** `queued`/`running` row.
- stop on a finished run: idempotent, returns the row. stop on a non-live
  `running`/`paused` run (dead process): marks `orphaned`.
- pause/resume on a non-live run: WorkflowError 409 "not running in this process".

### Replay semantics (mined from tests)

- Unchanged script rerun → **zero** live calls (including the async-latency pipeline
  reproduction: unchanged 2-stage pipeline with skewed stage-1 latency replays 4/4).
- Editing call 3 of 6 → replays 1-2, runs 3-6 live INCLUDING unchanged keys; audit
  reports `forced` for those.
- A failed source call ends the prefix; its successors re-run even if answered
  (answers behind a failure were never available — `available` = prefix in
  ReplaySummary).
- Old journals: keys without `|` get `pos = String(idx)` — an old sequential journal
  replays; an old concurrent one misses and re-runs (safe direction).
- `parallel` slots keep stable positions under varying latency; nested
  parallel-inside-pipeline coordinates stay distinct.
- A first run reports `diverged: null` (an accusation with no defendant otherwise).
- A relaunch gets a new run id, never touches source rows; inherits args unless
  replaced; refuses a live source (Conflict); unknown source 404.

### Meta scanning edge cases (test-pinned)

Braces in strings/templates/`${…}`/comments don't end the literal; a commented-out or
quoted `export const meta = {` is not the declaration; unterminated literal is an
ERROR, never a short literal; trailing commas fine; `__proto__` is data; escape decoding
incl. `\u{…}`; error messages carry the line number. In `metaSpan` (unlike
`scanBalanced`) an unterminated string/comment just ends the search (`null`) — the
syntax check names it better.

### Structured output edge cases (test-pinned)

Contract appended on attempt 1; repair appendix quotes prior report; prose-only report
retries; exhausted retries throw 422 (parallel slots it `null`); inner-runner failure
NOT retried; stopped run does not start another attempt; canonical JSON return means a
`{schema}` call resolves to a PARSED object in the script, byte-identical live vs
replayed. The `controlledRunner` claim spans all schema retries (one journal row per
`agent()` call regardless of retries).

### Misc traps

- `distinctLabel` prevents a shared-preamble fan-out from rendering N identical rows.
- The transcript card MUST be buffered until turn end (wholesale part rewrites erase
  direct appends) — "a launch card survives the runner's next wholesale write" is a
  pinned test.
- Confirm gate: default ON; decline = catchable AskDeclinedError; nothing created on
  decline (gate sits before `startWorkflowRun`); only start/rerun gated ("a confirm on
  a brake teaches people to hit enter without reading").
- `workflowVerb` answers summaries — the row carries the whole script.
- Boot wiring (server/main.ts): `recoverOrphanedWorkflows`, `syncScriptMirrors`,
  `ensureSavedDir`, fill `ctx.workflowControl`, `ctx.workflowCtx`
  (= structuredWorkflowCtx), `ctx.relaunch` (= workflowCtxFor/workflowCtxModel).
- Nesting rule still applies to workflow agents (depth), width caps do not
  (`exempt: true` lease).
- Env vars: `BOUGH_WORKFLOW_CONCURRENCY`, `BOUGH_WORKFLOW_TIMEOUT_MS`,
  `BOUGH_WORKFLOW_SIZE`, `BOUGH_WORKFLOW_TOKEN_WARN`, `BOUGH_SCHEMA_ATTEMPTS`,
  `BOUGH_WORKFLOW_CONFIRM`.
- `clip()` counts UTF-16 units and appends `…` (used in labels/notes — cosmetic, but
  label clipping feeds `callKey` via the default label, so keep semantics).

---

## 5. Dependencies

**Imports** (workflow/* → rest of bough):

- `errors.ts`: `NotFoundError, WorkflowError(status, msg), WorkflowScriptError(400),
  ConflictError, BadRequestError, AskDeclinedError` — all HttpError subclasses.
- `harness/protocol.ts`: workflow message types + name lists (frozen).
- `harness/vm.ts`: `unterminatedString(body)` (shared diagnostic).
- `paths.ts`: `confine(root, relative)` (NUL check + resolve + prefix containment),
  `workflowsDir()`, `workflowScriptPath(id)`.
- `schema/parts.ts`: `WorkflowRun, WorkflowAgent, WorkflowPhase, WorkflowPart, Message,
  Part`; `schema/requests.ts`: `CreateWorkflowBody, RerunWorkflowBody`.
- `types.ts`: `Db, Bus, AppCtx, TurnCtx, HostFns, WorkflowHostFns`.
- `agents/caps.ts` (`cappedLaunch`), `agents/notes.ts` (`postSystemNote`),
  `agents/subagent.ts` (`launchSubagent, LaunchDeps`), `hostfn/ask.ts` (`raiseAsk`),
  `hostfn/delegate.ts` (`LaunchFn`), `turn/runner.ts` (`DEFAULT_MODEL, interruptTurn`),
  `turn/queue.ts` (`turns`), `server/http.ts` (`json, parseBody`).

**Imported by**: `server/app.ts` (routes), `server/workflows.ts` (handlers),
`server/main.ts` (boot wiring + seams), the host-fn dispatcher (binds
`createWorkflowHostFn` into a turn's `HostFns.workflow`), TUI (`workflow.updated` /
`workflow.agent` / `workflow.log` events, run view, `WorkflowPart` card).

Intentional non-imports (port must preserve the direction): journal.ts has no engine
dependency; relaunch.ts does not import control.ts (injected seam, loud 500 when
unwired); wf_worker.ts is never imported by the host (its traps would break
`Date.now()` server-wide) — the param list is duplicated and pinned by a probe test.

---

## 6. External deps → Rust equivalents

| TS/Bun dependency | Where | Rust replacement |
|---|---|---|
| `Worker` (Web Worker, `new Worker(url, {type:"module"})`, postMessage/onmessage/onerror/terminate) | run.ts ↔ wf_worker.ts | **The big one — workflow scripts are user-authored JS and must stay JS.** Embed a JS engine: `rquickjs` (QuickJS, small, easy native fns, per-runtime interrupt handler for termination) or `deno_core` (V8, heavier, better async). Run each script on its own thread with its own runtime; replace postMessage with `tokio::sync::mpsc` channels carrying the same message enums. `worker.terminate()` → QuickJS interrupt handler flag / V8 `terminate_execution`. |
| `AsyncLocalStorage` (frame propagation) | wf_worker.ts | Not needed if combinators are implemented as **native host functions** in the embedded runtime: pass the frame explicitly — each `parallel` thunk / `pipeline` stage invocation is wrapped in a JS closure generated host-side that carries its own frame object, and `agent` reads the frame from the closure scope (or a per-promise-chain slot via engine-level opaque data). Do NOT use a global "current frame". |
| `new AsyncFunction(...params, body)` (syntax pre-flight + script compile) | run.ts, wf_worker.ts | Compile `(async (agent, phase, log, args, parallel, pipeline, console) => { <body> })` in the embedded engine; a compile error at submit time = the pre-flight. Reproduce the shadow-declaration and unterminated-string diagnostics on top of the engine's SyntaxError message. |
| `zod` (WorkflowMeta, request bodies, arg schemas) | meta.ts, control.ts, server | `serde` + `#[serde(deny_unknown_fields)]` + manual length checks; keep the per-field error message contract (path + message joined by `; `). |
| JSON Schema validation (hand-rolled subset) | schema.ts | **Port by hand** — do not substitute the `jsonschema` crate: the subset rules (reject-by-name with advice, additionalProperties:false required, error text) are the product surface. `serde_json::Value` walking. |
| `crypto.randomUUID()` (run/agent ids) | run.ts | `uuid::Uuid::new_v4()`. |
| FNV-1a double hash / `Math.imul` / `charCodeAt` | callKey | `wrapping_mul` over `str::encode_utf16()`; `serde_json` with **preserve-order off + manual canonicalization** must reproduce `JSON.stringify` output (string escaping differences are a compatibility risk — see risks). |
| `node:fs/promises` (mkdir/readFile/writeFile/stat/readdir/rm), `readFileSync` | journal/saved/report | `tokio::fs` (+ `std::fs` for the sync guideline read). |
| `setTimeout`/`clearTimeout` (run timeout) | run.ts | `tokio::time::sleep` in a spawned task, `AbortHandle`/select. |
| `AbortController`/`AbortSignal` (run + per-attempt) | run/control | `tokio_util::sync::CancellationToken` (child tokens for the per-attempt controller relay). |
| Hand-rolled counting semaphore w/ FIFO queue + pause gate | run.ts | Do NOT use `tokio::sync::Semaphore` blindly — the engine releases a slot while parked on pause and re-acquires on resume, and FIFO wake order is asserted. A small custom `Mutex<State> + Notify` (or a fair async semaphore) reproducing `admit()`'s loop is safer. |
| Bus (`publish/subscribe`) | everywhere | `tokio::sync::broadcast` (existing bus abstraction). |
| `navigator.hardwareConcurrency` | run.ts | `std::thread::available_parallelism()` (keep `cores - 2`, clamp 1..16, fallback 4). |
| `Date.now()` | injected `now` | `now: Box<dyn Fn() -> i64>` / trait; keep ms epoch. |
| `localeCompare` (saved list sort) | saved.ts | Plain byte/`str::cmp` is acceptable (test only asserts name ordering). |
| `toLocaleString("en-US")` (token counts in flag reasons) | report.ts | Manual thousands-separator formatting. |

---

## 7. Suggested Rust layout (crate `bough-workflow`)

```
bough-workflow/
  src/
    pos.rs        CallPos newtype, compare_pos (Ord impl), journal_key/split_journal_key
    key.rs        call_key (utf16 double-FNV), canonical_json
    meta.rs       scanner (scan_balanced, meta_span) + literal parser + WorkflowMeta
                  validation; pure, fully unit-testable — port meta.test.ts wholesale
    replay.rs     ReplayStep/Plan, replay_plan, replayable_prefix, DivergenceKind,
                  classify_divergence, replay_audit  (pure over rows)
    engine.rs     start_workflow / stop / pause / resume / rerun / recover_orphaned,
                  LiveRun registry (Mutex<HashMap<RunId, LiveRun>>), admit()/gate/
                  semaphore, finish(), the agent() host-side lifecycle, note assembly,
                  workflow_summary
    worker.rs     the embedded-JS side: runtime setup, determinism + exit traps,
                  agent/phase/log native fns, parallel/pipeline combinators with
                  stage-major frames, run-message loop  (mirror of wf_worker.ts)
    runner.rs     trait AgentRunner: async fn run(&self, call, token, on_spawned) -> Result<String>
                  + the production SubagentRunner (cascade interrupt, exempt lease)
    control.rs    WorkflowAgentRegistry (claim/release/restart loop), controlled runner
                  wrapper, workflow_ctx_for wiring order, workflow_verb dispatcher,
                  confirm gate, transcript-card buffering, agent views (sort_by_position)
    structured.rs schema check/instance validate/extract_json/contracts/StructuredRunner
                  decorator (wraps any AgentRunner)
    journal_fs.rs mirror_path/mirror_script/read_mirror/sync_script_mirrors/
                  resolve_rerun_script
    relaunch.rs   preview/relaunch/RelaunchReport/relaunch_line + the two handlers
    report.rs     ReplaySummary/summarize/replay_line, RunCost, guideline, large-run flag
    saved.rs      name validation, save/list/read/delete/ensure
    http.rs       axum handlers ↔ server/workflows.ts (thin, no try/catch)
```

Traits & boundaries:

- `AgentRunner` is THE seam (async trait / `Arc<dyn AgentRunner>`); the whole engine
  must run in tests with a fake runner, no LLM — every engine test depends on this.
- Decorator order is part of the contract: `SubagentRunner` → `StructuredRunner` →
  `ControlledRunner` (outermost). Model as `Arc<dyn AgentRunner>` wrapping.
- `WorkflowCtx { db, bus, runner, notify: Option<...>, now: Option<...> }` as a struct;
  `Deps`-style option structs for the control seams.
- Async boundaries (tokio): one task per run (message loop + timeout), one task per
  admitted agent call, one thread (or LocalSet task) per JS runtime. The prefix
  decision and journal-row creation must stay on the run's message-loop task in one
  non-await section (or under one mutex) to preserve the synchronous-decision
  guarantee.
- The JS runtime does not need to be Send: dedicate a thread per run and speak to it
  over channels — this exactly mirrors the Worker architecture.

---

## 8. v1 scope cut

**Core (cannot cut — the loop is not a workflow engine without them):**
meta.rs (submit gate), engine.rs (start/finish/stop + journal writes + semaphore),
worker.rs (script execution, `agent` + `parallel` + `pipeline`, determinism traps,
structural coordinates — coordinates are NOT optional: without them the flagship
pipeline case re-bills every relaunch), replay.rs (plan + prefix + divergence),
runner.rs production runner with the stop cascade, journal_fs.rs (mirror +
resolve_rerun_script — "edit the file, rerun" is the iteration loop), rerun/relaunch
operation, `recoverOrphanedWorkflows` at boot, the completion note, REST: list/create/
get/stop/rerun, bus events.

**High (daily-driver, port right after the loop closes):** pause/resume (the admit()
gate is already in core; the verbs are small), replay/relaunch reporting (ReplaySummary
+ `GET /workflows/:id/replay` — the money-visibility surface the spec calls required),
the program-side `workflow.*` verb + confirm gate + transcript card, single-agent
stop/restart (registry + per-attempt controllers).

**Stub in v1:**
- `structured.rs`: accept `{schema}` but pass through (or reject with "not yet
  supported") — the decorator slots in later without touching the engine (schema is
  already hashed into keys as an opaque value, so journals stay valid).
- `report.rs` cost/guideline/large-run flag: return zeros / `warning: null` /
  `guideline: "medium"`; settings endpoints can 501.
- `saved.rs` + its routes: 501 or omit.
- Agent activity trail in `workflowAgentViews` (`tokens/toolCalls/activity`) → zeros.
- Confirm gate default OFF until `raiseAsk` is ported.

**Drop for v1:** nothing else — the remaining surface IS the subsystem.
