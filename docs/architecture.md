# bough — Architecture (binding)

This document is the binding design for the system. Per-module contracts live in
`specs/*.md` (16 files) — this document decides the workspace shape, the crate
boundaries, the shared-types strategy and the concurrency model. Where this document
and a spec disagree on structure, this document wins; where they disagree on
*behavior*, the spec wins.

It was written as the design for the Rust rewrite of a TypeScript system, and still
refers to that system as the reference for behavior. That tree is gone; what it pinned
now lives in `crates/` and in the cargo test suite.

## 0. The parity anchor

**The server/TUI split over loopback HTTP + SSE is load-bearing.** Fixed routes, JSON
field names, status codes (202 for postMessage, 201 for creates) and SSE framing
(`event:` + single `data:` line, no `id:` field, `: connected` / `: ping` comments).
`BOUGH_PORT` (default 4321, loopback only, no CORS ever) and `BOUGH_HOME` (default
`~/.bough`) let a dev instance run beside the live install. The full route table is
`specs/server.md` §3 — that file IS the API contract.

## 1. Cargo workspace layout

Four crates, dependency order top to bottom:

```
bough/
  Cargo.toml            # [workspace] members + [workspace.dependencies]
  crates/
    bough-core/         # lib: everything that is not HTTP and not a terminal
      src/
        schema/         # parts.rs, events.rs, requests.rs — THE shared wire types
        errors.rs       # BoughError taxonomy (status + name + verbatim messages)
        paths.rs        # boughHome/dbPath/…/confine (lexical, never canonicalize)
        bus.rs          # sync fan-out Bus (see §5)
        types.rs        # ports: Db trait, BusPort, LlmClient, CheapTier, Clock,
                        #   AppCtx, TurnCtx, HostFns, Patch<T>
        db/             # schema.sql (include_str!), migrate.rs, sqlite_db.rs,
                        #   extensions.rs, embed.rs (v1: stub)
        llm/            # routing, anthropic, openai, openai_compat, sse, retry,
                        #   pricing (+pricing.json), trace (v1: stub), discovery
        harness/        # protocol.rs, preflight.rs, vm.rs, wf.rs + js/ sidecar
        hostfn/         # patch, files, spill, shell, jobs, ask, state, delegate,
                        #   artifact, schedule + HostState registries
        turn/           # runner, queue, replay, state
        agents/         # caps, subagent, notes
        history/        # tags/{record,hygiene,stats,echo,embed}, ops/{seed,fork,
                        #   unsend,compact,extract,move_into,handoff,sections,explore}
        workflow/       # pos, key, meta, replay, engine, runner, control,
                        #   structured, journal_fs, relaunch, report, saved
        mcp/            # config, client(stdio), manager, status, remote, oauth, keychain
        prompt/         # assemble.rs + sections/*.md (include_str!), project.rs
        vcs/            # repodiff (Changes rail git layer)
        skills/         # SKILL.md discovery
        worker/         # cheap tier: titles, ghost, activity (v1: None)
        schedules.rs    # ticker + fire + report-back
        scratch.rs      # ensure + sweep
        logs/           # `bough patterns` pipeline (v1: absent)
    bough-server/       # lib: axum HTTP + SSE — the ONLY crate that speaks HTTP-server
      src/              # app.rs (router), http.rs, events.rs (SSE), one module per
                        # route family (sessions, turns, questions, jobs, artifacts,
                        # comments, changes, search, fs, models, skills, theme,
                        # defaults, attachments, workflows, mcp_routes, schedules),
                        # boot.rs (the composed main wiring)
    bough-tui/          # lib: ratatui client — speaks ONLY loopback HTTP+SSE
      src/              # api.rs, args.rs, events.rs, store/{state,reduce,shell,
                        # selectors}, forest.rs, ansi.rs, format.rs, keys.rs,
                        # lines.rs, selection.rs, paste.rs, clipboard.rs, term.rs,
                        # input.rs, theme.rs, components/, app.rs (event loop)
    bough/              # bin: subcommand dispatch
      src/main.rs       # bare `bough` → TUI; `start` → server; `exec`, `mcp`,
                        # `tags`, `sync-mcp` (later), `patterns` (stub exit 2)
```

Crate dependency DAG (arrows = depends-on):

```
bough (bin) ──→ bough-server ──→ bough-core
      └───────→ bough-tui ─────→ bough-core   (schema/errors modules ONLY — see rule)
```

**Rules the DAG enforces (module-boundary invariants from the TS tree):**

1. `bough-core::hostfn` / `turn` / `history` / `agents` **never** reference
   `bough-server` — they throw `BoughError`; only the server crate converts errors to
   responses. The crate boundary makes the TS lint rule ("hostfn never imports server")
   structural.
2. `bough-tui` may use only `bough_core::{schema, errors, types::Effort/UsageTotals}` —
   it is a wire client, not a domain participant. It must never link the Db or LLM
   paths. Enforced by review + a `#[cfg(test)]` import-list test; we accept one crate
   rather than a fifth `bough-types` crate because the workspace builds as one unit and
   the discipline is cheap.
3. No raw SQL outside `bough-core::db`. No provider name outside `bough-core::llm`.
   No URL string outside `bough-tui::api` (client side) / `bough-server::app` (routes).

Why not more crates: the TS system is one package; the seams that matter are module
seams, not compilation units. Four crates gives us the two hard walls (server-only
HTTP, client-only TUI) and keeps `cargo test` / incremental builds simple. Splitting
`bough-core` further buys nothing until compile times prove otherwise.

## 2. Crate choices (workspace dependencies, pinned)

```toml
[workspace.dependencies]
tokio        = { version = "1.47", features = ["full"] }
tokio-util   = { version = "0.7", features = ["rt"] }   # CancellationToken
axum         = { version = "0.8" }                       # bough-server only
reqwest      = { version = "0.12", features = ["json", "stream"] }
rusqlite     = { version = "0.37", features = ["bundled", "load_extension"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
ratatui      = "0.29"                                    # bough-tui only
crossterm    = { version = "0.29", features = ["event-stream"] }
termimad     = "0.35"                                    # GH table layout (bough-tui)
uuid         = { version = "1", features = ["v4"] }
thiserror    = "2"
chrono       = "0.4"                                     # daily@ local-time math (DST)
regex        = "1"
sha2         = "0.10"                                    # sectionSha, callKey inputs
base64       = "0.22"                                    # attachments, OSC 52
unicode-width= "0.2"                                     # display columns (bough-tui)
futures      = "0.3"
async-trait  = "0.1"
dirs         = "6"                                       # home_dir
nix          = { version = "0.29", features = ["signal", "process"] }  # kill/killpg
tracing      = "0.1"
tracing-subscriber = "0.3"
libc         = "0.2"
```

Minor-version drift is fine; major-version bumps need a note in this file. Notes:

- **rusqlite `bundled`**: compiles its own SQLite with FTS5 and extension loading —
  the whole `Database.setCustomSQLite` Homebrew dance from `db/extensions.ts`
  disappears. Keep the `BOUGH_NO_EMBED` gate and the once-per-process decision
  (`OnceLock<bool>`).
- **No Anthropic SDK.** The Anthropic client is hand-rolled reqwest + SSE per
  `specs/llm.md` §3a (that spec exists so the SDK isn't needed). The SSE parser stays
  hand-rolled (~40 lines) — the `[DONE]`/stall/trailing-fragment semantics are custom
  and test-pinned; do not substitute `eventsource-stream`.
- **No clap** for `bough exec`/TUI args — the grammars are tiny and USAGE text is
  product surface, ported verbatim.
- **termimad for GH tables only.** A table is the one markdown block whose layout is
  real arithmetic — balancing columns against the width, wrapping inside a cell,
  honoring `:---:` — so `format::md` hands the gathered block to `termimad`
  (`FmtText::from`, which reads no terminal) with a skin built from bough's palette,
  and keeps everything else. It is **not** the markdown renderer: fed a whole message
  it loses OSC 8 links, the fence highlighting, and the heading style, all of which
  `md` already does better. It pulls `minimad`/`coolor`/`crokey` and a second
  `unicode-width` (0.1); the alternative was hand-rolled column arithmetic.
- **ANSI handling** (`string-width`/`slice-ansi`/`wrap-ansi`): no drop-in crates.
  Port `ansiSpans` first in `bough-tui::ansi` and do width/truncate/wrap/slice **over
  parsed spans**; OSC 8 links are zero-width in all of them. `ansi.rs` is also the
  bridge to ratatui (`Vec<AnsiSpan> -> Line<'_>`).
- **sqlite-vec / sqlite-lembed (embeddings)**: **ported (row 3.17)**, in
  `bough-core::db::embed` with the drain pump in `history::tags::embed`.
  `sqlite-vec` is the crate, **statically registered** via `sqlite3_auto_extension` —
  no dylib, no install step. **lembed is dylib-loaded** from `$BOUGH_LEMBED_PATH`, else
  `~/.bough/lib/lembed0.{dylib,so,dll}` (copy it out of the npm
  `sqlite-lembed-<os>-<arch>` package, or build asg017/sqlite-lembed). `fastembed` was
  the alternative and is NOT used: it is a different embedding pipeline from the one the
  TS install has been filling `~/.bough/embeddings.db` with, so the model-id check would
  throw that store away — the dylib keeps the same GGUF and the same SQL, and cutover is
  a no-op (verified: same `embed_meta` model id, no rebuild, identical KNN rows and
  distances as `bough tags similar` in TS). Separate `embeddings.db` + ATTACH +
  count-delta drain + probed dims + model-id rebuild all kept. Absent lembed or
  `BOUGH_NO_EMBED` → `create_embed_layer()` returns `None`; graceful absence is the
  documented contract, tags + FTS carry recall alone, and `bough tags similar` exits 1
  with the existing message.
- **Sidecar JS runtime**: Bun if present on PATH, else Node ≥ 20 (see §4). Not a
  Cargo dependency; the two worker scripts ship via `include_str!` and are written to
  `~/.bough/bin/` (or a cache dir) at first use.

## 3. Shared-types strategy

**Every JSON wire shape and every DB row type is defined ONCE, in
`bough-core::schema`, serde-derived, field names matching the TS wire format exactly.**
Both `bough-server` and `bough-tui` import them; there is no second declaration
anywhere. This is what makes the cross-runtime parity tests (§0) meaningful.

Rules (each maps to a pinned TS behavior — see `specs/db.md` §2/§4):

- `#[serde(rename_all = "camelCase")]` on every wire struct. DB columns are
  snake_case; the row→domain mappers in `bough-core::db::sqlite_db` are the ONLY
  translation point.
- `Part` is `#[serde(tag = "type", rename_all = "snake_case")] enum Part` — the closed
  7-variant union (text, reasoning, tool_call, tool_result, image, ask, workflow).
  Note the asymmetry: persisted parts use `callId`/`output`; LLM wire blocks use
  `toolUseId`/`content`. Two types, never unified.
- `EventType` is a closed 16-name enum (`#[serde(rename = "message.delta")]`-style).
  The TUI reducer matches exhaustively with **no default arm** — a new event type must
  be a compile error. Envelope `BoughEvent { r#type, session_id: Option<String>, seq: u64,
  ts: i64, data: serde_json::Value }` is parsed at the socket; payloads are typed via a
  per-type `EventData` mapping.
- Optionals: `Option<T>` + `#[serde(skip_serializing_if = "Option::is_none")]` for
  omit-when-absent fields (`costUsd`, `tokens`, `lastTurnStatus`, `originId`, `error`,
  …). `Session` parsing **strips** unknown keys — `deny_unknown_fields` is WRONG here
  (freeze test: `archivedAt` must not survive parsing, but must not reject either).
- PATCH bodies need tri-state absent/null/value. Canonical type, in `types.rs`:

  ```rust
  #[derive(Clone, Debug, Default)]
  pub enum Patch<T> { #[default] Keep, Clear, Set(T) }
  ```

  with a serde adapter (double-Option deserialize). Used by `PatchSessionBody`,
  `PatchScheduleBody`, `TurnPatch.error`, `WorkflowPatch`, `WorkflowAgentPatch` —
  everywhere TS does key-membership merges (`"error" in patch`).
- Zod validation becomes explicit `validate()` fns / `TryFrom` at the router edge
  returning `BoughError::BadRequest` with the same `{error}` envelope; clients only
  read `{error}`, so the message shape is loose but the status codes are not.
- `pricing.json` and `db/schema.sql` are vendored verbatim and `include_str!`-ed.

Error taxonomy (`errors.rs`): one `enum BoughError` (thiserror) with `status() -> u16`
and `name() -> &str`. Variants carrying distinct data get their own arm
(`Llm { status, retry_after_ms, message }`, `SpawnCap`, `ContextOverflow`); the
caller-status families (Turn/Agent/Workflow/Branch/Changes/Schedule/State/Artifact/
Mcp/Net/Skill…) are `Http { status, kind, message }` with a `kind` enum. **Every
constructor-site message string in TS is model-facing product surface: port them
verbatim** — tests grep substrings and the model's behavior is trained on them.

## 4. Bun-specific pieces → Rust

### 4.1 Code-mode JS harness: sidecar subprocess, NOT an embedded engine

**Decision: keep `vm_worker.ts` and `wf_worker.ts` as JavaScript, run them in a
sidecar JS runtime process (Bun if on PATH, else Node ≥ 20), speaking the existing
worker protocol as NDJSON over stdin/stdout. Only the host side (`harness/vm.rs`,
`harness/wf.rs`) becomes Rust.** rquickjs/Boa/deno_core are rejected.

Justification (this is the single most consequential porting decision,
`specs/harness.md` says so up front):

- The program worker executes *model-written JS with full user authority*: `npm:`
  imports, `node:*` builtins, a **real** `require` (weak models write CommonJS — a
  stub caused measured abandonment of code-mode), platform `fetch`, real subprocess
  spawning with pids the interrupt sweep can kill. No embeddable Rust engine provides
  that; deno_core still doesn't give free npm resolution and drags a V8 build.
- The worker-side behaviors are *monkey-patching* (`process.exit` trap, child-process
  tracking, the 9 shell doors, `Bun.$` removal, determinism traps via Proxy,
  AsyncLocalStorage structural coordinates) — not portable to Rust and must not be
  attempted.
- A sidecar reuses the two worker scripts nearly verbatim (postMessage → stdout
  NDJSON, onmessage → readline on stdin) and keeps the protocol (`specs/harness.md`
  §3) byte-identical, so the ~600 lines of lifecycle tests port 1:1 against a real
  worker, as the TS suite insists.

Host-side mechanics (`vm.rs`):

- Spawn sidecar with `process_group(0)`, stdin/stdout piped, stderr captured for the
  `worker error:` path. One writer task owns stdin (mpsc). The read loop
  `tokio::select!`s over {stdout line, CancellationToken, wall-clock sleep}.
- **Host calls are `tokio::spawn`ed** and post results back through the writer mpsc —
  never process the message loop serially through an awaited host call (the JS event
  loop gave re-entrancy for free; a serial Rust loop would block `log` lines behind a
  slow `bash`).
- Wind-down handshake exactly as TS: send `{"type":"abort"}`, wait ≤ `ABORT_GRACE_MS`
  (1s) for `{"type":"aborted"}`, then `killpg`. Pre-flight (`check_program_syntax`) is
  delegated to the sidecar via a `check` message before `run` — this guarantees engine
  parity for the shadow/unterminated-string error messages (option (b) in the spec).
- `ProgramResult`, timeout-vs-interrupt message texts, capability-denial text, the
  `HOST_FN_NAMES` closed list: verbatim from `protocol.rs`. A probe test runs a real
  program printing `typeof` of every bound name — that test is what keeps the Rust
  list and the JS list from drifting (replaces the TS shared-import invariant).
- Node sidecar: `Bun.*` doors reduce to the `child_process` patches; the
  direct-binary-spawn test switches accordingly. Runtime choice is made once at boot
  and logged.

### 4.2 Workflow script execution

Same sidecar architecture: `wf.rs` spawns `wf_worker.js` (determinism traps,
stage-major structural coordinates, combinators — all stay JS). The Rust engine
(`workflow/engine.rs`) owns the journal, keys, semaphore, pause gate, and prefix
replay. `WORKFLOW_PROGRAM_PARAMS` is duplicated Rust-side by design (importing a
worker into the host would evaluate the `Date.now` traps in the server process);
a probe test pins the two lists equal. One sidecar process per run; the prefix
decision + journal-row insert happen in one non-await section on the run's message-
loop task (the TS synchronous-decision guarantee).

### 4.3 The 3B worker (cheap tier)

The TS "worker" (titles, ghost text, activity blurbs) is not a Bun Worker — it is
`CheapTier`, three methods that each resolve `Option` and never error, one in-flight
blurb per session, drop-don't-queue. Rust: a plain `struct CheapTierImpl` over
`complete_text` with a per-session `Mutex<HashSet<SessionId>>` in-flight guard.
**v1 ships `cheap: None`** — every reader degrades on absence by contract.

### 4.4 Everything else Bun

| Bun | Rust |
|---|---|
| `Bun.serve` (idleTimeout 0) | axum on `TcpListener::bind("127.0.0.1:port")`, **no** read/idle timeout middleware (SSE idles between turns; `bough exec` holds one request for a whole turn) |
| `Bun.spawn` detached shells | `tokio::process::Command` + `process_group(0)`, stdin null, pipes pumped by two tasks; pids captured before wait; `ps -Ao pid=,ppid=` parse for `descendant_pids` (the proven macOS path) |
| `bun:sqlite` (sync) | rusqlite, sync, behind the Db seam (§5) |
| `URLPattern` routing | axum router (`/{id}`, `/{*path}`); the only order-sensitive overlap (`/saved-workflows` vs `/workflows/:id`) is statically disambiguated; percent-decode **per segment** for artifacts |
| `AbortSignal` | `tokio_util::sync::CancellationToken` (child tokens = cascade) |
| Bun single-thread atomicity | explicit `Mutex` around check+take sections (SpawnCaps.reserve, TurnRegistry.begin, workflow admit) — this does NOT come free in Rust and each site is called out in the specs |
| module-static registries | one explicit `HostState { jobs, snapshots, writes, asks, detached, caps }` built at boot, `Arc`-cloned into each turn — the TS statics existed only because `TurnCtx` was frozen |

## 5. Concurrency model

**Runtime:** one tokio multi-thread runtime for the server; the TUI runs its own
small runtime (single event loop task + SSE task + timer tasks).

**Bus (`bough-core::bus`): hand-rolled synchronous fan-out, NOT `tokio::broadcast`.**
`tokio::broadcast` is rejected because it is async-delivered (violates the
synchronous, in-seq-order contract), drops on lag, and cannot express "a listener
unsubscribed mid-fan-out does not receive the in-flight event" — all three are
test-pinned. Shape:

```rust
pub struct Bus {
    seq: Mutex<u64>,
    listeners: Mutex<HashMap<u64, Arc<dyn Fn(&BoughEvent) + Send + Sync>>>,
    on_listener_error: ...,  // injectable
}
```

`publish` stamps `{seq, ts}` into a fresh event, iterates listener ids in insertion
order re-checking membership per call (live-set semantics), wraps each call in
`catch_unwind` (one bad subscriber never silences the rest), returns the stamped
event. `size()` backs the SSE leak test. Bus is display transport, never storage:
**persist first, then publish**, everywhere. SSE handlers subscribe with a closure
that pushes into an **unbounded mpsc** per connection; the axum SSE stream drains it;
dropping the stream unsubscribes (teardown idempotent across abort/cancel/failed
write).

**Db:** rusqlite `Connection` is `!Sync`. `SqliteDb` lives behind
`Arc<Mutex<SqliteDb>>` implementing the ~60-method `Db` trait synchronously; async
call sites either tolerate the short lock (single-user local server, contention is
negligible — the TS is fully sync here too) or wrap hot paths in `spawn_blocking`.
`PRAGMA foreign_keys = ON` at every open. The search-safe wrapper is a newtype
`SearchSafeDb<D: Db>` delegating everything except `index_message` (counts failures,
never propagates), installed on the ctx **after** boot recovery used the raw handle.

**Turns:** `TurnRegistry` = `Mutex<HashMap<SessionId, Entry>>` where `Entry` holds a
`CancellationToken` + interrupt-hook slab. `begin` claims synchronously (throws
before the placeholder message exists); `end` is identity-checked; `interrupt` aborts
the token then fires a snapshot of hooks (throwing hook swallowed). `begin_turn` does
claim + message-create + `message.started` publish **inline** (synchronous contract),
then `tokio::spawn`s the drive loop; the epilogue (registry release → drain via
`has_unanswered_input`) runs after `await` on every path including panics
(`catch_unwind` → error-path turn). The drive loop is a single sequential async fn —
tools execute one at a time by design.

**Tokio tasks replacing Bun workers/timers:**

- program/workflow sidecars: spawned processes + pump tasks (§4).
- schedule ticker: `tokio::time::interval(30s)` in a spawned task returning a
  `CancellationToken` stopper (tokio timers never hold the runtime open — unref is
  free — but `bough exec` and tests still need the stopper).
- job registry: two pump tasks per shell feeding head/tail buffers + optional spill
  sink under a small mutex; `exit` as `watch::Receiver<Option<ExitStatus>>`;
  SIGTERM→SIGKILL backstop via `tokio::time::sleep`.
- LLM stall guard: `tokio::time::timeout(60s)` around each stream chunk read.
- cheap-tier / note delivery / subagent result pipelines: fire-and-forget
  `tokio::spawn` with errors routed to the injectable `report_error` seam
  (`tracing::error!` default).
- TUI: SSE reader task + timer tasks (notice TTL 10s, usage poll 3s, spinner 120ms)
  all post `StoreAction`/`Event` over one mpsc so the reducer stays single-threaded
  and pure — the property the entire TS store test suite pins.

**Clock:** `pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>` (epoch ms), injected
everywhere the TS injects `now` (db updateTurn, schedules, stats, ask, caps…). Tests
never sleep.

## 6. Boot sequence (bough-server::boot, order is load-bearing)

1. decide sqlite extension capability (`OnceLock`) — before first open
2. open db (migrate: refuse newer `user_version`, run 3 reshapes, exec schema, stamp)
3. build Bus, HostState, AppCtx
4. `recover_orphaned_turns` + `recover_orphaned_workflows` (raw db handle) —
   before the listener binds; orphaned-subagent notes recorded, never woken
5. install `SearchSafeDb` wrapper on ctx.db
6. wire the ONE composed turn starter (skill-aware, tier-graded grants, note
   deliverer, survivingJobs) — port only the final TS composition, not its seven
   append-only supersessions
7. `sweep_scratch()` best-effort; sync workflow script mirrors
8. start schedule ticker (after the starter exists)
9. bind `127.0.0.1:$BOUGH_PORT`
10. signal handlers: SIGINT/SIGTERM → `jobs.kill_all()` + `kill_all_mcp_servers()`

## 7. Testing strategy

- Every test offline and hermetic: in-memory rusqlite, scripted fake `LlmClient`,
  fake sidecar where lifecycle isn't under test / real sidecar where it is
  (`vm.test` ports run trivial programs through the real worker — nothing mocks the
  bridge, per the TS header).
- Server handler tests via `tower::ServiceExt::oneshot` (no socket).
- TUI reducer/format/forest/lines/keys/selection tests are pure — port the ~200 TS
  cases with the code.
- Cross-runtime parity smoke (manual gate per wave): Rust TUI ↔ TS server and
  TS TUI ↔ Rust server on a scratch `BOUGH_HOME`.
- The live `~/.bough/bough.db` must open under the Rust migrate (idempotence +
  reshape tests use a copy).

## 8. Do not port

- `lsp.*` host fn, `canvas()`, the acceptance/CHECK gate, the web UI, the worker()
  ladder — all deliberately deleted in TS; the sources say the machinery is gone on
  purpose.
- The TS import-cycle workarounds (hoisted `function` handlers, `WithXxx` ctx
  extension interfaces, module-static seams) — Rust's module system and an owned
  `AppCtx`/`HostState` make them moot; fold optional seams into ctx fields.
- The Homebrew-dylib SQLite swap (rusqlite `bundled` obsoletes it).
- The stale `history` host-fn verb (removed in 50d65da0; `HOST_FN_NAMES` in
  `specs/harness.md` §2 is authoritative — 18 names).
