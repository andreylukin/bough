# Port spec: `src/turn/` — the turn runner, queue, replay, state machine

Source files (all under `src/turn/`):
- `runner.ts` (1170 lines) — the turn loop: rounds, tool execution, nudges, ending rules
- `queue.ts` (298 lines) — TurnRegistry (one turn per session), interrupt cascade, derived queue, retry ring
- `replay.ts` (298 lines) — stored parts → provider messages (pure)
- `state.ts` (223 lines) — persisted turn state machine + boot recovery

This is the heart of the agent loop. Every behavior below is pinned by tests in the
sibling `*.test.ts` files; edge cases mined from them are called out inline.

---

## 1. Purpose & invariants

### runner.ts (verbatim invariant comment)

> THE INVARIANT THIS HOLDS: **a turn always ends, always ends visibly, and always
> ends exactly once.** Three separate failures hide behind that one sentence:
>
> 1. **A turn never ends implicitly.** The model calls `stop` after its final text,
>    in the same response. A response that just trails off is not an ending — it is
>    a model that forgot — so it gets nudged, with a bounded count so a
>    stop-incapable model cannot loop the API forever. The nudges and the `stop`
>    call itself are loop control, never content: they live only in this turn's
>    in-memory exchange and are never persisted, so the thread and every future
>    replay stay clean.
> 2. **Every turn must produce user-visible text.** A turn of nothing but tool
>    calls shows the user a stack of collapsed cards and no answer. Narration
>    counts for nothing: `saidSomething()` asks only about text *after the last
>    tool call*, and a turn about to end mute is asked once for a closing report,
>    then forced into a text-only round (`toolChoice: "none"`).
>    **Scoped by kind:** a `mind` session's wakeups are addressed to nobody, so
>    for `kind: mind` the report nudge and the forced text round are skipped and
>    a mute ending is a legal ending (specs/mind.md §3). Every other kind keeps
>    the full guarantee; invariants 1 and 3 are unscoped and hold everywhere.
> 3. **The pending message is closed on every path.** Success, failure, interrupt,
>    a crash in the loop — `pending` goes false and `message.finished` fires.

Also from the runner header, load-bearing for the port:
- **No acceptance gate.** `run_steps`'s `done` flag is the model's own statement;
  it is recorded with the call and acted on by nobody.
- **Provider-blindness.** Nothing in this file knows which provider it talks to;
  everything goes through `LlmClient`. "If a provider name ever appears below, it
  has leaked, and it will leak everywhere next."
- **Reasoning echo rule.** Across turns, reasoning is dropped (replay.ts). *Within*
  one turn the block goes back verbatim, `meta` and all, because a provider that
  signs thinking rejects a tool call whose thinking was altered. The in-turn echo
  comes from `LlmResult.content` in memory; the cross-turn drop is about what
  replay reads out of the database. Nothing signed is ever stored... incorrectly —
  reasoning is persisted WITH `meta` and the signing `model` so the next turn can
  replay it to the same model.

### queue.ts (verbatim)

> THE INVARIANT: **a session runs at most one turn at a time, and no user input is
> ever lost to that rule** (spec §5).

Three consequences:
- **Interrupt is not a flag the loop checks between rounds** — it must reach into a
  running program and into detached children. The registry holds the live
  `AbortController` per session plus cascade hooks. A normal turn ending does NOT
  fire hooks; only an explicit interrupt cascades.
- **A message posted mid-turn queues, it does not race.** The queue is *derived
  from the database* (`hasUnansweredInput`), not from an in-memory flag — a flag
  would be lost across a restart, stranding the message forever. The explicit
  `enqueue` is only a nudge.
- **A round that fails is retried, not executed.** Specifically a tool call whose
  input was cut off mid-stream: re-stream immediately; executing it would run the
  wrong program against the user's checkout. Provider outages wait; retries bounded.

### replay.ts (verbatim)

> **1. Reasoning replays only to the model that signed it.** A thinking block
> returns EXACTLY as received or not at all. `meta` goes back untouched, and the
> text is never reconstructed into a block on its own — an unsigned imitation of
> thinking is both wrong and billable. The gate is the model, and nothing else.
> Dropping reasoning is NOT the conservative default it looks like — removing
> thinking blocks can itself provoke ordering and signature errors; this module
> drops only what it cannot vouch for: a part with no `meta`, or one signed by a
> different model.
>
> **2. `ask` parts replay as plain text and can never re-block.** A settled hold is
> a fact about what the user said, not a live question. It becomes
> `[ask] <question>\n→ <outcome>` in the user-side message, AFTER the tool results
> — text jammed in front of a tool_result is a provider 400.
>
> - **A `tool_use` with no matching `tool_result` gets a synthetic one** saying
>   `(interrupted)` — every provider rejects an open pair.
> - **A lost attachment replays as placeholder text, never as a failure.**

Purity: the image loader is injected; `messageToLlm` reads nothing and calls no clock.

### state.ts (verbatim)

> THE INVARIANT: **a session is never busy forever.** `busySessionIds()` reads
> `turns WHERE status = 'running'`, so a `running` row is what blocks a session. If
> the process dies mid-turn that row survives, and the session is wedged. Recovery
> at boot is the only thing that can observe this.
>
> `step` is not telemetry — it is the evidence a restart reads.
>
> **Orphan-and-surface, not resume.** A checkpoint is deliberately not enough to
> re-enter the loop from: re-running a program because a checkpoint says it started
> would duplicate every side effect. Surfacing the interruption is the honest answer.

---

## 2. Public API

### runner.ts

| Export | Signature | Semantics |
|---|---|---|
| `RUN_STEPS` | `= "run_steps"` | Tool name: run one JS program in the workspace. |
| `STOP` | `= "stop"` | Tool name: end the turn (loop control, never persisted). |
| `TOOLS` | `LlmToolDef[]` | Exactly the two tools above. **Byte-stable across rounds and sessions** (prompt-cache contract — tool defs precede the system prompt in cache order; one varying byte splits the cache for every session). Tested: `deepEqual(calls[0].tools, TOOLS)`. |
| `RunStepsInput` | zod: `{ code: string, done?: boolean }` | Boundary validation; a numeric `code` must never reach `runProgram`. |
| `MAX_TOKENS` | `32_000` | Output reservation each round makes; context meter measures against it. Sized to the largest output a round plausibly needs, not the largest a provider permits — a reservation costs usable context everywhere, and per-minute quota on providers that bill it (Cerebras). |
| `DEFAULT_MODEL` | `"claude-opus-4-8"` | Used when neither ctx nor session pins one. |
| `MAX_STOP_NUDGES` | `3` | Re-prompts before the harness stops waiting for `stop`. |
| `baseHostFns(ctx: TurnCtx): HostFns` | | Build the always-wired shell+file host fns for one turn; lazily initializes shared trails on the ctx (`exits`, `touched`, `record`, `reads`) so every construction path shares one array. |
| `defaultProgramRunner(ctx, host?): ProgramRunner` | | Production runner: fresh worker per round, host fns bridged, interrupt wired. Reads the ctx trails **by index from a per-round snapshot** (`slice(from)`) and appends result-log notes (exit notes, dir-tag hints, project-rule notes). |
| `withExitNotes(result, exits): ProgramResult` | | Append `[exit code N] <cmd ≤80 chars, one line>` for each non-zero exit the program did not print itself (`said.includes("[exit code N")` check). |
| `BASE_HOST_FNS` | `HostFnName[]` | `bash, sh, bashBg, bashOutput, bashWait, bashKill, view, patch, write` — default `granted` for the prompt's capability gating. |
| `ProgramRun` | `{ code, callId, signal, onLog }` | One `run_steps` execution as the runner asks for it; `callId` attributes streamed lines to the right card. |
| `ProgramRunner` | `(run: ProgramRun) => Promise<ProgramResult>` | Injected seam so tests never spawn a worker. |
| `TurnDeps` | interface | All injection points: `registry?`, `program?` (fixed runner, wins), `programFor?` (ctx→runner; two fields because both are 1-arg fns), `assemble?`, `granted?`, `notes?`, `now?`, `maxRoundRetries?`, `outageDelayMs?`, `maxTokens?`, `survivingJobs?(sessionId) → string[]`, `startNext?(ctx, sessionId)`, `reportError?(error, sessionId)`. |
| `TurnOutcome` | `{ turnId, messageId, status: "done"\|"error"\|"interrupted", error?, usage }` | What an awaiting caller gets. |
| `beginTurn(ctx, sessionId, deps?) → { message, done: Promise<TurnOutcome> }` | | Start a turn. Message created + announced **synchronously** (client reconciling by id must see it even if the turn finishes before the POST returns); promise separate because the HTTP path 202s and discards it. |
| `createTurnStarter(deps?) → (ctx, session, message) => void` | | The `TurnStarter` the server reads off the ctx. Busy session ⇒ `registry.enqueue`, else `startDetached`. |
| `interruptTurn(sessionId, registry?) → boolean` | | Delegates to `registry.interrupt`. |
| `programOutput(result: ProgramResult): {output, isError, interrupted?}` | | Program result as the model sees it. Success + empty logs ⇒ `"(the program ran and printed nothing — console.log what you need to see)"`. Failure: partial output leads, then blank line, then error (`body\n\nerror`). |
| `usableContextLimit(model, maxTokens?) → number \| null` | | Catalog window minus reservation; `null` for unknown models (an unknown window must not fail turns that would work). |
| `friendlyTurnError(err, model): string` | | Provider failures in plain language; see §4. |

Private but load-bearing: `STOP_NUDGE` / `REPORT_NUDGE` texts (both begin
`[harness]` — tests grep for that and for `/still open/`), `TRAILING_STOP_SENTINEL
= /(?:\s*<stop\s*\/>)+\s*$/i` (end-anchored so prose mentioning the token in a
code span is untouched; stripped from stored text and honored as a stop),
`STOPPED_NOTE = "⏹ Stopped."` (`⏹` not `⚠︎`: the user asked for this),
`InterruptedError`, `unknownToolMessage`, `executeTool`, `runRound`, `foldUsage`,
`stoppedNote`, `indexQuietly`, `startDetached`, `drive`.

### queue.ts

| Export | Semantics |
|---|---|
| `class TurnRegistry` | `#running: Map<sessionId, AbortController>`, `#queued: Set<sessionId>`, `#hooks: Map<sessionId, Set<fn>>`. Methods: `isRunning`, `runningSessions` (getter, prompt's running-subagent note reads it), `begin` (throws `"a turn is already running for session X"` when busy — must throw BEFORE the placeholder message exists), `end(sessionId, controller)` (**identity-checked**: a late `end` from a superseded turn must not unregister its replacement), `interrupt` (aborts controller if any, then fires a **snapshot** of hooks — a hook that unregisters itself must not mutate the set mid-walk; a throwing hook is swallowed and does not stop the cascade; returns `false` only when there was neither a controller nor hooks; hooks fire even when the session is idle — a detached child outlives its spawner's turn), `onInterrupt(sessionId, hook) → unregister-thunk` (removes the set when it empties), `enqueue`, `drain` (take-and-clear), `clearQueued`. |
| `turns` | The process-wide production registry instance. A class (not module globals) so tests get isolation. |
| `hasUnansweredInput(db, sessionId): boolean` | Walk the session's OWN messages (never the inherited thread — an ancestor's trailing user message was answered on its own branch; treating it as unanswered would make every fresh fork start a turn nobody asked for) newest→oldest: `supervisor` ⇒ false, `user` or `system` ⇒ true, empty ⇒ false. A `system` note owes a turn exactly like a user message — that is how a finished background child wakes its spawner. Terminates because the drained turn always appends its own supervisor message. |
| `shouldDrain(db, sessionId, registry): boolean` | `registry.drain(sessionId) || hasUnansweredInput(...)`. The nudge is taken either way so a caller that declines does not leave it armed. |
| `MAX_ROUND_RETRIES` | `2`. |
| `OUTAGE_DELAY_MS` | `60_000` — the client's own backoff already spent ~30s before failures reach here. |
| `isTruncatedToolCall(err)` | `err instanceof LlmError && /truncated mid-call/i.test(err.message)`. |
| `isAbort(err)` | `errName(err)` is `"AbortError"` or `"APIUserAbortError"`. |
| `RetryDecision` | `{ retry, delayMs, reason }` — reason is one short line shown to the user as-is. |
| `classifyRoundFailure(err, attempt, opts?)` | `attempt` 1-based. No retry when: abort, `attempt > maxRetries`, or neither truncated nor `isRetryable(err)`. Retry with `delayMs: 0` for truncation ("a lost frame is not an outage — re-stream now"), else `outageDelayMs`. Truncation reason: `"the model's tool call was cut off mid-stream — re-running the round rather than executing a truncated program"`. |
| `shortReason(err, max=120)` | One line, whitespace-collapsed, `…`-clipped — goes straight into an event payload. |
| `abortableDelay(ms, signal?)` | Sleep that an interrupt cuts short; rejects with `DOMException("interrupted while waiting to retry", "AbortError")`. `ms <= 0` + already-aborted signal still rejects; `ms <= 0` unaborted resolves immediately. Removes its abort listener on normal completion. |

### replay.ts

| Export | Semantics |
|---|---|
| `ImageLoader` | `(part: ImagePart) => { data: string /*base64*/, mediaType: string } \| null` — injected for purity. |
| `attachmentPath(part)` | Relative paths resolve under `~/.bough/attachments`; absolute taken as-is (stored by this server, not request input). |
| `readAttachment` | Production loader: `readFileSync` + base64; every failure mode is `null`. |
| `lostAttachmentText(part)` | `[image: <name> — the attachment is no longer on disk, so it cannot be shown this time. It was <size> bytes. Ask for it again if you need to see it.]` |
| `ReplayOptions` | `{ loadImage?, model? }` — `model` omitted ⇒ NO reasoning replays at all (right for UI/export/test callers). |
| `stringifyOutput(output: unknown): string` | string passthrough; `undefined` → `""`; `JSON.stringify` with `?? String(output)`; cyclic values caught → `String(output)` (must never throw). |
| `messageToLlm(m, opts?) → LlmMessage[]` | One stored message → 0, 1 or 2 provider messages. See §4. |
| `ThreadOptions` | `ReplayOptions & { exclude?: string }` — the pending supervisor message being written. |
| `buildThread(messages, opts?) → LlmMessage[]` | Takes the already-ordered list (ordering is the DB's contract: ancestors root→parent, then own, each by `(created_at, rowid)`); skips `exclude`; concats `messageToLlm`. |
| `stripReasoning(messages: LlmMessage[]): void` | In-place: drop every reasoning block from assistant messages; delete a message left empty (content-less message is itself a 400). Used when a provider rejects a round or across a mid-turn model swap. **Note: exported here but the runner does not currently call it — the LLM client layer does.** |

### state.ts

| Export | Semantics |
|---|---|
| `FinalTurnStatus` | `Exclude<TurnStatus, "running">` = `"done" \| "error" \| "interrupted" \| "orphaned"`. |
| `INITIAL_STEP` | `"start"`. |
| `ORPHAN_NOTE` | `"⚠︎ Interrupted: the server restarted before this turn finished. Anything it had already done (files written, commands run) still stands — check the changes, then continue."` — says the SERVER restarted, not that the turn failed; a user told only "failed" will redo work that stands. |
| `ORPHAN_ERROR` | `"the server restarted while this turn was running"`. |
| `startTurn(db, sessionId, messageId, now?)` | Create turn row: uuid, `status: "running"`, `step: INITIAL_STEP`, `createdAt = updatedAt = now()`, `error: null`. |
| `checkpoint(db, turnId, step, usage?)` | `db.updateTurn(turnId, { step, usage? })`. Usage **REPLACES** (runner carries the running total; accumulating here would double-count). DB bumps `updated_at` from its own clock. |
| `finishTurn(db, turnId, status, opts?)` | `error` is written on every path (`opts.error ?? null`) so a re-driven turn does not keep a stale message. |
| `OrphanedTurn` | `{ turnId, sessionId, messageId, step, closedMessage }`. |
| `RecoverOptions` | `{ onOrphan?, onHookError? }` — `onOrphan` exists so a stranded subagent's parent can be told, distinguishably; a throwing hook is isolated (one unnotifiable parent must not abandon remaining orphans). |
| `recoverOrphanedTurns(db, bus, opts?) → OrphanedTurn[]` | For each `turnsByStatus("running")` row: (1) `finishTurn(..., "orphaned", {error: ORPHAN_ERROR})` **first** — until this lands the session is still busy and every later step can fail without re-wedging it; (2) if the message is still `pending`, append `ORPHAN_NOTE` text part, close it, publish `message.part` + `message.finished`; (3) publish `turn.finished` with `status: "orphaned"` **even when the message was already closed** — that event is what a client keys "is this session busy" off; (4) call `onOrphan` in try/catch. Idempotent: second call finds nothing. Call once at server start, **before the listener binds**. |

---

## 3. Data structures

### DB tables touched (via `Db` trait, never SQL directly)

- **messages**: `createMessage`, `getMessage`, `updateMessage(id, parts, pending)`,
  `messagesFor(sessionId)` (own only), `threadFor(sessionId)` (ancestors
  root→parent then own, `(created_at, rowid)` order), `indexMessage` (FTS).
- **turns**: `createTurn`, `updateTurn(id, {status?, step?, error?, usage?})`,
  `getTurn`, `turnForMessage`, `turnsByStatus("running")`, `turnsForSession`,
  `busySessionIds()` (= sessions with a `running` turn row).
- **sessions**: `getSession`, `getSessionRuntime(id).workspace`,
  `addSessionUsage(id, usage, at)`, `setSessionOutcome(id, ok)`.

### Core types (exact field names — these are wire/DB shapes)

```
Part (discriminated on "type"):
  { type: "text", text }
  { type: "reasoning", text, meta?, model? }        // meta = opaque provider payload; model = who signed it
  { type: "tool_call", id, name, input }             // input: unknown
  { type: "tool_result", callId, output, isError, interrupted? }  // output: unknown
  { type: "image", path, mediaType, name, size }
  { type: "ask", id, question, options?, status: "answered"|"declined"|"interrupted", answer? }
  { type: "workflow", id, name, description, rerunOf? }

Message: { id, sessionId, role: "user"|"supervisor"|"system", parts: Part[], pending: bool, createdAt: number }

Turn:   { id, sessionId, messageId, status: "running"|"done"|"error"|"interrupted"|"orphaned",
          step: string, createdAt, updatedAt, error: string|null, usage? }

Usage:  { inputTokens, outputTokens, reasoningTokens?, cacheReadTokens?, cacheWriteTokens?, costUsd? }
```

### LLM boundary (from `types.ts`; owned by the llm subsystem but the runner's whole vocabulary)

```
LlmBlock:        {type:"text",text} | {type:"reasoning",text,meta?} | {type:"tool_use",id,name,input}
LlmContentBlock: LlmBlock | {type:"tool_result",toolUseId,content:string,isError} | {type:"image",data,mediaType,name}
LlmMessage:      { role: "user"|"assistant", content: LlmContentBlock[] }
LlmToolDef:      { name, description, inputSchema }
LlmParams:       { model, system?, systemVolatile?, maxTokens, messages, tools, toolChoice?: "none", effort? }
LlmResult:       { content: LlmBlock[], stopReason: string, usage?: Usage }
LlmClient:       run(params, onText: (delta)=>void, signal?) -> LlmResult
```

Note the asymmetry: persisted parts use `callId`/`output`, wire blocks use
`toolUseId`/`content` — a naive port that unifies them corrupts the DB or the wire.

### ProgramResult (from `harness/protocol.ts`)

`{ ok: bool, logs: string[], error?: string, interrupted?: bool }`.

### Bus events published (exact `type` strings and data shapes)

- `message.started` — data: the full `Message` placeholder.
- `message.delta` — `{ messageId, delta }` (streamed text).
- `message.part` — `{ messageId, part }` (each appended Part).
- `message.retry` — `{ messageId, attempt, reason }` (also emitted by the LLM
  client's `onRetry` with reason `"<shortReason> — retry N/M"`).
- `tool.log` — `{ messageId, callId, line }` (streamed program output lines).
- `session.updated` — the refreshed `Session` (after each round's usage fold).
- `message.finished` — `{ messageId }` (exactly once per turn — tested).
- `turn.finished` — `{ turnId, sessionId, status, error? }` (`error` key omitted
  entirely when absent, not null — `deepEqual` in tests pins the exact shape).

### Checkpoint step strings

`"start"` → `"round:N"` (1-based, after each round persists) → `"tool:<name>"`
(after each tool result) → final `"done"` (success) / `"ended"` (failure path).

---

## 4. Behaviors & edge cases

### The full turn lifecycle (drive loop)

1. `registry.begin(sessionId)` — throws if busy, **before** the placeholder
   message exists (an announced-then-abandoned message would sit `pending` forever).
2. Create supervisor `Message` (uuid, `parts: []`, `pending: true`), persist,
   publish `message.started`. Return `{message, done}` synchronously.
3. Resolve config: `model = session.model ?? ctx.model ?? DEFAULT_MODEL` —
   **session pin first** (reading ctx first makes `setSessionModel` a no-op on
   installs that set `BOUGH_MODEL`); `effort` same order; `workspace =
   db.getSessionRuntime(id).workspace ?? process.cwd()`; ensure scratch dir.
4. `startTurn` → `running` row.
5. Build `TurnCtx` (spread of AppCtx + sessionId/turnId/messageId/workspace/model/
   effort/signal; `depth = 1` for `subagent`/`workflow_agent` kinds, else 0).
6. Resolve program runner: `deps.program ?? (deps.programFor ?? defaultProgramRunner)(turnCtx)`.
7. Build LLM client: `ctx.llm ?? clientFor(model, {trace, retry.onRetry → message.retry})`.
   The injected `ctx.llm` (tests) bypasses tracing/retry wiring.
8. Assemble prompt once per turn. Notes order (tested): `workspaceNote(workspace)`
   first, `scratchNote(scratch)` second, then tag-history note (nullable, session-
   frozen for cache stability), then project-rules note (re-read per TURN so
   editing AGENTS.md takes effect next message, but drained onto the round result,
   not the prompt), then `deps.notes`. `granted = deps.granted ?? BASE_HOST_FNS`,
   `kind = session.kind ?? "root"`. If tracing, write the manifest (section shas).
9. Build the thread ONCE: `buildThread(db.threadFor(sessionId), {exclude:
   messageId, model})`. A turn's history does not change under it; rebuilding per
   round would re-read every attachment per round.
10. Round loop (`for round = 0;;`):
    - If `signal.aborted` → throw `InterruptedError`.
    - **Context overflow check BEFORE the request** (sending it anyway would spend
      tokens to be told so in provider dialect): if `usableContextLimit` non-null
      and `contextTokens > limit`, throw `ContextOverflowError` whose message names
      the model, the numbers, and the move: "...Compact or fork this session to
      continue — nothing was summarized automatically." (Never auto-compact.)
      `contextTokens` = last round's `inputTokens + cacheReadTokens + cacheWriteTokens`
      — a gauge, not a total.
    - `runRound` (see retry ring below) with params; `toolChoice: "none"` only
      when `forceText`; `effort` only when set; text deltas → `message.delta`.
    - Fold usage into the turn total; `db.addSessionUsage`; re-read session and
      publish `session.updated`.
    - Walk `result.content`, persisting parts and building the in-memory
      `assistant` echo (they diverge in exactly two places):
      - `text`: strip trailing `<stop/>` sentinel(s) (sets `stopRequested`);
        append + echo only if non-empty after stripping.
      - `reasoning`: persist as Part `{text, meta, model}` **if** text non-blank OR
        `meta !== undefined` (a signed block with no displayable text is redacted
        thinking — it goes back whole or not at all); ALWAYS echo into `assistant`.
      - `tool_use` named `stop`: set `stopRequested`, **never persisted, never echoed**.
      - other `tool_use`: persist Part `{type:"tool_call", id, name, input}`, echo.
    - Push `{role:"assistant", content}` only if non-empty. `checkpoint("round:N")`.
    - If `forceText`: **break** — the forced round had tools forbidden, so
      whatever it said is the ending (even if it said nothing).
    - Execute each non-stop `tool_use` **sequentially, in order**:
      - Re-check `signal.aborted` before EACH call (stop before the side effect).
      - `executeTool` — **never throws**: unknown tool ⇒ error result with
        `unknownToolMessage`; invalid input ⇒ error result listing zod issues +
        `"It takes {code: string, done?: boolean}."`; otherwise run the program and
        map via `programOutput`.
      - Persist Part `{type:"tool_result", callId, output, isError,
        interrupted?}` (the `interrupted` key only present when true); push wire
        block `{type:"tool_result", toolUseId, content, isError}`;
        `checkpoint("tool:<name>")`.
    - Ending decision, with tools this round:
      - `stopRequested && !saidSomething()`: first offense ⇒ append `REPORT_NUDGE`
        as a text block **inside the tool_result user message** (a model answers an
        inline nudge far more reliably than a standalone one), `reportNudges++`,
        continue. Second ⇒ push results, set `forceText`, continue.
      - else push results; `stopRequested` ⇒ break; else continue.
    - Ending decision, no tools this round:
      - `stopRequested && saidSomething()` ⇒ break.
      - `stopRequested`, mute, first offense ⇒ standalone user message with
        `REPORT_NUDGE`, continue.
      - `stopRequested`, mute, second ⇒ if the trailing assistant message is
        reasoning-only, **pop it** (ending a prompt on a thinking-only assistant
        message is itself invalid), set `forceText`, continue.
      - No stop, no tools (trailed off): if `nudges >= MAX_STOP_NUDGES` break
        (**status "done"**, not error — the cap ends the turn, it does not fail
        it); else push `STOP_NUDGE` user message (in memory only), continue.
11. Success path: `finalized = true`; `db.updateMessage(messageId, parts, false)`;
    `indexQuietly`; `finishTurn("done", {usage, step:"done"})`; publish
    `message.finished` then `turn.finished`; for `subagent`/`workflow_agent` kinds
    `db.setSessionOutcome(sessionId, true)` (records whether the TURN errored,
    nothing about work quality). Return outcome.
12. Catch path: `finalized = true`. `interrupted` = InterruptedError || signal
    aborted || errName in {APIUserAbortError, AbortError}. Append closing note:
    interrupted ⇒ `stoppedNote` (`"⏹ Stopped."` + `"bg_1, bg_2 still running —
    they survive the interrupt."` when `survivingJobs` names any; singular "it
    survives"; absent seam ⇒ say nothing rather than claim there were none);
    else `"⚠︎ Turn failed: " + friendlyTurnError(err, model)`. Close message,
    index, `finishTurn(status, {usage, error: error ?? null, step: "ended"})`.
    Publish `message.part` (the note), `message.finished`, `turn.finished` (with
    error only for real errors — an interrupt has `error: undefined` and the turn
    row's error is null: "an interrupt is not an error"). Raw error goes to
    `reportError` **only when not interrupted**, exactly once ("the UI must never
    know more than the server log does"). setSessionOutcome(false) for delegated
    kinds. **Returns the outcome — does not rethrow.**
13. `finally` (in `beginTurn`): `registry.end(sessionId, controller)` (identity-
    checked), then `if shouldDrain(...)` start next turn via `deps.startNext ??
    startDetached` — **after** the release, never before (`begin` would throw on a
    session this turn had not let go of). A throwing `next` is caught and logged.
    Exactly ONE drain regardless of how many messages queued (tested), because the
    drained turn's thread contains all of them.

### `saidSomething()` — the exact predicate

`parts.findLastIndex(p => p.type === "tool_call")`, then any `text` part after
that index with `text.trim() !== ""`. NOT `parts.some(isText)` — that asked "was
there ever any text", which mid-turn narration satisfies, producing turns that end
on a raw tool dump with their last word being "Let me implement the changes:".

### The round retry ring (`runRound`)

Infinite `for attempt = 1;;` around `llm.run`. On throw: `classifyRoundFailure`;
if `!decision.retry || signal.aborted` rethrow; else publish `message.retry`
(a retried round re-streams from the top; a client holding partial text must drop
it) and `abortableDelay(decision.delayMs, signal)`. Exists above the client's own
internal retries for two failures that layer cannot fix: truncated tool call
(re-stream now) and an outage outliving ~30s of client backoff (wait 60s).
Exhaustion ⇒ the error propagates ⇒ turn error. Tested: 4 consecutive truncations
⇒ `llm.calls.length === MAX_ROUND_RETRIES + 1 === 3`... **careful**: the test
scripts 4 throws but asserts 3 calls — `attempt > maxRetries` stops after attempt 3.

### Replay mapping (`messageToLlm`) — exact rules

- `user`/`system` role → ONE `user` message: text parts (empty-string text parts
  skipped), image parts via loader (null ⇒ `lostAttachmentText`). No blocks ⇒ no
  message at all (providers reject empty messages). System notes replay user-side:
  input *to* the model, never words it said.
- `supervisor` role → assistant message (text/signed-reasoning/tool_use) then, if
  results or asks exist, a user message `[...results, ...asks]` — results LEAD,
  reversing this is a provider 400.
  - reasoning: emit iff `meta !== undefined && opts.model !== undefined &&
    part.model === opts.model`; block carries `{text, meta}` verbatim.
  - tool_call: track id in `requested`; tool_result: track in `resolved`,
    stringify output.
  - ask → `askText`: `[ask] <question>\n→ the user answered: <answer ?? "">` /
    `"the user declined to answer"` / `"the turn was interrupted before an answer"`.
  - image on supervisor: skipped (reaches the model as a system note elsewhere).
  - workflow: skipped (display only; the `[workflow done]` system note is the
    record — echoing the launch line would read as two runs).
  - After the walk: every requested id not resolved gets a synthetic
    `{tool_result, content: "(interrupted — this call never returned a result)",
    isError: true}` **in call order, appended after real results**.
  - Reasoning-only message ⇒ `[]` (vanishes, not an empty turn).

### Things a naive port WILL get wrong (mined from tests)

1. **`stop` and nudges must never be persisted.** Tests grep the stored parts JSON
   for `[harness]` and for a `tool_call` named `stop` and require absence.
2. **In-turn echo vs cross-turn replay** are different mechanisms with opposite
   defaults; the acceptance test drives both against a transcript the runner
   itself wrote, plus a third turn on a different model asserting the signed block
   is dropped AND its text is not smuggled as prose.
3. **`registry.end` identity check** — a stale end from a superseded turn must not
   free the session.
4. **The interrupt transcript shape**: `text, tool_call, tool_result, text` with
   `interrupted: true` (NOT conflated with `isError` — "you stopped it" and "it
   failed" are different facts), partial output preserved, closing note exactly
   `"⏹ Stopped."`, turn row `status: "interrupted"`, `error: null`, NO further
   LLM round ("stop means stop"), session freed.
5. **Two rapid messages**: second persists but does not start; drain fires exactly
   once, synchronously with the first turn's release; turn 2's thread contains
   both messages in order; turn 1's never saw the second.
6. **Truncated call**: the program that eventually runs must be the re-streamed
   one; `f.programs.length === 1`; retry announced with `attempt: 1` and the
   two-clause reason; transcript shows only the real round.
7. **`finalized` flag**: a late `append` (an `ask` settling as the turn dies) must
   not flip a finished message back to pending.
8. **Overflow check fires before round 2 is sent** (`llm.calls.length === 1`),
   message names the model and "Compact or fork".
9. **`crypto.randomUUID()`** for message and turn ids.
10. **`friendlyTurnError` contract**: names the env var per provider
    (`ANTHROPIC_API_KEY`/`OPENAI_API_KEY`/`OPENROUTER_API_KEY`) because there is no
    keys panel; distinguishes missing key (`Could not resolve authentication
    method|apiKey or authToken|API_KEY is not set`) from rejected key (`invalid
    x-api-key|authentication_error|Incorrect API key`); special-cases
    `CLOUDFLARE_ACCOUNT_ID is not set`; folds HTTP bodies matching
    `:\s*(\d{3})\s+(...)$` — tool-call-formatting 400s get "a repaired retry
    usually clears it", other ≥400 get `"<Provider> error <status>: <shortReason>"`;
    everything else passes through raw.
11. **`withExitNotes` dedup** is `said.includes("[exit code " + code)` — a program
    that printed the failure is left alone ("saying it twice is its own noise").
12. **Boot recovery ordering**: row → message → events → hook; `turn.finished`
    emitted even for already-closed messages; note APPENDED not substituted;
    recovery never invents a tool_result on the message (replay's synthetic pair
    closing handles that).
13. **`beginTurn` claim-before-create ordering** and **drain-after-release**.
14. **Sequential tool execution** with per-call abort check; `currentCallId`
    captured for `tool.log` attribution.
15. **TurnCtx shared trails** (`exits`, `reads`, `touched`, `record`): initialized
    on the ctx (`??=`), never closure-local — host fns are built from the ctx in
    more than one place (`baseHostFns` and `delegationDeps`), and a closure-local
    array shipped green tests while doing nothing live. Per-round deltas read by
    index snapshot (`slice(from)`).

---

## 5. Dependencies

### Imports (module → what for)

- `../errors.ts` — `ContextOverflowError`, `LlmError`.
- `../llm/client.ts` — `clientFor`, `errName`, `providerFor`, `API_KEY_ENV`,
  `isRetryable`.
- `../llm/pricing.ts` — `contextWindowFor`.
- `../llm/trace.ts` — `traceLabel`, `writeManifest` (BOUGH_TRACE_DIR debugging).
- `../scratch.ts` — `ensureScratchDir`.
- `../hostfn/files.ts`, `../hostfn/shell.ts` — the bridged host functions.
- `../harness/vm.ts` — `runProgram` (worker execution); `../harness/protocol.ts`
  — `ProgramResult`, `HOST_FN_NAMES`, `HostFnName`.
- `../prompt/assemble.ts` — `assemblePrompt`, `workspaceNote`, `scratchNote`;
  `../prompt/project.ts` — AGENTS.md discovery + report queue.
- `../history/echo.ts`, `../history/record.ts`, `../history/stats.ts` —
  tag-history memory (command recorder, dir tag hints, tags note).
- `../paths.ts` — `boughHome`, `attachmentsDir`.
- `../schema/parts.ts` — Message/Part/Session/Turn/Usage.
- `../types.ts` — AppCtx/TurnCtx/Db/Bus/HostFns/Llm*.
- `zod` — RunStepsInput.
- Node: `node:path` (dirname, isAbsolute, resolve), `node:fs` (readFileSync),
  `crypto.randomUUID`, `AbortController`/`AbortSignal`, `DOMException`, `setTimeout`.

### Imported by

- `server/sessions.ts` — `createTurnStarter`, `interruptTurn`, `busySessionIds`
  gating (checks then 202s; the registry is the authority and repeats the guard).
- Server boot — `recoverOrphanedTurns` before the listener binds.
- Delegation (`hostfn` agent/spawn paths) — `beginTurn` with custom `TurnDeps`
  (`granted`, `notes`, `programFor` via `delegationDeps`), `TurnRegistry.onInterrupt`
  for detached children, `turns` registry.
- Prompt assembly — `turns.runningSessions` for the running-subagent note.

---

## 6. External deps → Rust equivalents

| TS/Bun | Rust |
|---|---|
| `zod` (RunStepsInput) | `serde` + `serde_json`; validate `code: String, done: Option<bool>` with `#[serde(deny_unknown_fields)]`; hand-roll the error text listing paths (tests pin `"invalid input for run_steps"` prefix + `"It takes {code: string, done?: boolean}."`). |
| `crypto.randomUUID()` | `uuid` crate, `Uuid::new_v4()`. |
| `AbortController`/`AbortSignal` | `tokio_util::sync::CancellationToken` (child tokens give the cascade for free; `is_cancelled` = `signal.aborted`; `cancelled()` future replaces event listeners). |
| `setTimeout` / `abortableDelay` | `tokio::select! { _ = tokio::time::sleep(d) => Ok(()), _ = token.cancelled() => Err(Interrupted) }`. |
| `Promise` / `.finally()` drain | `tokio::spawn` + a guard struct whose `Drop`/explicit epilogue runs registry release + drain. |
| `structuredClone` (tests) | `Clone` derives on param types. |
| `readFileSync` + base64 | `std::fs::read` + `base64::engine::general_purpose::STANDARD.encode`. |
| `DOMException("...", "AbortError")` | A dedicated `TurnError::Interrupted` variant; drop the string-typed error-name dance (`errName`) entirely for typed enums. |
| JS regexes (`TRAILING_STOP_SENTINEL`, friendly-error matchers) | `regex` crate; `(?i)(?:\s*<stop\s*/>)+\s*$` etc. |
| `Date.now()` injected clock | `Fn() -> i64` boxed clock or a `Clock` trait; keep injectable for tests. |
| Bun test + fake LlmClient | `#[tokio::test]` with a scripted `LlmClient` trait impl; in-memory rusqlite DB. |
| `console.error` reporting | `tracing::error!`, still behind the `report_error` injection point. |

---

## 7. Suggested Rust layout

```
crates/turn/
  src/
    lib.rs        // re-exports
    runner.rs     // begin_turn, drive, run_round, execute_tool, program_output,
                  // friendly_turn_error, usable_context_limit, TOOLS constants
    queue.rs      // TurnRegistry, has_unanswered_input, should_drain,
                  // classify_round_failure, abortable_delay, short_reason
    replay.rs     // message_to_llm, build_thread, strip_reasoning,
                  // stringify_output, lost_attachment_text, ImageLoader
    state.rs      // start_turn, checkpoint, finish_turn, recover_orphaned_turns
```

**Traits / seams:**
- `LlmClient`: `async fn run(&self, params: LlmParams, on_text: &mut dyn FnMut(&str), cancel: &CancellationToken) -> Result<LlmResult, LlmError>` — object-safe (`Arc<dyn LlmClient>`), the runner stays provider-blind.
- `ProgramRunner`: `async fn run(&self, run: ProgramRun) -> ProgramResult` — boxed
  closure or trait object; tests inject a fake, production wraps the harness VM.
  `on_log` as `mpsc::UnboundedSender<String>` or callback.
- `ImageLoader`: `Fn(&ImagePart) -> Option<LoadedImage>` — keeps replay pure.
- `Db` and `Bus` come from the shared core crate as traits (`Arc<dyn Db>`,
  `Arc<dyn Bus>`); replay takes `&[Message]`, never the Db.
- `TurnDeps` → a builder-style struct of `Option<...>` fields mirroring the TS one.

**Concurrency model:**
- `TurnRegistry`: `Mutex<HashMap<String, RegistryEntry>>` where the entry holds a
  `CancellationToken` + hook set (`Vec<(u64, Box<dyn Fn() + Send + Sync>)>` with id-based
  unregister). All methods sync and lock-scoped; `begin` returns the token.
  One process-wide instance via `Arc`, injected — never a static in tests.
- `begin_turn` spawns the drive future on tokio and returns `(Message,
  JoinHandle<TurnOutcome>)` (or a oneshot receiver). The message row and
  `message.started` publish MUST happen before the function returns (synchronous
  claim + create, matching the TS "synchronous up to the first await" contract —
  do the claim/create inline, spawn only the loop).
- The registry-release + drain epilogue must run on every completion including
  panics: wrap `drive` in `AssertUnwindSafe(...).catch_unwind()` or run it inside
  the spawned task with the epilogue after `await`, converting panics to the error
  path (the TS catch-all maps any throw to a finished-with-error turn).
- The drive loop itself is a single sequential async fn — no internal parallelism;
  tool calls run one at a time by design.

**Error handling:** one `enum TurnFailure { Interrupted, ContextOverflow{...},
Llm(LlmError), ... }`; `drive` never propagates — it converts every failure into a
closed message + `TurnOutcome` exactly as TS does.

---

## 8. v1 scope cut

**Must ship for a working loop (do NOT cut):** the whole ending state machine
(stop / sentinel / nudges / report nudge / forceText), registry begin/end/interrupt,
queue drain via `hasUnansweredInput`, replay with all four invariants (signed
reasoning gate, ask-as-text, synthetic tool_result closing, empty-message
elision), state checkpoints + boot recovery, the truncation/outage retry ring,
interrupt transcript shape, context-overflow refusal, `programOutput` /
`unknownToolMessage`. These ARE the subsystem; every one is pinned by a test and
by a real field failure documented in the source.

**Safe to stub or defer in v1:**
- **Tracing** (`traceLabel`/`writeManifest`) — debugging aid; stub to no-op.
- **Tag-history memory hooks** (`withDirTagHintNotes`, `tagsNoteFor`, command
  recorder/echo, `reads`/`touched` trails) — additive result-log notes; stub to
  identity. Keep the `exits`/`withExitNotes` note though — it is cheap and it
  prevented a documented model-confabulation bug.
- **Project rules (AGENTS.md) notes** (`findProjectRules`/`noteProjectRules`/
  `drainProjectRuleNotes`) — defer to the prompt subsystem's schedule; identity-stub
  `withProjectRuleNotes`.
- **FTS indexing** (`indexQuietly`) — already fail-soft; no-op stub.
- **`session.updated` refresh publish per round** — nice-to-have for live cost
  chips; can lag.
- **`survivingJobs` note** — the seam's absent-case already says nothing; wire later.
- **`friendlyTurnError`** — start with the missing/rejected-key branches + raw
  passthrough; the Cloudflare/HTTP-body folds can follow.
- **`stripReasoning`** — port the function (trivial) but its caller lives in the
  llm crate; not needed for the core loop.
- **Effort plumbing** — pass-through field; fine to carry but not exercise.
- **Attachment/image replay** — if the v1 TUI cannot attach images, `ImageLoader`
  can be a stub returning `None` (the lost-attachment text path already handles it
  gracefully); keep the placeholder text exact.

**Do not port:** anything marked deleted in the TS headers (acceptance/CHECK gate,
`lsp.*`, canvas) — the source explicitly says the machinery is gone on purpose.
