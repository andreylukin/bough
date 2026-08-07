# Port spec: `src/harness/` — code-mode VM workers

Files: `protocol.ts` (258 ln), `vm.ts` (404), `vm_worker.ts` (392), `wf_worker.ts` (494), `vm.test.ts` (589, contracts mined below).

**The single most important porting decision is stated up front:** the program worker
executes *model-written JavaScript* with the *full user-authority runtime* — filesystem,
network, env, subprocesses, `npm:` imports, `node:*` builtins, `require`, `fetch`
(spec §2.2, quoted below). No embeddable Rust JS engine (Boa, rquickjs/QuickJS) provides
that. **The Rust port must keep the two worker scripts in JavaScript and run them in a
sidecar JS runtime process (Bun, or Node ≥20), speaking the existing protocol over
stdio as NDJSON instead of `postMessage`.** Only the *host side* (`vm.ts` and the host
half of the workflow driver) becomes Rust. Embedding `deno_core`/V8 is a heavier
alternative that still doesn't give free `npm:` resolution; a sidecar reuses
`vm_worker.ts`/`wf_worker.ts` nearly verbatim and is the low-risk path.

---

## 1. Purpose & invariants

One "program" (a JS async-function body written by the model) runs per round, in a
fresh worker, against bridged host functions. A "workflow script" is the same shape
with a different, five-name scope, driven by the journaled workflow engine.

Quoted invariant comments (verbatim, these are the contracts):

- `protocol.ts`: *"The invariant: **host names are declared here exactly once, and both
  sides import them.** The host side dispatches on them, the worker side binds them as
  program parameters, and the pre-flight syntax check uses the same list to reject a
  program that shadows one (`let bash = 1`)."* … *"The list is CLOSED."* … *"The wire is
  string-only, both directions."* … *"Declaring a name here does not grant it. A host
  function exists in a program only when the turn bridges it AND the system prompt
  documents it."*
- `vm.ts`: *"**Nothing here is a security boundary** (spec §2.2)."* … *"THE INVARIANT
  THIS HOLDS: **a program never outlives its turn, and never takes the server with
  it.**"* (three mechanisms: pre-flight before spawn; wind-down as a handshake, not a
  terminate; partial output survives).
- `vm_worker.ts`: *"THE INVARIANT THIS HOLDS: **because the isolate is not sealed,
  everything the program can start must be stoppable from here.**"* (the exit trap;
  child-process tracking). Plus a third interception: *"SHELL-SHAPED process creation is
  shut … because a shell run that way leaves no row in the command memory that
  `bash(cmd, tags)` feeds. That is a memory boundary, not a security one."*
- `wf_worker.ts`: *"THE INVARIANT THIS HOLDS: **a workflow script is deterministic, and
  its combinators have exactly the concurrency semantics the spec states.**"* … *"every
  `agent()` call carries a STRUCTURAL COORDINATE, computed from the script's shape
  rather than from the order calls happen to reach the host."*
- `vm.test.ts` header: tests run *"through a REAL worker with trivial programs …
  Nothing here mocks `postMessage` — the things that can go wrong are ordering and
  lifecycle, and a fake bridge would prove neither."* Port the tests against the real
  sidecar the same way.

STALENESS NOTE: `protocol.ts`'s header says *"Amended 2026-08: `history` added"* but
commit 50d65da0 ("the memory has one door, and it is `bough tags`") removed it again.
The `HOST_FN_NAMES` array below is authoritative; there is **no** `history`, `image`,
`fetch`, or `recall` verb. Do not port the stale comment.

## 2. Public API

### `protocol.ts` (pure declarations — becomes a Rust module + a JS mirror, see §7)

- `HOST_FN_NAMES: readonly string[]` — the closed, ordered list:
  `bash, sh, bashBg, bashOutput, bashWait, bashKill, view, patch, write, agent, spawn,
  join, adopt, workflow, ask, state, schedule, artifact` (18 names). Order matters: it
  is the parameter-binding order.
- `type HostFnName` — union of the above.
- `PROGRAM_PARAMS = [...HOST_FN_NAMES, "console", "require"]` — the program's full
  parameter list (20 names). `console` is bound (streams + batches, §4); `require` is a
  *real* `createRequire(import.meta.url)` (weak models write CommonJS; a stub caused
  model abandonment of code-mode).
- `HOST_FN_VERBS` — verb lists for the three method-object functions:
  - `state: ["get", "set", "list", "delete"]`
  - `schedule: ["list", "add", "enable", "disable", "remove"]`
  - `workflow: ["start", "rerun", "stop", "pause", "resume", "status", "list"]`
- Message types (§3 for exact wire shapes): `RunMessage`, `HostResultMessage`,
  `AbortMessage`, `ToProgramWorker`; `HostCallMessage`, `LogMessage`, `AbortedMessage`,
  `DoneMessage`, `ProgramErrorMessage`, `FromProgramWorker`; `ProgramResult`.
- Workflow side: `WORKFLOW_HOST_FN_NAMES = ["agent", "phase", "log"]` (`parallel`/
  `pipeline` are pure worker-side combinators, never on the wire);
  `WORKFLOW_SCRIPT_PARAMS = [...WORKFLOW_HOST_FN_NAMES, "args"]`;
  `WorkflowRunMessage`, `ToWorkflowWorker`, `WorkflowHostCallMessage` (adds `pos?`),
  `WorkflowDoneMessage`, `FromWorkflowWorker`.

### `vm.ts` (host side — becomes Rust)

- `DEFAULT_TIMEOUT_MS = 180_000` — wall-clock ceiling per program ("a liveness limit,
  not a resource limit").
- `ABORT_GRACE_MS = 1_000` — how long a stopping worker gets to sweep its children
  before termination regardless.
- `unterminatedString(src) -> Option<{line, col, text, quote}>` — hand-rolled scanner
  locating a `"`/`'` string closed by a raw newline. Skips `//` and `/* */` comments and
  template literals (with `${}` nesting depth so their newlines are legal). THE failure
  mode for model code assembled inside an outer template literal (`\n` consumed by the
  outer literal). 1-indexed line/col; `text` is the full source line.
- `checkProgramSyntax(code) -> Option<String>` — pre-flight parse with the *same*
  parameter list the worker binds (`new AsyncFunction(...PROGRAM_PARAMS, code)`);
  returns the model-facing error message or `null` if it parses. Three message shapes:
  1. Shadowed bound name → carries the engine's words plus:
     `` `X` is already bound in every program's scope, so declaring it shadows the
     binding. Rename your variable (`myX`) and call `X` as it is.`` (Detects both JSC
     "Cannot declare a … twice: 'x'" and V8 "Identifier 'x' has already been declared";
     only fires when the name is in `PROGRAM_PARAMS` — shadowing anything else is fine.)
  2. Unterminated-string hit → `program does not parse: <why> — line N: a
     double/single-quoted string is closed by a real newline.` + the clipped (90 chars)
     source line + the escaping advice ("write \\n (escaped) … a bare \n is consumed by
     the outer literal").
  3. Otherwise → `program does not parse: <why>`.
  Non-SyntaxError from the constructor is re-thrown (impossible in practice).
- `RunProgramOptions { code, host: HostFns, timeoutMs?, signal?, onLog? }` — an options
  struct on purpose (five positionals grew a bug).
- `runProgram(opts) -> Promise<ProgramResult>` — **never rejects**; throw/timeout/
  interrupt are all ordinary `ok:false` results.

### `vm_worker.ts` (worker side — stays JS, runs in the sidecar)

No exports; it *is* the worker. Behaviors it implements: the bridge (pending-map +
monotonic `id`), the bound scope, `console` stream+batch, the exit trap, child
tracking + `killChildren()`, the shell doors, the abort handshake.

### `wf_worker.ts` (workflow worker — stays JS)

No exports. Script scope = `agent, phase, log, args` + worker-built `parallel,
pipeline, console` (7 names, `SCRIPT_PARAMS`). Implements: structural coordinates,
determinism traps, exit trap, combinators, the bridge.

## 3. Data structures & wire shapes (exact field names)

The wire is string-only both directions; the one exception is `view`/`patch`, whose
text IS the payload. Structured values are JSON-serialized by the worker bindings and
re-inflated before the program sees them.

Host → program worker (`ToProgramWorker`):

```json
{"type":"run","code":"<program source>"}
{"type":"host_result","id":7,"ok":true,"value":"<string>"}   // ok:false → value is the error message; rejects the pending promise catchably
{"type":"abort"}
```

Program worker → host (`FromProgramWorker`):

```json
{"type":"host","id":7,"fn":"bash","args":["echo hi",""]}      // args: unknown[] by type, strings by convention
{"type":"log","line":"one line as printed"}
{"type":"aborted"}                                             // children swept; safe to terminate
{"type":"done","logs":["…","…"]}
{"type":"error","message":"<Error.stack or String(err)>","logs":["…"]}
```

`ProgramResult` (what the turn runner persists):

```
{ ok: bool, logs: string[], error?: string, interrupted?: bool }
```
`logs` = console output in order; partial output survives interrupt. `error` present
iff `!ok`: the thrown stack, the timeout notice, or the interrupt notice — *"Timeout
and interrupt must be distinguishable, and must say what partial work survived
(spec §6)."* `interrupted: true` only for user interrupt, never timeout.

Workflow worker wire: `{"type":"run","code":…,"argsJson":…}` in;
`{"type":"host","id":…,"fn":"agent"|"phase"|"log","args":[…],"pos":"0.1.1.0"}` out
(`pos` present on `agent` only — dot-joined slot indexes, format `\d+(\.\d+)*`,
compared component-wise as numbers, never as text; host treats it as opaque ordering,
falls back to its own counter when absent); `{"type":"done","resultJson":…}` /
`{"type":"error","message":…,"logs":[]}` back; same `host_result`/`abort`/`aborted`.

Per-binding wire conventions (from `vm_worker.ts` `bindings` and `types.ts HostFns`
docs — the Rust host dispatcher must honor these exactly):

- `bash(cmd, tags?)` → `["bash",[cmd, tags ?? ""]]`. Tags **always cross the wire, even
  absent**, so the host enforces the required param with a corrective ProgramError, not
  an arity surprise. Returns plain text.
- `sh(...)` → one JSON array arg. Two program-side shapes: variadic `sh("a","b")`
  (untagged) and array-first `sh([{cmd,tag},…])`. Returns JSON `[{code,out},…]`, in
  order; **non-zero exit code is data, never a throw**.
- `bashBg(name, cmd)` → returns JSON `{id, name, pid}`. `bashOutput/bashWait/bashKill
  (id)` → plain text.
- `view(path)`/`patch(input)`/`write(path, content)` → plain text both ways.
- `agent(task, opts?)`/`spawn(task, opts?)` → `[task, JSON.stringify(opts ?? {})]`,
  JSON back (`agent` → `{sessionId, ok, report, changedFiles}`; `spawn` →
  `{sessionId, title}`). `join(sessionId)` JSON back; `adopt(sessionId)` plain text.
- `ask(question, opts?)` → `[question, JSON.stringify(opts ?? {})]`, **plain string**
  answer back; rejects catchably on decline/interrupt.
- Method objects `state`/`schedule`/`workflow`: program calls `state.get(args)`;
  wire is `["state",["get", JSON.stringify(args ?? null)]]`; result parsed as JSON.
- `artifact(name, content)` → content stringified iff not already a string; JSON back
  (`{url, href}`).

DB tables: **none touched by this subsystem directly.** All persistence happens behind
the bridged host functions (other subsystems). The turn ctx (`types.ts Ctx`) carries
`exits`, `record`, `reads`, `touched` for the command memory, but those are populated
by `hostfn/*`, not by harness code.

Compile-time list agreement: `types.ts` pins `HostFns` ⇔ `HOST_FN_NAMES` via
`type UnboundHostFn = Exclude<HostFnName, keyof HostFns>` (must be `never`) and the
converse. In Rust this becomes: `HostFns` is a struct of `Option<…>` closures keyed by
an enum `HostFnName`; exhaustive `match` in the dispatcher gives the same guarantee.

## 4. Behaviors & edge cases (mined from code + tests; a naive port gets these wrong)

**runProgram lifecycle (`vm.ts`):**

1. Pre-flight parse *before* spawning; failure resolves immediately with
   `{ok:false, logs:[], error}` — the worker is never spawned (test: host fn that
   throws "the program must never have started" is not reached).
2. Signal already aborted at entry → resolve `interrupted()` immediately, **no worker,
   no wind-down, no ack wait** (`logs` empty, `interrupted:true`).
3. Streamed `log` lines are accumulated host-side (`streamed`) *and* forwarded to
   `onLog`. On interrupt/timeout the worker dies before posting its batched `logs`, so
   the streamed copy is what appears in the result. On `done`/`error` the worker's own
   batch is used (same lines — test pins stream == batch, same order:
   `["one","two","three",'{"a":1}',"multi part"]`).
4. **Wind-down handshake** (`stop()`): idempotent (guarded by `settled || onAborted`);
   post `{type:"abort"}`, arm a `ABORT_GRACE_MS` timer, finish on whichever of
   {`aborted` ack, grace timeout} first, *then* `worker.terminate()`. Posting into an
   already-dead worker is swallowed and finishes immediately. Reverse order orphans
   processes. Timeout takes the *same* wind-down path as interrupt.
5. `finish()` clears both the wall timer *and* an armed grace timer ("an unclaimed
   timer keeps the process awake for another second"), removes the signal listener,
   terminates, resolves once (`settled` guard).
6. **Host-call dispatch:** validate `msg.fn ∈ HOST_FN_NAMES` *before* indexing (the
   worker global is program-reachable, so `fn` is attacker-ish input;
   `host["constructor"]` must not be callable) → `unknown host function: X`. A name in
   the list but absent from `host` → the capability-denial message:
   `X() is not available in this turn — the system prompt lists the host functions this
   session was granted. Use another approach.` (test pins both "not available in this
   turn" and "system prompt"). Result posted as `String(value)`; any throw/reject →
   `{ok:false, value: err.message}` — **host failures are catchable program exceptions,
   never a killed worker** (test: `catch (e)` sees `patch conflict at src/a.ts:74-76`).
   Posts after settle are dropped silently.
7. `worker.onerror` (worker-level compile/crash) → `{ok:false, logs:streamed,
   error:"worker error: <msg>"}`. Sidecar equivalent: nonzero exit / stderr before
   `done`.
8. Error-message texts are **product surfaces pinned by tests** — port verbatim:
   - timeout: `program timed out after <N>ms — <survived>. Long-running commands belong
     in bashBg(name, cmd), not in a foreground wait.` (must contain "timed out after
     300ms", "bashBg", must NOT contain "interrupted"; `interrupted` field absent)
   - interrupt: `program interrupted by the user — <survived>` with `interrupted:true`
   - `<survived>` = `it printed nothing before stopping; anything it had already done
     (files written, commands run) still stands` when 0 lines, else `the N line(s) it
     printed before stopping are above; anything it had already done (files written,
     commands run) still stands`.

**Worker (`vm_worker.ts`):**

- Program = `new AsyncFunction(...PROGRAM_PARAMS, code)` called with the scope values
  in `PROGRAM_PARAMS` order. Completion → `{done, logs}`; throw →
  `{error, message: String(err.stack ?? err), logs}` (stack included; partial logs
  ride along).
- `console.{log,error,warn,info,debug}` all map to the same `print`: args mapped
  through `show()` (string as-is; else `JSON.stringify`, fallback `String`), joined
  with a single space; each line pushed to the batch AND sent as `{type:"log"}`
  immediately.
- **Exit trap:** `process.exit` replaced with a throw:
  `exit(<code ?? 0>) is not available to a program — a program ends by returning, and
  signals failure by throwing an Error. Calling exit() would terminate the worker
  mid-turn with no result to report.` Tests pin: caught → program continues ("still
  running"); uncaught → surfaces as program error `exit(0) is not available`, not a
  dead worker. (Uncaught exit can otherwise take the whole server down — with a
  sidecar process this hazard shrinks to "turn hangs until wall timeout", but the trap
  stays for the error-reporting contract.)
- **Child tracking:** `Bun.spawn` wrapped (plain assignment — property writable, NOT
  configurable; forwarding wrapper, signature-agnostic, arguments untouched) to
  `trackChild()` into a `Set`; reaped on `exited` (`.catch(()=>{}).finally(delete)`).
  `killChildren()` = SIGTERM sweep, throws swallowed ("already exited between the sweep
  and the signal"), set cleared. Only the async path is tracked — `spawnSync` blocks the
  event loop so abort couldn't be handled during it anyway. On `{type:"abort"}`:
  `killChildren()` **then** `{type:"aborted"}` — a dedicated test drives the protocol by
  hand *without ever calling terminate* to prove the sweep itself (not runtime
  teardown) killed the child.
- **Shell doors** (memory boundary, NOT security): shell set = `sh, bash, zsh, dash,
  ksh, fish, csh, tcsh, pwsh, powershell, cmd, cmd.exe`, matched on
  `argv0.split("/").pop()` (so `/bin/sh` counts) or any opts object with truthy
  `shell`. Shut across: `Bun.spawn` (both overloads: `(cmd, opts)` and `({cmd,…})`),
  `Bun.spawnSync`, `child_process.{exec,execSync}` (unconditionally — a command line is
  a shell by definition), `child_process.{spawn,spawnSync,execFile,execFileSync}` (only
  when shell-shaped). `node:child_process` is patched via its CJS export object
  (covers `import()`, destructured import, and `require` — all resolve to one object)
  so the error names the door the program actually used, not "Bun.spawn". Error text:
  `<what> is not available inside a program — a command run that way is absent from
  your command history, so no future session can recall it. Use await bash(cmd, tags)
  for one command, sh(a, b, …) to run several at once, or bashBg(name, cmd) for work
  that should outlive the round. Spawning a binary directly is still fine.`
  Tests pin the redirect regex `/bash\(cmd, tags\)|bash\(cmd\)/` on all 9 doors, AND
  that direct binary spawn (`Bun.spawn(["bun","-e",…])`) still works — *"If this test
  starts failing, the block has grown into a sandbox, which spec §2.2 says it must not
  be."*
- **`Bun.$` is removed entirely** (throws with its own message naming bash/sh/bashBg):
  it doesn't route through `Bun.spawn`, exposes no pid/kill handle, so a shell started
  with it would survive the sweep while the interrupt reports success — *"a hole that
  reports itself closed is worse than a missing feature."* (Node sidecar: no `Bun.*` at
  all; patch `child_process` only, and the Bun-door tests become Node-door tests.)
- All patching wrapped in try/catch: frozen globals degrade to documented open holes,
  never a crashed worker.
- `fetch`/`Response` must remain the runtime's own — `fetch` was once a bridged verb
  and the parameter shadowed the platform one (test pins `typeof fetch === "function"`
  inside a program).
- `require("node:path")` etc. must work (test pins `path.join("a","b") === "a/b"`).
- `host_result` for an unknown pending id is dropped silently.

**Workflow worker (`wf_worker.ts`):**

- **Determinism traps** (throw, with message template `<what> is not available inside a
  workflow: scripts must be deterministic, because rerun replays every agent() call
  whose key — hash(prompt + opts) — is unchanged, and a clock reading or a random value
  in a prompt changes that key on every run. <instead>`): `Date.now()`, arg-less
  `new Date()` (Proxy, NOT a subclass — `new Date(ms)`/`new Date(iso)` must keep
  working), `Math.random()`, `performance.now()`, `crypto.randomUUID()`,
  `crypto.getRandomValues()`. `<instead>` is `PASS_TIMESTAMPS` (args carries JSON
  verbatim) for clocks, `VARY_BY_INDEX` for randomness. Frozen-global failure degrades
  to "rerun re-runs everything", not silence.
- **Structural coordinates:** `Frame {path: number[], next: number}`; ROOT `{[], 0}`;
  propagated via `AsyncLocalStorage` (load-bearing — a module-level variable would name
  whichever concurrent item resumed last after an `await`). `claimSlot()` is
  synchronous and must stay so — the slot is claimed on the way into `agent()` before
  the first await. Bare sequential `agent()` calls number `0,1,2,…` (compatible with
  the host's old monotonic counter). `parallel` claims one slot then opens a child
  frame per thunk `[...base, slot]`. `pipeline` opens a frame per cell —
  **STAGE-MAJOR: `[...base, s, index]`, not item-major** — the long comment explains
  why: coordinates are the replay frontier, so structural order must imply causal
  order; item-major let a later-sorting cell dispatch before the divergence point and
  replay stale. Do not "simplify" this to item-major.
- `agent(prompt, opts?)`: claims slot sync, sends `[String(prompt),
  JSON.stringify(opts ?? {})]` with `pos`. **Throws on subagent failure** (that is what
  makes `parallel` null the slot and `pipeline` drop the item). Report returned
  verbatim UNLESS `opts.schema` present, then `JSON.parse` with failure message
  `agent(prompt, {schema}) did not return valid JSON — the report began: <first 200
  chars>`. `AgentOpts = {label?, phase?, model?, schema?}`.
- `phase(title)` / `log(message)`: fire-and-forget, return `void`, swallow transport
  failures (`.catch(()=>{})`) — a wedged UI must not stall a fan-out; awaiting one gets
  an already-resolved value, not a hang. `console.*` in a script all map to `log`.
- `parallel(thunks)`: rejects with a TypeError (naming the `() =>` mistake) if not an
  array; else a **barrier that never rejects** — each thunk via
  `Promise.resolve().then(t)` (so a synchronous throw lands in the same `.catch` as a
  rejection), non-function elements passed through as values, any failure → `null` in
  that slot.
- `pipeline(items, ...stages)`: TypeError if items not an array; per-item async loop,
  **no barrier between stages**; non-function stage → TypeError → that item drops to
  `null` (remaining stages skipped), siblings untouched; resolves in input order.
  Stage callbacks receive `(prev, originalItem, index)`; `prev` (`carried`) is read
  *before* entering the frame.
- Exit trap: same mechanism, workflow-flavored message (`a script ends by returning its
  result`).
- Abort: no children to sweep; reject all pending with `new Error("workflow stopped")`,
  clear, ack `aborted` — same handshake shape as the program worker on purpose.
- `argsJson` unparseable → script still runs with `args = null` ("the script's own
  guards are a better error than a dead worker"). Done →
  `{resultJson: JSON.stringify(result ?? null) ?? "null"}` (note the double null-guard:
  `JSON.stringify(undefined)` is `undefined`).
- `SCRIPT_PARAMS` is deliberately NOT imported by `workflow/run.ts` — importing a
  worker module into the host would evaluate the traps in the server process and break
  `Date.now()` server-wide. `workflow/run.ts` re-extends the list and a probe test pins
  the two equal. The Rust host must keep this separation: the JS sidecar owns the
  traps; the Rust driver owns its own copy of the name list, pinned by a probe test.

**Test-only helpers worth keeping as porting contracts:** `fakeHost()` echoes
`label:arg1|arg2`; the child-orphan tests use `bun -e` (not `sh -c` — shells are shut)
and assert a marker file is never written 3s after abort.

## 5. Dependencies

Imports (harness → elsewhere): only `../types.ts` (`HostFns`) and node builtins
(`node:module` createRequire, `node:async_hooks` AsyncLocalStorage). Harness is a leaf.

Imported by:
- `turn/runner.ts` — the primary consumer: `runProgram`, `ProgramResult`,
  `HOST_FN_NAMES`, `HostFnName`. One program per round.
- `workflow/run.ts` — spawns `wf_worker.ts` directly
  (`new Worker(new URL("../harness/wf_worker.ts", …))`), imports protocol types +
  `unterminatedString` for its own script pre-flight, orders journal entries by `pos`.
- `types.ts` — `HostFns`⇔`HOST_FN_NAMES` compile-time pin; `WorkflowHostFns`.
- `prompt/assemble.ts`, `hostfn/delegate.ts` — `HostFnName` type only.
- `hostfn/state.ts` — `HOST_FN_VERBS` (host dispatcher for the verb objects).
- Host implementations of every verb live in `hostfn/*` (shell, delegate, ask, state,
  schedule, artifact) — separate subsystems; harness only carries the calls.

## 6. External deps → Rust equivalents

| TS/Bun API | Where | Rust replacement |
|---|---|---|
| `new Worker(url, {type:"module"})` / `postMessage` / `onmessage` / `terminate()` | vm.ts, workflow/run.ts | `tokio::process::Command` spawning the sidecar (`bun <worker.js>` or `node <worker.js>`), NDJSON over stdin/stdout (`serde_json` + `tokio::io::BufReader::lines`); `terminate()` → `child.start_kill()` + kill the sidecar's process group (`libc::killpg` / spawn with `process_group(0)`) |
| `AsyncFunction` constructor (pre-flight) | vm.ts | Either (a) an `oxc_parser`/`swc_ecma_parser` parse of the code wrapped as an async fn body + a top-level-declaration walk against `PROGRAM_PARAMS` for the shadow check, or (b) delegate pre-flight to the sidecar (a `check` message before `run`). (b) guarantees engine parity — see risks. |
| `setTimeout`/`clearTimeout` (wall + grace timers) | vm.ts | `tokio::time::timeout` / `tokio::select!` over {result, wall-clock, cancellation} |
| `AbortSignal`/`AbortController` | vm.ts | `tokio_util::sync::CancellationToken` |
| `JSON.parse`/`stringify` | everywhere | `serde_json` |
| `String(value)` on host results | vm.ts | host fns return `String` already; enum-typed errors → `.to_string()` |
| `AsyncLocalStorage`, `Proxy`, monkey-patching `Bun.spawn`/`child_process`/`process.exit` | workers | **stays JavaScript in the sidecar — not portable to Rust and must not be attempted** |
| `bun:test` + `node:assert` | vm.test.ts | `#[tokio::test]` against the real sidecar; keep the orphan tests' marker-file design |

Crates: `tokio` (process, io, time, sync), `serde`/`serde_json`, `tokio-util`
(CancellationToken), `libc` or `nix` (killpg, SIGTERM), optionally `oxc_parser`.

## 7. Suggested Rust layout

```
crates/harness/
  src/
    protocol.rs      # HostFnName enum (+ FromStr/Display with exact wire strings),
                     # HOST_FN_NAMES, PROGRAM_PARAMS, HOST_FN_VERBS,
                     # serde types for every message (tag = "type", rename_all as-is),
                     # ProgramResult
    preflight.rs     # unterminated_string scanner (direct port) + check_program_syntax
    vm.rs            # run_program(opts) -> ProgramResult; sidecar spawn, NDJSON pump,
                     # timers, handshake, host dispatch
    wf.rs            # the workflow-worker driver half that workflow/run.rs uses
                     # (spawn wf_worker.js, pos-carrying HostCall stream)
  js/                # shipped alongside the binary (include_str! or installed files)
    vm_worker.js     # vm_worker.ts transpiled/adapted: postMessage→process.stdout
                     # NDJSON, onmessage→readline on stdin; everything else verbatim
    wf_worker.js     # same treatment
```

- `HostFns` → a struct of `Option<Arc<dyn Fn(Vec<String>) -> BoxFuture<Result<String,
  HostError>> + Send + Sync>>` fields (or a single trait `HostFns` with
  `async fn call(&self, fn_: HostFnName, args: Vec<serde_json::Value>) ->
  Result<String, HostError>` where "not granted" is a distinct error variant carrying
  the pinned message). Trait is cleaner: the dispatcher's exhaustive match replaces the
  TS `satisfies` pin.
- `run_program` shape: spawn sidecar with `stdin/stdout` piped, `stderr` inherited or
  captured for the `worker error:` path; send `run`; loop with `tokio::select!` over
  {stdout line, cancellation token, wall-clock sleep}. Host calls are dispatched as
  spawned tasks so a slow host call doesn't block `log` lines (the TS event loop gave
  this for free — **do not process the message loop serially through an awaited host
  call**; `vm.ts` awaits `hostCall` inside `onmessage` but JS message delivery is
  re-entrant between awaits; in Rust, `tokio::spawn` each host call and let results
  post back through an mpsc to the writer task).
- One writer task owns stdin (mpsc<serde_json::Value>); drop the sender to close.
- Wind-down: send `abort`, `tokio::time::timeout(ABORT_GRACE, wait_for_aborted)`,
  then kill process group.
- Async boundary: `run_program` is `async fn`; everything inside is tokio. No blocking.
- The JS mirror of `protocol.ts` constants: keep `PROGRAM_PARAMS` etc. in the worker
  JS files, and add a probe test (port of "the worker binds exactly PROGRAM_PARAMS")
  that runs a real program printing `typeof` of every name — that test is what keeps
  the Rust list and the JS list from drifting, replacing the shared-import invariant.

## 8. v1 scope cut

- **Core (cannot cut):** `protocol.rs`, `preflight.rs` (shadow + unterminated-string
  messages are what keep weak models productive), `vm.rs` with the full lifecycle
  (stream+batch logs, timeout/interrupt distinction with verbatim messages, abort
  handshake, capability-denial message), `vm_worker.js` with exit trap + child
  tracking + shell doors + `require`. Always-wired verbs on `HostFns`: bash, sh,
  bashBg/Output/Wait/Kill, view, patch, write.
- **Stub in v1:** the optional verbs (`agent, spawn, join, adopt, workflow, ask,
  state, schedule, artifact`) — absence already IS the denial path, so shipping v1
  with them absent exercises the pinned "not available in this turn" behavior and cuts
  nothing structural. Wire them as their host subsystems land.
- **Defer:** `wf.rs` + `wf_worker.js` entirely — workflows are a separate engine
  (`workflow/run.ts`) and the agent loop runs without them; when ported, port the
  stage-major coordinates and determinism traps exactly, they are journal-correctness,
  not polish.
- **Drop:** nothing else — the file is already minimal. Do NOT drop the shell doors or
  `Bun.$` removal "temporarily": the interrupt contract ("children are killed") becomes
  a lie without them, and the tests that pin the 9 doors are cheap to port.
- Sidecar runtime choice for v1: whichever of Bun/Node ships with the install; if
  Node, the `Bun.*` doors reduce to the `child_process` patches and the
  direct-binary-spawn test switches to `child_process.spawn`.
