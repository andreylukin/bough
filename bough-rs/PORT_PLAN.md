# bough-rs — Port plan (dependency-ordered waves)

Companion to `ARCHITECTURE.md`. Each module row: **disposition** (port | v1-stub |
drop) and a **verification gate** — the check that proves the module before the next
one builds on it. A wave is done when every `port` row's gate is green AND the wave's
parity smoke passes (Rust TUI ↔ TS server and/or TS TUI ↔ Rust server, per wave).

Conventions:
- "spec §" references are into `specs/<name>.md`.
- v1-stub = compiles, honest minimal answer, contract-shaped (never `todo!()` on a
  reachable path); the stub's answer is one the TS system can legitimately give
  (empty list, `None`, 400 "not yet ported").
- Port order **within** a wave follows the listed order — it is topological.

---

## Wave 1 — the core loop

Goal: model writes a program, patches a file, streams output; minimal server; minimal
TUI (transcript + composer + streaming + interrupt). Exit criterion: a live Anthropic
turn end-to-end in the Rust TUI against the Rust server, AND the Rust TUI drives the
TS server for the same flow.

| # | module | disposition | verification gate |
|---|---|---|---|
| 1.1 | core/errors (`BoughError` taxonomy) | port | unit: every variant's `status()`/`name()`; messages verbatim vs root.md §2 |
| 1.2 | core/paths (accessors + `confine`) | port | port paths.test: all accept/reject cases root.md §4 incl. NUL, prefix-sibling, lexical-only symlink rules; env read per call |
| 1.3 | core/schema (parts, events, requests) | port | port parts.test freeze suite: closed Part union, unknown-key strip, 16-event enum, `PartPick` min-1; serde round-trip camelCase |
| 1.4 | core/bus (sync fan-out) | port | port bus.test: thrower isolation, live-set mid-fan-out unsubscribe, seq monotonic per instance, size→0 leak check |
| 1.5 | core/types (Db trait, ports, `Patch<T>`, AppCtx/TurnCtx) | port | compiles; `HostFns` ⇔ `HostFnName` exhaustive-match pin (harness.md §3) |
| 1.6 | db (open/migrate/sessions/messages/turns/session_state/fts/deleteMessagesFrom/busySessionIds/latestTurnStatuses/usage) | port | port db.test: same-ms `(created_at, rowid)` ordering, 3-level threadFor, migration idempotent ×3 opens, newer-version refusal, usage accumulate-vs-gauge, FK pragma on; **live-db copy opens clean** |
| 1.6a | db: 3 migrate reshapes | port | pre-reshape fixture files ALTER in place / rebuild empty per db.md §2 |
| 1.6b | db: command-history methods (recordCommand, commandTagRows, priorFailures, recentFailures, lastSuccessLike, programForMessage) | port | db.test rows 30–31 (dir-descendant scope, one-transaction insert) |
| 1.6c | db: tagSpread/tagDiversityByDay/commandsForTag/repoTagCounts | v1-stub (empty returns) | trait compiles; CLI wave un-stubs |
| 1.6d | db/embed (sqlite-vec + lembed layer) | v1-stub (`create_embed_layer` → `None`) | callers observe absence; `bough tags similar` exits 1 with the FTS pointer message |
| 1.7 | llm: routing/retry/sse/parse_tool_args/blocks_to_parts | port | port client.test + stream.test: `@cf/` before slash, isRetryable table, backoff+Retry-After, stall guard throws retryable, truncated-args vs `{}`-legal |
| 1.8 | llm: Anthropic client (raw HTTP+SSE) | port | canned-SSE tests: 3 cache breakpoints in order, thinking replayed verbatim via meta, usage normalization (input inclusive of cache); live smoke 1 turn |
| 1.9 | llm: pricing (+vendored pricing.json) | port | pricing.test: unpriced→null never 0, fresh-input clamp, catalogKeys mirrors providerFor (drift test) |
| 1.10 | llm: complete_text | port | unit with scripted client |
| 1.11 | llm: trace | v1-stub (`with_trace(inner, None)` = identity) | identity pin test |
| 1.12 | llm: openai/openai_compat/cloudflare clients, discovery | v1-stub (trait-shaped "provider not configured" LlmError 401) | routing still resolves; wave 2 ports openai_compat |
| 1.13 | harness/protocol + preflight | port | HOST_FN_NAMES 18-name pin; unterminated-string scanner cases; shadow-check messages verbatim |
| 1.14 | harness/vm.rs + js/vm_worker.js sidecar | port | port vm.test against the REAL sidecar: stream==batch logs, timeout-vs-interrupt texts, abort handshake kills grandchild (marker-file test), 9 shell doors redirect to `bash(cmd, tags)`, direct binary spawn still works, `require("node:path")` works, capability-denial text |
| 1.15 | hostfn/patch (pure engine) | port FIRST in hostfn | port the full patch.test suite (~90 cases): grammar, viewed coordinates, rebase interior-line rule, all-or-none, CRLF/BOM, FNV tag over UTF-16 units |
| 1.16 | hostfn/spill | port | streamed-sink tests: the chunk that opened the sink is in the file, true-total marker, write-failure degrades to truncateMiddle |
| 1.17 | hostfn/files (view/patch/write + SnapshotStore) | port | resolved-path snapshot key, `[path#]` empty-tag contract, alias check, 2MiB/dir/NUL refusals, write-records-own-snapshot |
| 1.18 | hostfn/jobs (JobRegistry, signal tree, buffers) | port | kill-tree grandchild test, auto-background never kills + ignores cap, delta reads via readTo, detached process group (no /dev/tty), killAll on shutdown |
| 1.19 | hostfn/shell (bash/sh + bridge) | port | tags REQUIRED at bridge with teaching error, sh never throws on non-zero, 60s auto-background handoff note, interrupt drain + ProgramError text, exits pushed before return |
| 1.20 | turn/replay | port | four invariants: signed-reasoning gate by model, ask-as-text after results, synthetic `(interrupted)` tool_result, empty-message elision |
| 1.21 | turn/queue (TurnRegistry, classify, abortableDelay) | port | identity-checked end, hook snapshot cascade, hasUnansweredInput own-messages-only, truncation retries now / outage waits 60s, 4-throws-3-calls ring test |
| 1.22 | turn/state (checkpoints + boot recovery) | port | orphan recovery ordering (row→message→events→hook), idempotent second pass, ORPHAN_NOTE verbatim |
| 1.23 | turn/runner (the drive loop) | port | the ending state machine end-to-end with scripted client: stop/sentinel/nudges/forceText never persisted, saidSomething after-last-tool-call, overflow refusal before send, interrupt transcript shape (`⏹ Stopped.`), one message.finished per turn, drain-once |
| 1.24 | prompt/assemble + sections/*.md + project.rs (AGENTS.md) | port | section table renders; workspaceNote/scratchNote order; AGENTS.md re-read per turn; missing section file is fatal at boot |
| 1.25 | scratch (ensure; sweep) | port ensure / v1-stub sweep | `$BOUGH_SCRATCH` exported to shells; sweep no-op pin |
| 1.26 | server: http/app/error dispatch | port | route-table tests: first-match, 405+allow, 404 text, one catch maps every HttpError status, root pointer |
| 1.27 | server: /events SSE | port | no `id:` ever, `: connected`/`: ping`, filter passes global events, N connect/disconnect cycles leave bus.size==0, teardown idempotent |
| 1.28 | server: sessions routes (list/create/get/patch/postMessage 202/draft/usage) + model-settings + defaults.rs | port | sessions.test: collapsed kinds hidden top-level, stored-row announce, queued computed before start, draft emits nothing, 202/201 codes |
| 1.29 | server: interrupt route | port | idle→`interrupted:false` 200, never waits, double-tap safe |
| 1.30 | server: boot.rs (wave-1 subset of §6 order) | port | boot against live-db copy: orphans recovered before bind, loopback-only bind |
| 1.31 | server: jobs/questions/theme GET/models(static)/fs/search/changes/history-ops/workflows/mcp/schedules/artifacts/attachments/skills/ghost routes | v1-stub (honest answers per server.md §8) | every route answers; status codes right; TS TUI boots against Rust server without crashing |
| 1.32 | tui: api.rs + args.rs | port | ApiError/OfflineError texts, exit 2 preflight, unknown-flag usage error |
| 1.33 | tui: events.rs (SSE client) | port | parseFrames pure tests; reconnect loop never resumes; bad frames skipped |
| 1.34 | tui: ansi.rs + format.rs (width/wrap/truncate/spans/md-basic/busyLine/meterLine) | port | span concat == stripped text; OSC 8 zero-width; meterLine degradation ladder |
| 1.35 | tui: store (state/reduce/shell/selectors) | port | the dedupe/watermark/merge trio tests, retention bounds, queue + turn meter, exhaustive event match (no default arm) |
| 1.36 | tui: keys.rs (chat mode, line editing, chunkInput/stripCtl, esc-unwind) | port | trailing-`\r` sends only, bare `\n`=^j, stripCtl whole-sequence, escape unwind order, ^c two-row quit arm |
| 1.37 | tui: lines.rs (messages, tool folds, live toolLogs, geometry) | port | live lines replaced by result, caps not lifted by expand-all, visibleSlice/lineAtSlot bottom-hang math |
| 1.38 | tui: term.rs (caps + title + enter/leave) + input.rs (crossterm gaps) | port | caps pure-fn tests; leaveTui idempotent on every exit path incl. panic hook |
| 1.39 | tui: components app/chat/composer/status + main loop | port | shell-use smoke: type → stream → interrupt → scroll against BOTH servers |
| 1.40 | bin: `bough` dispatch (tui + start) | port | `bough start` + `bough` end-to-end on scratch BOUGH_HOME |

Wave-1 explicit stubs carried: cheap tier `None` everywhere; theme = FALLBACK palette
(GET /theme serves `{theme:null, defaults}` statically); mouse = wheel-scroll only;
no image paste; ghost `{ghost:null}`.

---

## Wave 2 — daily driver

Goal: subagents, background jobs surfaced, ask(), history fork/unsend, changes rail,
search, schedules, `bough exec`, full panel skeleton. Exit criterion: the user can
daily-drive the Rust build; `bough exec` passes the TS bench harness smoke.

| # | module | disposition | verification gate |
|---|---|---|---|
| 2.1 | agents/caps (SpawnCaps, leases, treeRootOf) | port | 12-launch allSettled tests both shapes; refusal charges nothing; Mutex-atomic reserve under real concurrency; bus backstop + per-turn GC |
| 2.2 | agents/subagent (launch, buildResult, naming) | port | isolation test both directions (spawner sentinel never in child LlmParams), 4-status matrix, timeout appends reason, depth cap /depth limit \(2\)/ |
| 2.3 | agents/notes (wake rule, formatters) | port | note format verbatim (TUI parses it); burst→one drain; interrupted stays stopped; sync-throw→queued arm |
| 2.4 | hostfn/delegate (tiers, agent/spawn/join/adopt, DetachedSubagents) | port | tier grant == bridged set; cascade reaches blocking not detached; explicit stop reaches detached; adopt lineage check + canned string |
| 2.5 | hostfn/ask + server questions routes | port | buffered-parts flush on message.finished/turn.finished, decline rejection text `user declined to answer:`, settled-race 409, reconnect via GET /questions |
| 2.6 | hostfn/state (lineage root + verbs) | port | fork/compaction/subagent share one store; 16KB refuse-not-truncate; 201st key refused, overwrite-at-cap works; unset get = `null` |
| 2.7 | hostfn/artifact + server artifacts routes | port | traversal 403, per-segment decode, session-scoped by construction, listing survives DB reset (comment-widget injection may lag to wave 3) |
| 2.8 | schedules (ticker/fire/report-back) + hostfn/schedule + routes | port | missed-N-slots-fires-once, advance-before-fire, daily@ DST local math, report-back note text verbatim, sessionId never from the wire |
| 2.9 | history/tags: record + hygiene | port | normalize/reference grammar tests, attribution dir-scan caps, hygiene snap-then-drop + never-untag, one-transaction insert |
| 2.10 | history/tags: stats (priming note + dir hints) | port | ACT-R weights pinned, references never rank, session-frozen memo (512 clear-wholesale) |
| 2.11 | history/tags: echo (note + guard) | port | guard fires at exactly 3-in-2min-same-session; signature path catches the 100-distinct-commands incident; success-prefix LIKE-escape |
| 2.12 | turn/runner: wire tag memory + echo + dir hints + exit notes | port | runner integration: tags note in volatile tier, hints on round RESULT not prompt, withExitNotes dedup |
| 2.13 | history/ops: seed (Seeder/picks) + fork + unsend | port | source byte-identical after fork; same-ms seed ordering; unsend narrow rules + FTS cleanup; validation before open_branch |
| 2.14 | vcs/repodiff + server changes routes | port | non-repo degrades not fails; noise filter; `paths: []` 400 vs absent=all; revert posts no-wake note |
| 2.15 | server/search (FTS + SearchSafeDb) | port | quoted-retry rewrite reported, 503 vs 400 discrimination, degraded-index counter heals on reindex, wrapper delegates all-but-one |
| 2.16 | llm/openai_compat (OpenRouter + Cloudflare) + MODELS/mergeModels | port | repair pass for orphan tool_calls, fragment accumulator by index, terminal error chunk, `[DONE]` truncation guard |
| 2.17 | server/models (static + TTL/deadline catalog), fs.rs, attachments, theme PUT/DELETE, skills list | port | 2.5s deadline race, git-ls-files listings, 5MB/4-type attachment gate, theme validate-on-write forgive-on-read |
| 2.18 | skills discovery (core) | port | SKILL.md walk incl. broken-listed-never-omitted; /name matching in lines.rs |
| 2.19 | worker/cheap tier (titles, ghost, activity) | port (small) | never rejects; one in-flight per session; readers degrade when absent |
| 2.20 | tui: forest.rs + panel chrome + tree tab + changes tab + rail + job view + help | port | forest walk cycle-guarded, busyBelow, two-press revert idiom, rail rows one-screen-row, help generated from bindings |
| 2.21 | tui: ask card, take-back window, unsend, notice/marks, background toast | port | 3s UNSEND_MS window; take-back outranks stop inside window; marks survive switches |
| 2.22 | tui: fuzzy completion + @-file trigger + slash dispatch | port | slash at SEND time; unknown `/word` intercepted; browsePrefix rules |
| 2.23 | cli/exec | port | full exec contract (cli.md): stream-before-post ordering, interrupt-on-timeout, ask-decline, retry-reset, `--json` envelope; TS bench harness smoke passes |
| 2.24 | tui: theme.rs (palette/apply/presets/preview) | port | preview repaints, cancel restores baseline byte-for-byte, Default persists as DELETE |
| 2.25 | mouse selection/copy (selection.rs + clicks) | port | single-row exact span; multi-row src substitution only edge-to-edge; panel border strip |
| 2.26 | image paste + attachments upload path | v1-stub → port late in wave | clipboardImagePath pure tests; upload 201 round-trip (arboard preferred over swiftc helper) |

---

## Wave 3 — mcp, workflows, harness code-mode extras, small modules

Goal: full parity minus explicitly-dropped items. Exit criterion: TS TUI and Rust TUI
indistinguishable against the Rust server for mcp/workflow flows; `bough tags`
answers; cutover checklist (live `~/.bough` on the Rust server) green.

| # | module | disposition | verification gate |
|---|---|---|---|
| 3.1 | mcp/config (registry, grants, TTL, expandEnv, childEnv) | port | fail-closed corrupt registry, `${VAR}` missing throws, expired grant filtered, never cached (re-read per call) |
| 3.2 | mcp/client (stdio, no-hang contract) + killAllMcpServers | port | every timeout path in ms-scale tests; server-initiated requests refused; exit fails all in-flight |
| 3.3 | mcp/manager + status + server mcp routes | port | grant inheritance enum (Live→Inherited at spawn), require_granted 403 wording, four-key status doc, per-(session,server) stdio vs shared remote |
| 3.4 | mcp/remote (Streamable HTTP) | port (after 3.1–3.3) | 401-as-question, bounded requests, 401 remembered on failed auth |
| 3.5 | mcp/oauth flow + keychain | port last in mcp | TokenStore perms 0700/0600, state round-trip; until then beginAuth = 502 "not yet ported" with TokenStore real (existing token files keep working) |
| 3.6 | mcp/service reconcile + cli/mcp + cli/sync-mcp | port | diagnose table pure tests; sync-mcp credential-safety suite verbatim |
| 3.7 | workflow/pos + key + meta | port | comparePos numeric, callKey UTF-16 double-FNV, meta scanner cannot execute |
| 3.8 | harness/wf.rs + js/wf_worker.js | port | determinism traps, stage-major coordinates probe test, param-list drift pin |
| 3.9 | workflow/engine + runner + replay + journal_fs | port | journal-before-semaphore, pause gates admission, prefix replay stops at first change, only-success replays, mirror-preferred rerun |
| 3.10 | workflow/control + relaunch + report + server workflow routes | port | stop kills worker AND interrupts subagent turns; replayed+ranLive+pending==total; 409 pause when not live |
| 3.11 | workflow/structured (schema retries) | v1-stub → port | pass-through accepted first (keys already hash schema opaquely); then retry-then-fail |
| 3.12 | workflow/saved + routes | port | name confined to saved/ dir; PUT strict body |
| 3.13 | history/ops: compact + explore scout + extract + move_into + handoff + sections | port | source byte-identical; one summary per run; scout every-failure→None; move is a copy; sections partition [0,n) |
| 3.14 | server: comments + widget injection | port | sidecar outside artifacts dir; one batch one turn; widget interpolation-free |
| 3.15 | llm: OpenAI Responses client + discovery (all four) | port | reasoning-item-precedes-function_call, meta-less reasoning dropped, byNewest natural sort |
| 3.16 | llm/trace (un-stub) | port | n counts failed attempts; prefix sha emitted once; all fs errors swallowed |
| 3.17 | db/embed + history/tags/embed (un-stub) | port | count-delta drain (never `changes`), probed dims, model-change rebuild, fixture: docker-exec KNN hit |
| 3.18 | cli/tags (list/show/stats/sql/similar) | port | read-only sql gate (query_only + keyword + 200-row cap), exit codes 0/1/2 |
| 3.19 | logs/ + cli/patterns | **PORTED** (`bough-core::logs`, `crates/bough/src/patterns.rs`) | 141 tests; gated by byte-identical `--json/--llm/--human` vs the TS on 31 fixture/flag combinations incl. a 400k-line log |
| 3.20 | tui: workflows tab (all levels) + mcp tab + skills tab + model tab | port | replay-accounting rows present once detail view exists; disjoint tab-letter sets verified by deadBindings |
| 3.21 | tui: sections/topic headers, search-in-tree, ghost text, activity blurbs, urlAcross/click-open, tab tint/progress/notifications, tmux/zellij renames | port (cosmetic tail) | per-feature TS test ports; all degrade silently when absent |
| 3.22 | scratch sweep + embeddings drain ticker + MCP grant promotion (no-op) | port / v1-stub(promotion no-op) | sweep boundary tests; drain pump on server tick |

**Drop (never port):** `lsp.*`, `canvas()`, acceptance/CHECK gate, web UI,
`worker()`/ladder, `history` host-fn verb, TS import-cycle workarounds, Homebrew
SQLite swap, `source:"backfill"` writer (keep the column).

---

## Cross-wave gates

- **G1 (end of wave 1):** `cargo test` green; Rust TUI drives a full turn against the
  TS server AND the Rust server; TS TUI boots against the Rust server (stub routes
  answer honestly). Live-db copy opens under Rust migrate.
- **G2 (end of wave 2):** `bough exec` passes the bench-harness smoke; a day of real
  driving on a scratch BOUGH_HOME; subagent fan-out + interrupt cascade verified live.
- **G3 (end of wave 3):** workflow rerun replays a prefix on a real journal written by
  the TS engine (journal format compatibility); `bough mcp list` matches TS output on
  the user's real registry; cutover checklist: point Rust server at the live
  `~/.bough` (backup first), run for a session, diff nothing corrupted.
