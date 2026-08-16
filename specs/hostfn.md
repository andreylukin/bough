# hostfn — port spec (TS `src/hostfn/` → Rust)

Source files: `shell.ts` (513), `jobs.ts` (948), `spill.ts` (328), `files.ts` (466), `patch.ts` (697), `ask.ts` (496), `delegate.ts` (598), `artifact.ts` (302), `schedule.ts` (368), `state.ts` (320). Tests mined: all `*.test.ts` alongside (≈4k lines, contracts pinned in §4).

The subsystem is every function a model-written program can call through the worker bridge. **The wire is string-only in both directions** (`harness/protocol.ts`): every host fn is `(String, ...) -> Result<String, Error>`; structured payloads are JSON serialized/deserialized at the boundary; the ONE exception is `view`/`patch`/`ask`, whose payload is already text. Host-fn failures reach the program as ordinary catchable exceptions (rejected promise carrying the message), never a killed worker. **Absence is the capability denial**: a verb not bridged for this turn is simply not on the host object.

Canonical name list (`HOST_FN_NAMES`, closed): `bash, sh, bashBg, bashOutput, bashWait, bashKill, view, patch, write, agent, spawn, join, adopt, workflow, ask, state, schedule, artifact` (+ `history` amended 2026-08). Method-object verbs (`HOST_FN_VERBS`): `state: get/set/list/delete`, `schedule: list/add/enable/disable/remove`, `workflow: start/rerun/stop/pause/resume/status/list`.

Module boundary rule (plan §3): **`hostfn/` never imports from `server/`**. Everything takes a `Db`, `Bus`, or `TurnCtx` (or a structural `Pick` of one); `server/*` imports from here, never the reverse. Preserve this direction in the Rust crate graph.

---

## 1. Purpose & invariants (quoted verbatim from module headers)

- **shell.ts**: "a foreground command never blocks the turn and is never killed for taking too long (plan §6.7). Past the threshold, `bash` returns '…moved to background as bg_N' and the command KEEPS RUNNING … 'it timed out, try again' is not an outcome this module is allowed to produce." Second rule: "**`sh` never throws on a non-zero exit.**" `sh` also must not auto-background (a backgrounded shell has no exit code, and `[{code,out}]` with a missing code is a contract the caller cannot branch on). Nothing here is confinement (spec §2.2) — shells run as the user, unconfined.
- **jobs.ts**: "**a long command is never lost and never blocks the turn**." Three sub-rules: (1) buffers retained per shell and readable while running; (2) retention bounded but deterministic — head+tail verbatim with an explicit omission marker, no LLM digestion; (3) "Exit is **announced**, not discovered" — `job.spawned`/`job.exited` bus events, and an unclaimed exit posts a `[background]` system note. Shells are in-memory per session, die with the server; `BackgroundJob` is deliberately not a table ("a persisted row would always be a lie after a restart").
- **spill.ts**: oversized command output goes to a file in the session scratchpad, and the turn is told where. "NO SCRATCHPAD MEANS NO SPILL, AND THEN NOTHING IS DROPPED THAT WOULD NOT HAVE BEEN" — without a scratch dir it falls back to the generous 100k/300k head/tail truncation. Pure core, injected edges: `planSpill` decides; `spill` writes.
- **files.ts**: "**`[path#]` — the empty tag — always means the exact bytes this session last saw at that path, and a patch is refused outright when there are no such bytes on record.**" view/patch/write all RECORD what they rendered/wrote, keyed by the RESOLVED absolute path. Keeping the TEXT (not just its hash) makes a stale patch *recoverable* (rebase), not merely *detectable*. No `read()`, no `edit()` — one editing idiom. Workspace is the origin for relative paths, never a boundary.
- **patch.ts**: "a patch never silently lands on text its author did not see." Rules in order: (1) **Rebase or refuse, never guess** — a silent lost update is the one forbidden outcome; (2) **Viewed coordinates** — all line numbers in the viewed version's coordinates, result assembled in ONE pass so earlier ops never shift later anchors; (3) **All or none** — multi-file failure leaves everything untouched. Module is pure: strings/arrays in and out, no IO.
- **ask.ts**: "**a hold is memory-only, and it always settles.**" Four settle paths: answered, declined, turn interrupt, and the turn-end sweep (`turn.finished`). Settled parts are buffered until `message.finished` because the runner writes the parts array wholesale and would erase an out-of-band append. Restart leaves nothing pending by construction.
- **delegate.ts**: "**a blocking child is part of its spawner's turn; a detached one is not.**" `agent()`/`join()` cascade the spawner's abort signal into a still-running child (and detach the cascade the instant they resolve); `spawn()` never touches `ctx.signal` — only an explicit stop of the spawner session reaches it via registry cascade hooks. THERE IS NO DONE-GATE: no `checkPassed`; `ok` says only whether the child's TURN completed, `status` distinguishes errored/interrupted/orphaned.
- **artifact.ts**: (1) "**Publishing never touches the workspace**" — bytes go to `~/.bough/artifacts/<sessionId>/`; (2) "**Names and session ids are CONFINED to their directory**" (server-side path-construction guard, explicitly *not* a sandbox); (3) "**The filesystem is the source of truth**" — no artifacts table, listing walks the directory.
- **schedule.ts**: "**`next_run_at` is always computed FROM NOW, never from the stale stored value**" — a laptop asleep through 16 slots fires once, then cadence resumes. Also recomputed from now on spec change and on disabled→enabled (the disabled stretch is not downtime). `parseSpec`/`nextRun` are pure with `now` injected.
- **state.ts**: "**the store is keyed by the LINEAGE ROOT, never by the session id**" — a fork, compaction child, and subagent share one store. Second invariant: "**it is notes, not storage**" — 16KB/value, 200 keys/lineage, oversized value REJECTED never truncated.

---

## 2. Public API

### shell.ts
- `type ShellCtx = { sessionId, workspace, exits?: Vec<{command, code}>, signal?: AbortSignal, scratch?: String, record?: fn(FinishedCommand), echo?: { note(cmd, exitCode, output) -> Option<String>, guard(cmd) -> Option<String> } }` — what shell verbs need from a turn; `TurnCtx` satisfies it structurally.
- `interface ShellOptions { registry?, bgAfterMs?, shTimeoutMs? }` — injected seams; every default a constant.
- `DEFAULT_BG_AFTER_MS = 60_000`; `SH_TIMEOUT_MS = 120_000`; `defaultBgAfterMs()` — env override `BOUGH_BASH_BG_AFTER_MS` (finite, > 0), read per resolution not per call.
- `async bash(command, ctx, opts, tags="") -> String` — run via `sh -c` in workspace; non-zero exit reported inline as `[exit code N]`, not thrown; past `bgAfterMs` promotes to background (force) and returns the handoff note.
- `type ShCommand = String | { cmd, tag? }`; `interface ShResult { code: i32, out: String }`.
- `async shConcurrent(commands, ctx, opts) -> Vec<ShResult>` — truly concurrent, results in input order, **never rejects** (spawn failure/deadline/interrupt all become an ordinary `{code:-1|real, out}` with explanatory text).
- `createShellHostFns(ctx, opts) -> {bash, sh, bashBg, bashOutput, bashWait, bashKill}` — bridge layer. `bash(cmd, tags)` **requires tags** at this boundary only (ProgramError teaching format `"git:push:main"`; internal callers/tests owe none). `sh(cmdsJson)` parses a JSON array of strings-or-`{cmd,tag}` with schema errors that teach the call shape; returns `JSON.stringify(Vec<ShResult>)`.

### jobs.ts
- `interface ExitStatus { code: i32, signal: Option<String> }`.
- `interface Shell { id, name, command, sessionId, pid, startedAt, endedAt, killed, child, head, tail, written, scratch?, sink?, readTo, status: Option<ExitStatus>, exit: Promise<ExitStatus>, pumps: Promise<()>, onExit?, claimed, notified, limits }` — one tracked shell; buffer is `head` (fills once, immutable when full) + `tail` (rolling) + `written` (true total).
- `normalizeJobName(raw) -> Option<String>` — collapse whitespace, strip control chars (they'd repaint the TUI), cap 60 chars with `…`.
- `deriveName(command) -> String` — first meaningful words: strip `cd … &&` preludes and leading `VAR=value` assignments, split at `| && ;`, fallback `"shell"`.
- `shellInvocation(command, cwd) -> { argv: ["/bin/sh", "-c", command], cwd }`.
- `shellText(shell) -> String` — full retained buffer with omission marker.
- `formatFinal(shell, deps?) -> String` — spilled body + optional `[exit code N on SIG]` line; `"(no output)"` when empty.
- `backgroundNote(shell, id, afterMs) -> String` — `[still running after Ns — moved to background as bg_N "name". It keeps running; you'll be notified … bashOutput/bashWait/bashKill…]` + spilled output-so-far; advances `readTo`.
- `class JobRegistry` (options: `bus?, notify?, now?, limits?, maxRunning?` default 8):
  - `attachBus(bus)`, `attachNotifier(notify)` — post-construction wiring (server builds them in the other order).
  - `spawn(command, {cwd?, signal?, scratch?, sessionId?}) -> Shell` — spawns detached (new process group; blocks `/dev/tty` grabs), stdin ignored, stdout/stderr piped and pumped; env adds `BOUGH_SCRATCH` and `BOUGH_SESSION` when present; **the abort signal is deliberately NOT given to the spawn API** (the tree walk must snapshot descendants before the direct child dies). Does NOT register.
  - `promote(shell, ctx, {force?, name?}) -> Option<String>` — assign `bg_{seq}` id + name, register per session, wire `onExit`; `None` when at cap and not forced; auto-background always forces (never kills). Handles the race where the shell already exited (calls `onExit` directly instead of emitting `job.spawned`).
  - `bashBg(name, command, ctx, {wake?}) -> String` — JSON `{id, name, pid}`. Name REQUIRED (ProgramError), non-empty command required, ConflictError past cap naming running ids. Spawned **without** the turn signal. `wake:false` pre-sets `notified` (used by the TUI `!cmd` path) **before** the process can exit.
  - `bashOutput(id, sessionId) -> String` — delta since last read (advances `readTo`) spilled + status line `[running]` / `[exited with code N on SIG]`; `"(no new output)"` when empty.
  - `async bashWait(id, sessionId) -> String` — sets `claimed`, awaits exit + pumps, returns `bashOutput`.
  - `async bashKill(id, sessionId) -> String` — already-exited: `"bg_N already exited with code C"`; else claimed+killed, `signalTree(SIGTERM)`, SIGKILL backstop after `KILL_GRACE_MS=2000`, awaits real exit + pumps, returns `killed bg_N (SIGTERM|exit N)`.
  - `listJobs(sessionId) -> Vec<BackgroundJob>` — running + exited-within-`RECENT_MS` (30 min); running first, then newest.
  - `jobTail(id, lines=5) -> Option<{tail: Vec<String>, outputLines}>`; `jobOutput(id) -> Option<{output, job}>` — non-destructive (never advance `readTo`; a human glance must not steal the model's cursor). Both look up **across all sessions** (`#find`).
  - `killJob(id)` — cross-session kill for the UI (NotFoundError otherwise); `killJobsOf(sessionId) -> n` — SIGTERM the session's running shells, sets `killed`+`claimed` (no wake note); `killAll()` — server shutdown (a silent shell survives SIGPIPE and must be killed explicitly); `runningIds(sessionId)`; `async drain()` — await every exit+pumps.
  - `trackForeground(shell, sessionId) -> untrack-fn` and `inflightForegroundOutput(sessionId) -> Option<String>` — interrupt-time partial output blocks (`[interrupted] bash "cmd" — output before the interrupt:\n…`), read by the turn runner.
- `descendantPids(root) -> Vec<pid>` — parse `ps -Ao pid=,ppid=` (macOS has no setsid-group option here), deepest first, cycle-guarded (ps can race a reparent).
- `signalTree(shell, sig)` — descendants first (so a parent can't restart one), skip pid ≤ 1 and self, then the direct child; every kill error swallowed.
- `pub static jobs: JobRegistry` — the process-wide instance; tests construct their own.
- Session-scoped lookup errors name the ids that DO exist (`#require`).

### spill.ts
- `MAX_HEAD_CHARS = 100_000`, `MAX_TAIL_CHARS = 300_000`, `MAX_BUF = 400_000` — retention budget.
- `SPILL_OVER_CHARS = 20_000`, `SPILL_HEAD_CHARS = 5_000`, `SPILL_TAIL_CHARS = 5_000` — spill threshold and inline extract (symmetric on purpose: the file holds everything, the extract is a preview). These are CEILINGS: `hostfn::budget` scales the extract down to 1% of the reading model's context window, so a 131k-window model keeps ~5.2k chars, not 10k.
- `omissionMarker(omitted, total) -> String` — `\n[… N chars omitted from the middle of T — head and tail are verbatim. Filter at the source (rg, head, tail, targeted reads) instead of dumping output …]\n`.
- `truncateMiddle(text, {head?, tail?}) -> String` — pure, deterministic.
- `interface SpillDeps { exists, mkdirp, write, append, read }` — injected filesystem. `read` exists solely so the digest can be built from the complete file rather than the capped in-memory buffer.
- `interface SpillSink { path, chars, lines }`; `streamSpill(sink, text, ctx{scratch?, label?, totalSoFar, pending()}, deps) -> Option<SpillSink>` — opens lazily on the first chunk past threshold, seeding the file with `pending()` (the pre-threshold buffer); appends thereafter; counts lines while streaming (`- 1` per append so a chunk-spanning line isn't double-counted); any write failure returns the sink unchanged (a full disk must not kill a running command).
- `planSpill(text, canWrite) -> SpillPlan{spilled, head, tail, omitted, lines}` — pure decision.
- `spillMarker(path, total, lines, omitted, digest?) -> String` — names the path, true total, and the runnable follow-ups (`rg -n 'error|fail' '<path>'`, `bough patterns --llm '<path>'`, `view("<path>")`), ends "Do not re-run the command to see the middle". Paths shell-quoted. **With a digest** the `bough patterns` hint is dropped (its job was to get that analysis run, and it has been) and the digest is appended under `WHAT THE FULL OUTPUT IS MADE OF:`.
- `digest(text) -> Option<String>` — the log pipeline (`logs::analyze`) applied to spilled output, so the model learns what the output consists of in the same result instead of being handed a path and left to guess what to grep for. `DIGEST_TOP = 6`, `DIGEST_MAX_CHARS = 4_000` (cut at a `### ` pattern boundary by `clipToPattern`, which then says how many patterns it dropped), `DIGEST_MIN_LINES = 40`, `DIGEST_MIN_RATIO = 4` (one line in four must repeat, else prose/diffs/source get listed back as noise), `DIGEST_MAX_ANALYZE_CHARS = 8_000_000`. Pure — reads and writes nothing.
- `spill(text, ctx{scratch?, label?, sink?}, deps) -> String` — bound text for a tool result. With a sink: use THAT file and THAT total (the in-memory text is the retained size, not the real one), and digest the file's contents — digesting the retained text would describe a sample under a banner reading "FULL OUTPUT"; an unreadable file costs the digest and nothing else. Without: plan → write `<label>-NNN.log` → head+marker+tail, or fall back to `truncateMiddle` on no scratch / write failure. `nextPath` probes `label-001.log`…`999` for the first free name (a counter resets across restarts and would overwrite); slot 999 is reused explicitly.

### files.ts
- `MAX_SNAPSHOTS_PER_SESSION = 64`, `MAX_SESSIONS = 32`, `MAX_VIEW_BYTES = 2 MiB`, `MAX_TRACKED_WRITES = 200`. `MAX_VIEW_BYTES` is a backstop, not the operative limit: `hostfn::budget` caps a view at 10% of the reading model's context window (131k window → 52,428 bytes), because 2 MiB is ~500k tokens and would overflow any real window several times over.
- `class SnapshotStore` — `record(sessionId, absPath, text)` / `get` / `size` / `clear`; double-LRU (per-session file map, and per-store session map, both re-inserted on touch; eviction = oldest map key). `pub static sessionSnapshots`.
- `takeSessionWrites(sessionId) -> Vec<String>` — read-and-clear list of paths this session's programs wrote (fills the subagent report's `changedFiles`; git can't — siblings share the checkout).
- `createFileHostFns(ctx{workspace, sessionId, reads?}, {snapshots?}) -> {view, patch, write}`:
  - `view(path)` — stat first: directory → BadRequest ("list it with bash"); > 2 MiB → BadRequest (refuse, never truncate: truncated numbered lines would invite anchors against unseen lines); NUL byte → BadRequest (binary, lossy decode); ENOENT → NotFound naming the resolved path. On success: `snapshots.record(sessionId, resolvedAbs, text)`, push resolved path to `ctx.reads`, return `renderNumbered(pathAsWritten, text)`; empty file appends `(this file is empty — use INS.HEAD: …)`.
  - `patch(input)` — parse → group → **alias check** (two spellings resolving to one absolute path in one patch → PatchError; groupByFile merges by literal string only) → read every file (ENOENT → PatchError "create it with write(); nothing was written") → collect snapshots into `base` map (absent = never viewed; engine refuses) → `applyPatch(current, ops, {base})` (throws before anything is written) → write each file, `snapshots.record` + `recordWrite` per file; a mid-loop write failure reports exactly which files landed ("Already written and NOT rolled back: … re-view those"). Returns one line per file: `[path#NEWTAG] patched — N operations, now M lines`.
  - `write(path, content)` — content must be a string (else BadRequest telling to JSON.stringify); mkdir -p parents; write; record snapshot + write; returns `[path#TAG] wrote N lines (B bytes)`.

### patch.ts (pure engine)
- `type OpKind = swap | del | ins_pre | ins_post | ins_head | ins_tail`.
- `struct PatchOp { path, tag /* 4-hex uppercase or "" */, kind, a?, b?, body: Vec<String>, at /* input line, for errors */ }`; `struct FileOps { path, tag, ops }`.
- `normalize(text)` — strip leading BOM, CRLF→LF. `tagOf(text)` — low 16 bits of FNV-1a (offset 0x811c9dc5, prime 0x01000193, **over UTF-16 code units** — `charCodeAt`; a Rust port iterating bytes or chars would produce different tags, fine as long as it's self-consistent AND old transcripts are not replayed against it) as 4 uppercase hex digits. Collision degrades to a *rejected* patch (rebase re-checks lines), never a wrong one.
- `toLines(text)` — normalized split, trailing empty element dropped. `joinLines(lines, original)` — re-attach CRLF style and trailing-newline presence; empty result is `""` not `"\n"`.
- `renderNumbered(path, text)` — `[path#TAG]\n` + `N:text` lines, N right-padded to common width, **no space after the colon**.
- `parsePatch(input) -> Vec<PatchOp>` — grammar: section headers `[path#TAG]` / `[path#]` / `[path]` (tag uppercased); ops `SWAP A.=B:` (also `A..B`, `A-B`, `A B`, bare `A`; trailing `:` optional), `DEL A.=B`, `INS.PRE A:`, `INS.POST A:`, `INS.HEAD:`, `INS.TAIL:`; body rows `+text` (`+` stripped; lone `+` → `""`); blank line ends a body (never content); `*** Begin/End Patch` envelopes silently swallowed. Diagnostics by shape: `+` row with no open op (special text if the previous op was DEL — "DEL takes no body rows"), `-` rows rejected by name, `NNN:` rows diagnosed as pasted view() output, op before any header, empty patch, section with zero ops. All `PatchError` naming the input line and the fix.
- `groupByFile(ops)` — merge repeated sections of one literal path preserving first-appearance order; two different tags for one path → PatchError.
- `checkOps(path, ops, count)` — bounds (special text for an empty file: "use INS.HEAD:"), inverted range, bodiless SWAP ("DEL is how you remove lines"), overlapping swap/del spans, INS.PRE/POST anchored strictly inside another op's replaced span (ins_pre inside = `a < x <= b`; ins_post inside = `a <= x < b`).
- `materialize(lines, ops) -> Vec<String>` — one pass. Fixed gap order: HEAD bodies; per line: its PRE bodies, then the line (or SWAP body, or nothing for DEL), then POST bodies **keyed to the span's last line** (i.e. `INS.POST b` of a swallowed span still lands); finally TAIL. Same-kind ops at one anchor emit in patch order; `POST N` precedes `PRE N+1`.
- `lineMap(base, cur) -> Vec<Option<usize>>` — common prefix + suffix trimmed, then LCS over the diverged middles; `LCS_CAP = 400` lines per side (past it, everything in the middle reports changed — costs a rejected patch, never a wrong one). Monotonically increasing.
- `rebaseOps(ops, base, cur) -> {ok, ops} | {ok:false, conflicts: [{op, reason}]}` — an op survives iff every line of its span maps AND stays contiguous AND **every interior line** maps to exactly its offset position (endpoints-only checking would accept an in-place interior rewrite — the single most common concurrent-edit shape). Reasons: "lines A.=B were rewritten" / "had lines inserted inside them". ins_head/ins_tail always survive. All conflicts collected, not just the first.
- `applyPatch(files, ops, {base?}) -> Map<String,String>` — new map, neither argument mutated, repeatable. Per group: path must be in `files`; resolve base (`resolveBase`: base map present + path present → snapshot (explicit tag must equal `tagOf(snapshot)` or stale-tag error naming the CURRENT tag and the `[path#]` escape); base map present + path absent → "no viewed version … call view() first"; no base map → current text is base, explicit tag verified against it); `checkOps` in **viewed** coordinates; if `normalize(base) != normalize(current)` rebase (refusing with `conflictMessage` listing every reason + "Nothing was written — a patch applies to all its files or none"); `joinLines(materialize(currentLines, effective), current)`.

### ask.ts
- `type AskSettlement = answered | declined | interrupted`.
- `class AskHolds` (injected clock): `raise(bus, {sessionId, messageId, question, options?}, signal?) -> {record: AskQuestion, answer: Promise<String>}` — UUID id, `status:"pending"`, empty-string options filtered; hold registered **before** the `ask.question` publish (a synchronous same-process answerer must find it); already-aborted signal settles immediately; settle is first-wins (double-settle is an ordinary race, not an error) and re-emits the SAME id with final status. No timeout — user-paced by design; the turn bounds it. `answer(id, text) -> bool`, `decline(id) -> bool` (rejects with `AskDeclinedError` whose message starts `user declined to answer: <q>` — the phrase is load-bearing, the prompt tells the model to catch exactly it), `get(id)`, `list(sessionId?)` (oldest first — how a reconnecting client rebuilds cards; events never replay), `expire(sessionId?) -> n` (settle all as interrupted — the sweep), `size`.
- `pub static askHolds` + free-fn wrappers `raiseAsk/answerAsk/declineAsk/getAsk/pendingAsks/expireAsks` (the HTTP routes call these).
- `appendAskPart(db, bus, sessionId, messageId, part) -> bool` — idempotent on part id; **preserves `message.pending`, never sets it** (a late settle must not flip a finished message back to busy); publishes `message.part`; missing message → false, not an error.
- `createAskHostFn(ctx, {holds?, append?}) -> {ask}`: `ask(question, optsJson="{}") -> String` (the plain answer, not JSON). Options parsed leniently (`{options: [1,2]}` → `["1","2"]`; `""`/`"null"`/`"undefined"` → none; non-object bag → BadRequest). Empty question refused; already-aborted turn refused before announcing a card. Lazily arms a bus subscription on first ask: `message.finished` for this message flushes buffered settled parts to the row; `turn.finished` for this session disarms, flushes, and `expire(sessionId)`s. During the turn settled AskParts are **buffered**, after close appended straight through. On rejection the transcript part carries `declined` vs `interrupted` from `record.status`.

### delegate.ts
- `type DelegationTier = top | nested | none`. `TOP_LEVEL_DELEGATION = [agent, spawn, join, adopt]`; `NESTED_DELEGATION = [agent, adopt]` (spawn/join withheld: a detached grandchild would keep mutating the shared checkout after its spawner's report went up); `delegationFnsFor(tier)`.
- `delegationTier(db, sessionId)` — derived from lineage, never a flag: missing session or `workflow_agent` → none; `subagentDepth == 0` → top; `< MAX_SUBAGENT_DEPTH` → nested; else none. `childTierOf(tier)` — top→nested→none, one hop down never sideways.
- `class DetachedSubagents` — by child session id; `register`, `get`, `idsFor(spawnerId)`, `claim(spawnerId, sessionId)` (idempotent — re-join is a program being careful; wrong/foreign id → AgentError 400 naming this session's detached ids, or explaining the register is memory-only and cleared by restart), `forget`, `size`. Finished records are KEPT (join-after-completion is normal). `pub static detachedSubagents`.
- `DelegationOptions` schema: `{name?: String, model?: String, effort?: low|medium|high|xhigh|max}` — non-JSON or wrong shape → AgentError 400 teaching the form ("always pass a name").
- `createDelegationHostFns(ctx, deps{tier?, registry?, detached?, launch?, caps?, exempt?, child?, deliver?, reportError?}) -> DelegationHostFns` — returns ONLY the tier's verbs; `none` returns `{}` (absence = denial; the bridge rejects unknown names). Internals:
  - `childDeps` fills `changedFiles: (session) => takeSessionWrites(session.id)` unless caller supplied one.
  - `stopIfRunning(sessionId)` — cascade only into a child the registry says is running (interrupting a finished one would flip a persisted outcome).
  - `awaitAsOwnWork` — add abort listener → `JSON.stringify(await result)` → always remove listener.
  - `assertLive(verb)` — aborted turn → AgentError 409, nothing launched.
  - All launches go through `cappedLaunch` (width caps, T4.3): a refusal throws `SpawnCapError` naming which cap and costs running siblings nothing (`Promise.allSettled` fan-out stays lossless).
  - `agent(task, optsJson) -> JSON String` of `SubagentResult` `{sessionId, title, ok, status, report, changedFiles}` — blocking, cascaded.
  - `spawn(task, optsJson) -> JSON {sessionId, title}` — registers detached record, hooks `registry.onInterrupt(spawnerId, …)` for explicit-stop cascade, on completion delivers via `deps.deliver` unless claimed, errors go to `reportError`, hook unhooked in finally. Async fn even though nothing awaits: a refusal must arrive as a rejection like every other.
  - `join(sessionId)` — claim then `awaitAsOwnWork`.
  - `adopt(sessionId)` — validates `kind == "subagent" && originId == ctx.sessionId` (else AgentError 400 "cannot adopt a sibling, a grandchild, or an ordinary session"); publishes `session.updated`; returns an explanatory string: there is no worktree and nothing to merge, plus a next-step hint (join / wait / read the tree). Deliberately does NOT mark a detached child claimed. Vestigial: still bridged (old transcripts replay), no longer in the prompt.
- `delegationTurnDeps(tier, wiring)` — builds `TurnDeps` with `granted = base.granted ?? BASE_HOST_FNS + delegationFnsFor(tier)` and a `programFor` that composes base + extension + delegation host fns; the child's launch deps recurse lazily with `childTierOf`. `createDelegatingTurnStarter(wiring)` — three pre-built starters, picked per session at start time (the grant array is read once by the runner and must vary by tier).

### artifact.ts
- `struct Artifact { name /* session-relative, forward-slashed */, url /* /artifacts/<sid>/<name>, URI-encoded per segment */, href /* baseUrl + url */, bytes, ts /* mtime ms */ }`.
- `serverBaseUrl()` — `http://127.0.0.1:${BOUGH_PORT ?? 4321}` (loopback only, spec §17).
- `sessionArtifactDir(sessionId, {root?, baseUrl?})` — `confine(root, id)` plus **direct-child check** (rejects a descending id like `a/b` that stays in root but addresses a foreign dir); empty id → PathError.
- `resolveArtifactPath(sessionId, name)` — leading slashes stripped (meaning the store's own root, which is what every caller intends), then confined; name resolving to the dir itself → PathError.
- `async publishArtifact(sessionId, name, content, opts)` — mkdir -p, write (overwrite in place — an open link must keep working), stat, return Artifact.
- `listArtifacts(sessionId, opts)` — recursive dir walk, newest-first by mtime; unaddressable id or missing dir → `[]`; races with deletes swallowed.
- `publishForProgram` — wraps PathError → ArtifactError 400 teaching plain relative names; anything else → ArtifactError 500.
- `createArtifactHostFn(ctx, deps)` — `artifact(name, content) -> JSON {name, url, href, bytes}`; content `?? ""`; scoped to `ctx.sessionId` (a program never gets to name a session at all — a subagent publishes into its OWN directory).

### schedule.ts
- `type ParsedSpec = Every{ms} | Daily{hh, mm}`; `SPEC_HELP` string; `parseSpec(spec) -> Option` — `every:(\d+)(m|h|d)` with N ≥ 1 (`every:0m` would fire every tick), `daily@H{1,2}:MM` with hh ≤ 23, mm ≤ 59.
- `nextRun(spec, from) -> epoch_ms` — strictly after `from` (equal would be due again next tick); `every` = `from + ms`; `daily` via LOCAL wall-clock date math (set HH:MM:00.000 today, +1 day if `<= from`) so DST is absorbed — the run stays at HH:MM local.
- `resolveWorkspace(raw)` — `~`/`~/` expansion, absolutize, must stat as an existing directory now (else ScheduleError 400: every firing opens a session in it).
- `scheduleCreate(db, body{title, prompt, spec, workspace?, enabled?}, deps{now?, workspace?, sessionId?}) -> Schedule` — uuid, `nextRunAt = nextRun(spec, now)` (created at 09:00 `every:2h` → due 11:00, not immediately), `sessionId` from deps (stamped by the host fn from the calling turn, NEVER from the wire; REST path → null).
- `schedulePatch(db, id, patch, deps)` — recompute `nextRunAt` from now **iff** spec changed or disabled→enabled; explicit `workspace: null` clears; unknown id → ScheduleError 404.
- `scheduleRemove(db, id)` — 404s rather than silently succeeding.
- `scheduleVerb(db, verb, args, defaultWorkspace, deps)` — `list`; `add` (Zod-validated body; workspace defaults to the caller's turn workspace, explicit one wins); `enable`/`disable` (bare id string, else a 400 teaching `schedule.verb("<id>")`); `remove` → `{ok:true, removed:id}`; unknown verb names the five.
- `createScheduleHostFn(ctx, deps)` — `schedule(verb, argsJson) -> JSON String`; empty argsJson → null args; non-JSON → ScheduleError 400; `now = deps.now ?? ctx.now`; `sessionId = ctx.sessionId`; default workspace `ctx.workspace`.

### state.ts
- `MAX_VALUE_BYTES = 16_384`, `MAX_KEYS = 200`, `MAX_KEY_CHARS = 200`; `struct StateEntry { key, bytes, updatedAt }` (list returns keys and sizes, never values).
- `lineageRoot(db, sessionId)` — walk `ancestorChain(id)[0]` (parentId chain); if that root is a `subagent`/`workflow_agent` with an `originId`, hop the delegation edge and continue (subagents have `parentId: null` — parent-walk alone would make every subagent its own root, which is the pinned NOTE/delta from the old port). `seen` set breaks cycles from a bad write; unknown session is its own root.
- `stateVerb(db, rootId, verb, args, now)`:
  - `get(key | {key})` → parsed JSON value, or **null** for unset (`?? default` is the idiom — never a throw); stored non-JSON → StateError 500 saying it wasn't written by state.set.
  - `set({key, value})` → `{ok:true, key, bytes}`. `value === undefined` → 400 ("use state.delete"); unserializable → 400; serializes-to-undefined (function/symbol) → 400; UTF-8 byte length > 16KB → 400 "Nothing was stored" (put the payload in a file, store its path); key-count cap checked **only for a new key** (overwrite must keep working at the cap or a full lineage couldn't correct itself).
  - `list` → `Vec<StateEntry>`; `delete(key)` → `{ok, key, removed: bool}` ("there was none" is an answer, not an error).
  - Key rules: non-empty string, ≤ 200 chars.
- `createStateHostFn(ctx, {rootId?, now?})` — `state(verb, argsJson) -> Promise<String>`; root resolved **per call** (a turn can outlive a lineage edit); unset get returns the four characters `null`; all failures are promise rejections, never synchronous throws (contract parity with sibling verbs).

---

## 3. Data structures / wire shapes (exact field names)

- `ShResult`: `{"code": number, "out": string}`; `sh` returns `JSON.stringify(ShResult[])`.
- `bashBg` return: `{"id": "bg_3", "name": string, "pid": number}`.
- `BackgroundJob` (schema/parts.ts, frozen): `{id, name (default ""), sessionId, pid, command, status: "running"|"exited", exitCode: number|null, signal: string|null, startedAt, exitedAt}` — a killed shell reports as `exited`; the `signal` field is what lets the transcript card avoid `✓ done` on a user-killed job. NOT persisted.
- `AskQuestion`: `{id, sessionId, messageId, question, options?, status: "pending"|"answered"|"declined"|"interrupted", answer?, ts}` — bus event `ask.question`, emitted on raise and re-emitted on the same id at settle.
- `AskPart` (message part, persisted): `{type:"ask", id, question, options?, status: "answered"|"declined"|"interrupted", answer?}` — settled only, never pending.
- `agent()`/`join()` return: `JSON.stringify(SubagentResult)` = `{sessionId, title, ok, status, report, changedFiles}` (see agents/subagent spec). `spawn()` return: `{"sessionId", "title"}`.
- `artifact()` return: `{"name", "url", "href", "bytes"}`.
- `state.set` return `{"ok":true,"key","bytes"}`; `state.delete` `{"ok":true,"key","removed":bool}`; `state.list` `StateEntry[]`; `state.get` the raw value or `null`.
- `schedule` verbs return the `Schedule` row(s) JSON, or `{"ok":true,"removed":id}`.
- Bus events published here: `job.spawned`, `job.exited` (data = BackgroundJob), `ask.question`, `message.part`, `session.updated`. Subscribed: `message.finished`, `turn.finished`.
- DB touched: sessions (read: `getSession`, `ancestorChain`), messages (`getMessage`, `updateMessage`), schedules table (full CRUD via `Db`), state table (`getState/setState/listState/deleteState` keyed `(rootId, key)`), plus the command-history recorder callback (`ctx.record`, writes handled elsewhere in history/). Jobs, asks, detached subagents, snapshots: memory-only by contract.
- Command record (`ctx.record` callback shape): `{command, tags, exitCode: number|null, durationMs: number|null, outputHead (≤ 2000 chars, `OUTPUT_HEAD_CHARS`), spillPath: string|null}`; `spillPathFrom` re-parses the path out of the spill marker (`/FULL OUTPUT SAVED[^\n]*\n\s+(\S+)\n/`).
- Env exported to every shell: `BOUGH_SCRATCH` (session scratchpad, when present), `BOUGH_SESSION` (session id — `bough mcp` scopes grants by it; the model must never compose it).

---

## 4. Behaviors & edge cases (mined from tests + code; a naive port gets these wrong)

**Shell / jobs**
- Auto-background NEVER kills and ignores the concurrency cap (`promote(force:true)`); the cap brakes only explicit `bashBg` (ConflictError names the running ids). Pinned by tests "an auto-backgrounded command is never killed by the threshold" and "auto-background ignores the concurrency cap so no command is ever lost".
- The kill path must signal the whole TREE: `sh -c 'printf x; sleep 60'` doesn't forward SIGTERM; killing only the shell orphans the grandchild holding the stdout pipe and the read hangs. Signal descendants (via `ps` parse) deepest-first, then the child. Test: "interrupting a bash kills the grandchild too".
- The abort signal is not handed to the process-spawn API (the direct child would die before the tree snapshot) — `killTreeOnAbort` attaches an abort listener instead, and an already-aborted signal is handled explicitly (a listener on an aborted signal never fires). The listener stays attached **past promotion** (an interrupt must kill auto-backgrounded children of the running program) and detaches only after natural exit.
- `bash` interrupt semantics: aborted before spawn → throw immediately, nothing spawned/recorded; aborted mid-run → wait `drained(1000ms)` for pipe flush (partial output attached by the turn runner via `inflightForegroundOutput`), then throw `ProgramError` "command killed: the turn was interrupted … Anything it had already done still stands; nothing was rolled back". Aborted-after-exit also throws (checked between exit and return).
- `drained()` is bounded (1s): a finished command whose pipes were inherited by a grandchild dev server must not become an unbounded wait.
- Non-zero exits are pushed to `ctx.exits` **before** the string is returned (transcript honesty: the program may not log it, and the model once invented "bash() threw" — the harness must know the code independently).
- Echo memory ordering: `echo.guard(cmd)` runs before spawn (a skipped command is not spawned and NOT recorded — it must not enter the memory as another failure of itself); `echo.note(cmd, code, out)` is asked **before** this run is recorded ("already failed 3×" means three before this one); the note is appended BELOW the output separated by a blank line (`withEcho`). `sh` legs are noted but never guarded (concurrent legs race the guard's count).
- Promoted bash records the REAL exit later (fire-and-forget on `shell.exit`, `spillPath: null`, head from the retained buffer), not the handoff. Test: "an auto-backgrounded bash records the REAL exit, not the handoff".
- `bashWait`/`bashKill` set `claimed` → no completion note. `bashBg(wake:false)` sets `notified` BEFORE the process can exit (an `echo` finishes in the same tick). A clean, silent, unclaimed exit (code 0, no signal, zero output lines) posts NO note (would wake an idle session into a paid turn); noisy or failed exits do, with the note text naming id, name, exit, command head (60 chars), line count, and `bashOutput("bg_N")`.
- `bashOutput` delta-reads via absolute offset `readTo` against the head+tail buffer; a hole (unread chars that fell out of retention) renders the omission marker inline. UI reads (`jobOutput`/`jobTail`) never advance the cursor; they also look up across ALL sessions (the jobs endpoint aggregates subagent rows), while the model's verbs are session-scoped and their not-found error lists this session's actual ids.
- Retention: head fills first (immutable once full), tail rolls; `written` counts everything. `retainedFrom` stitches head + marker + tail from an absolute offset.
- Children spawn **detached** (fresh process group), stdin ignored: an interactive program (ssh/sudo/pinentry) must not be able to open `/dev/tty` and paint over the TUI's alternate screen. Test: "shells cannot write through the TUI's controlling terminal".
- Server shutdown must `killAll()`: a silent shell (bare sleep, idle dev server) never touches its broken pipe, survives SIGPIPE, and would be reparented invisibly.
- `sh`: per-leg 120s SIGKILL deadline with message naming `bashBg` as the escape hatch; spawn failure → `{code:-1, out:"could not start command: …"}`; interrupt → prefix `[the turn was interrupted; this command was killed]`; concurrency is real (tests assert overlap by timing).
- `raceExit`/timers are unref'd — pending threshold timers must not keep the process alive.

**Spill**
- The sink STREAMS: `append` updates the retained buffer FIRST, then `streamSpill` — so the sink opening mid-stream includes the very chunk that crossed the threshold via `pending()` (= head+tail, still whole because 20k ≪ 400k). The historical bugs each "shipped a lie": (1) writing the buffer at the end saved an already-truncated 400KB file under a "FULL OUTPUT SAVED" banner; (2) opening before writing dropped exactly one 262,144-char chunk. Tests: "the streamed file holds every byte, including the chunk that opened it", "the sink survives past the retention cap without losing the middle", "the marker reports the true total, not the retained size".
- Line counting is incremental (`countLines` counts partial trailing lines; each append subtracts 1 so a line spanning chunks isn't counted twice); the file is never re-read.
- Write failures anywhere (mkdirp, open, append) silently degrade to `truncateMiddle` — a full disk must not fail a successful command.
- `nextPath` probes for the first free `label-NNN.log`; label = the job id (`bg_3`) once promoted, else `bash`/`sh`/`output`.
- `formatFinal` spills BEFORE appending the exit line (the marker must not be separated from the output it describes).

**Files / patch**
- Snapshot keyed by RESOLVED absolute path: `view("m.ts")` then `[./m.ts#]` is one record ("echoed as WRITTEN, recorded as RESOLVED"). Per-session strictly: a sibling subagent's view is not mine (deliberate: hash anchoring must distinguish "I saw this" from "someone told me").
- Empty tag `[path#]` resolves to the snapshot; explicit tag must match `tagOf(snapshot)` — but an explicit tag naming a *superseded-but-known* version still rebases (test pins it). A tag matching neither → stale-tag error naming the current tag and the empty-tag escape.
- The alias-check in `patch()` (two path spellings → one absolute path) exists because `groupByFile` merges by literal string; without it the second group's write would silently discard the first's.
- `write()` records its own content as the snapshot — a freshly written file is patchable with `[path#]` in the same round without a view.
- CRLF/BOM: identity (`tagOf`) and all line math run over `normalize`d text; output re-attaches the original EOL style and trailing-newline presence; emptied file → `""`.
- Bounds are judged in VIEWED coordinates, not the current file's (test: "bounds are judged in VIEWED coordinates").
- Rebase conflict reporting collects ALL conflicting ranges; one touched range refuses the file's other clean ops too (all-or-none within the file, and across files).
- `applyPatch` is repeatable and mutates neither argument (test-pinned).
- INS.POST at the last line of a DEL/SWAP span still lands (post is keyed to `b`, and the materialize loop reads `post[span.b-1]`).
- An empty INS body (`INS.PRE 2:` + lone `+`) inserts `""` — a no-op-ish blank line, not corruption; op order in the patch text does not change the result.

**Ask**
- Hold registered before the pending event publishes; settle re-emits the SAME id; first settle wins; `expire` sweeps per session or all.
- The buffered-parts dance is load-bearing: an AskPart appended mid-turn is erased by the runner's next wholesale parts write (test: "a part written during the turn would be erased — the buffer is why it is not"). Flush on `message.finished` (this message) or `turn.finished` (this session, which also disarms the subscription and expires holds). `appendAskPart` preserves `pending` and is id-idempotent.
- Two questions in one turn both land, in ask order. `ask()` on an aborted turn refuses before announcing a card.

**Delegate**
- The interrupt cascade reaches a blocking child and NOT a detached one; an explicit stop of the spawner session DOES reach a detached child (registry `onInterrupt` hook, unhooked when the child settles). Cascade only if `registry.isRunning(child)` — never flip a finished branch to interrupted.
- `join()` of an unknown/foreign id: error text differs by whether this session has any detached children (names them) or none (explains spawn/join and the memory-only register).
- A blocking child that fails reports why in its result (`ok:false`, `status`), it does not throw at the spawner. Cap refusals (`SpawnCapError`) fail the one launch alone.
- The grant list and the bridged set are built together per tier (`delegationTurnDeps`) and must never disagree; tests pin "each tier's grant matches what it can actually call" and "the wired starter picks the tier from the session it is starting".

**Artifact**
- Session-scoped by construction (program can't name a session); escaping names refused with nothing written; republish overwrites in place; nested `assets/app.js` paths publish and list with forward slashes; listing survives a DB reset (filesystem is truth).

**Schedule**
- `nextRunAt` recompute matrix (test-pinned): cosmetic edit → untouched; spec change → from now; disabled→enabled → from now; disabling → untouched (and a disabled row is never due). `daily@` already-past-today lands tomorrow, local time, DST-stable. Create is one interval out, never immediate. Host fn defaults workspace to the turn's; explicit workspace wins; `sessionId` stamped from the turn only.

**State**
- Lineage root: fork + parent share a store; compaction child + deep fork chain resolve to same root; subagent (parentId null, originId set) shares its spawner's; a subagent of a fork reaches the fork's root; cycles and unknown sessions terminate. Byte cap is on UTF-8 BYTES (a value just under lands); the 201st key is refused but overwrite at the cap works; unset get is `null` (the string `"null"` on the wire).

---

## 5. Dependencies

Imports (non-hostfn): `../errors.ts` (ProgramError, BadRequestError, NotFoundError, ConflictError, PatchError, AskDeclinedError, AgentError, SpawnCapError, PathError, ArtifactError, ScheduleError, StateError — all `HttpError` subclasses with a status code), `../types.ts` (`TurnCtx`, `AppCtx`, `Db`, `Bus`, `HostFns`), `../schema/parts.ts` (BackgroundJob, AskPart, AskQuestion, Part, Schedule, Session, Message), `../schema/events.ts`, `../schema/requests.ts` (CreateScheduleBody, PatchScheduleBody), `../paths.ts` (`confine`, `artifactsDir`), `../history/record.ts` (`normalizeTags`, `OUTPUT_HEAD_CHARS = 2000`, `spillPathFrom`), `../harness/protocol.ts` (`HostFnName`, `HOST_FN_VERBS`), `../agents/subagent.ts` (`launchSubagent`, `subagentDepth`, `MAX_SUBAGENT_DEPTH`, types), `../agents/caps.ts` (`cappedLaunch`, `SpawnCaps`), `../turn/queue.ts` (`TurnRegistry`, `turns`), `../turn/runner.ts` (`BASE_HOST_FNS`, `baseHostFns`, `createTurnStarter`, `defaultProgramRunner`, `interruptTurn`, `TurnDeps`).

Imported by: `turn/runner.ts` (wires the base host fns per turn), `server/jobs.ts`, `server/questions.ts`, `server/artifacts.ts`, `server/main.ts`, `schedules.ts` (ticker + routes), `history/explore.ts`, `workflow/control.ts`, `tui/api.ts`. Note the intentional cycle-ish coupling with `turn/` in `delegate.ts` only (delegation builds child turns); everything else is leaf-ward.

Process-wide singletons a port must reproduce (or replace with an injected `AppState`): `jobs: JobRegistry`, `sessionSnapshots: SnapshotStore`, `sessionWrites` map, `askHolds: AskHolds`, `detachedSubagents: DetachedSubagents`. All exist because `TurnCtx`/`AppCtx` are frozen with no slot to thread them, and each spans turns. In Rust, prefer one explicit `HostState` struct handed to the turn builder over statics.

## 6. External deps → Rust equivalents

| TS/Bun | Used for | Rust |
|---|---|---|
| `Bun.spawn` (detached, piped, env) | shell children | `tokio::process::Command` + `process_group(0)` (`std::os::unix::process::CommandExt`), `Stdio::piped()/null()`; capture `id()` before wait |
| `Bun.spawnSync(["ps",…])` | descendant pids (sync, used in shutdown) | `std::process::Command` (blocking) parsing `ps -Ao pid=,ppid=`; consider `libproc`/`sysinfo` crate but `ps` parse is the proven portable path on macOS |
| `process.kill(pid, sig)` | signalTree | `nix::sys::signal::kill(Pid, Signal)` |
| `AbortSignal` / listeners | turn interrupt | `tokio_util::sync::CancellationToken` (`.cancelled()`, `.is_cancelled()`); "listener on aborted signal never fires" maps to checking `is_cancelled()` first — CancellationToken handles this natively |
| Promises / `Promise.all` | sh concurrency, pumps | `tokio::join!`/`futures::future::join_all`; `Shell.exit`/`pumps` → `tokio::sync::watch` or a shared `JoinHandle`/`Notify` |
| `setTimeout(...).unref()` | bg threshold, kill backstop | `tokio::time::timeout` / `sleep` in a select — tokio timers never hold the runtime open, unref is free |
| `zod` | boundary validation (ShCommands, DelegationOptions, AskOptions, schedule bodies) | `serde`/`serde_json` + hand-rolled validation producing the SAME teaching error strings (the messages are product surface — port them verbatim) |
| `node:fs` sync+promises | spill, files, artifact walk | `std::fs` (spill deps are already an injectable trait), `tokio::fs` for the async verbs |
| `crypto.randomUUID()` | ask ids, schedule ids | `uuid::Uuid::new_v4()` |
| `TextEncoder/Decoder` | byte lengths, stream decode | `str::len()` for UTF-8 bytes; incremental UTF-8 decode of pipe chunks: `String::from_utf8_lossy` is NOT chunk-safe — use `encoding_rs` streaming decoder or buffer split codepoints |
| `Math.imul` FNV-1a over `charCodeAt` | `tagOf` | wrapping u32 mul over **UTF-16 code units** (`s.encode_utf16()`) if tag-compat matters; otherwise document the break |
| `toLocaleString("en-US")` | spill marker thousands separators | hand-format with commas (small helper) |
| `Date` local-time math | `daily@` next run | `chrono::Local` (`with_hour/with_minute`, `+ Duration::days(1)`, DST-aware) |
| Bus (in-proc pub/sub) | job/ask events | `tokio::sync::broadcast` behind the shared `Bus` trait from the core spec |

## 7. Suggested Rust layout

Crate `bough-hostfn` (or module tree under the main crate), tokio throughout:

```
hostfn/
  mod.rs        — HostFns trait + registration; the string-only call contract
  spill.rs      — pure: TruncateLimits, planSpill, markers; SpillDeps trait (4 fns); SpillSink
  jobs.rs       — Shell, JobRegistry (Arc<Mutex<Inner>> or actor task), signal_tree, descendant_pids
  shell.rs      — bash/sh + bridge wrappers; ShellCtx as a struct of borrows/Arcs
  patch.rs      — 100% pure; port first, it has the densest test suite (>90 cases) and zero deps
  files.rs      — SnapshotStore (Arc<Mutex<LruMap<…>>>; `lru` crate or hand-rolled via IndexMap re-insert), view/patch/write
  ask.rs        — AskHolds (Mutex<HashMap<Uuid, PendingAsk>> with oneshot::Sender settle)
  delegate.rs   — tiers, DetachedSubagents, the four verbs (defer: needs agents/ + turn/)
  artifact.rs   — path confinement + store + host fn
  schedule.rs   — parse/nextRun pure core + CRUD + host fn
  state.rs      — lineage_root + verbs + host fn
```

- **Trait boundary**: one `#[async_trait] trait HostCall { async fn call(&self, name: &str, args: Vec<String>) -> Result<String, HostError> }` per turn, built from the tier + granted list; individual modules expose plain constructors (`shell_host_fns(ctx, opts)`) returning boxed closures or a small enum-dispatch. `HostError` carries `{status: u16, message: String}` mirroring `HttpError` — the message text IS the model-facing product; port the strings verbatim.
- **Async boundaries**: `bash`/`sh`/`bashWait`/`bashKill`/`ask`/`agent`/`join` are genuinely async (await child exit / user / subagent). `view/patch/write/artifact` are async-IO but trivially so. `state`/`schedule`/`bashOutput`/`bashBg`/`spawn`/`adopt` are effectively sync returning `Ready` futures. The output pumps are two spawned tasks per shell feeding the retained buffer + sink under a small mutex; `Shell.exit` as `watch::Receiver<Option<ExitStatus>>`.
- **Interrupt**: replace `AbortSignal` with a per-turn `CancellationToken`; `bash` = `select! { _ = exit, _ = sleep(bg_after), _ = token.cancelled() }` with the same ordering semantics (exit wins → drain → return; timeout → promote; cancel → drain → error).
- **Registries**: build a `HostState { jobs, snapshots, writes, asks, detached }` owned by the server and cloned (Arc) into each turn — do not reproduce the module-static pattern; it exists in TS only because the ctx types were frozen.
- Pure cores (`patch.rs`, `spill.rs` planning, `schedule.rs` parsing, `state.rs` verbs, `lineage_root`) should be sync free functions with injected `now: impl Fn() -> i64` — the TS tests translate 1:1.

## 8. v1 scope cut

- **core** (the loop dies without them): `bash` (with interrupt kill-tree + auto-background + spill), `sh`, the four job verbs + `JobRegistry` (+ `killAll` on shutdown, foreground tracking for interrupt output), `spill.rs` whole, `view`/`patch`/`write` + `SnapshotStore` + the full patch engine (do NOT stub rebase — refusing every divergence would break the shared-checkout subagent story, but for a v1 without subagents you MAY ship "stale → refuse, re-view" and add rebase in v1.1; the tag/refuse rules themselves are non-negotiable), `ask` (TUI blocks on it), `state`.
- **high**: `delegate` (agent/spawn/join tiers — daily-driver but the single-agent loop works without it; `adopt` can be the explanatory-string stub it already effectively is), `artifact`, the command-history hooks on `ShellCtx` (`record`/`echo` — optional fields already; ship `None`).
- **later**: `schedule` (needs the ticker anyway), `jobTail`/`jobOutput` UI niceties, `takeSessionWrites` → changedFiles reporting, `wake:false` path, echo/guard memory.
- **stub**: `workflow` verb surface (owned by workflow/ spec), `adopt` beyond the lineage check + canned string.
