# Phase codemode — one `run(program)` tool over an embedded JS sandbox

Status: PLAN. Nothing here is built yet. Branch `rebuild-codemode`, parallel to the track-B merge
on `rebuild`; the two merge afterwards.

**Direction from Andrey (relayed 2026-08-27).** The original bough (`~/repos/bough`, branch `main`)
was code mode: two API tools — `run_steps(program)` + `stop` — one JS program per round over ~20
pre-injected host functions, benchmarked 14/16 @ $0.042 against Claude Code's 16/16 @ $0.076.
REQUIREMENTS §0.4 dropped "the Code Mode SDK" from dsh and nothing replaced it. This phase revisits
that decision **by building the second consumer and measuring it**, not by arguing. The default stays
`tools-typed` until Andrey switches it by patch; the GO is his.

The literature the design leans on: CodeAct (arXiv 2402.01030) and arXiv 2602.15945 for
code-as-action; **headlong** (github.com/laude-institute/headlong, `design/`) for the product
lineage — its stance is that the model is an **OPERATOR of an environment, not a dispatcher of typed
calls**: actions are free-form commands written into an append-only trajectory and executed by one
actor, and every observation comes back through the same log. That is the same shape as one
`run(program)` over a ledgered pipeline, which is why §18 gains headlong as reference semantics
where the spec is ambiguous (WP-8).

---

## 0. What does NOT change

The `tools` seam is untouched, byte for byte, on this branch:

- `ToolSpec { name, description, input_schema, render, scope, tool }` and the `Tool` trait.
- the pipeline `tools/pre-execute` → `tools/execute` → `tools/post-execute` → `tools/result`,
  with the monotone guard, the deadline wrap, the content-OR-value rule, and the
  concurrency-safe/barrier rule.
- scope shadowing and `tools.restrict` as an intersection filter (§5).
- the two step types `tool/call` and `tool/result`, and their pairing invariant.

Code mode is a **second Consumer of that seam**. Every existing Provider and Consumer
(`tools-baseline`, `tool-actions`, `tool-workers`, `tool-mcp`, `claims`' global `propose_claim`,
`tool-leader`) keeps registering exactly what it registers today. What changes is *which surface the
model is shown*: one API tool instead of N schemas.

The agent loop is untouched too. It appends `tool/call` for the model's `run` call, executes it
through `ToolsHandle::execute_under`, and appends `tool/result` with what came back. Sub-steps are
appended by the codemode consumer during that execution, so they land in seq order between the two —
"under the program step" is a fact of the ledger, not of a nesting column.

### 0.1 The one seam gap, and the interim that works without it

The loop builds a request's tool list from `ToolsHandle::schemas(agent)`, and `schemas()` and
`resolve()` read the **same** filtered view. So with today's public API, hiding a tool from the
prompt also makes it un-executable: `Restrict { allow: {run} }` would give the right prompt and a
sandbox where every call answers `NotFound`.

What the phase actually wants is **visibility-only concealment** — which is what §9 already says
`restrict` is ("visibility composition, not an authority boundary"), except that §5 additionally
requires `restrict` to refuse execution, so `restrict` cannot be it. The hook is written up in
`docs/codemode-merge-notes.md` (WP-8) with file, signature and rationale:

```rust
// plugins/tools/src/registry.rs + lib.rs  (post-merge; NOT on this branch)
pub struct Conceal { pub keep: BTreeSet<ToolName> }
impl ToolsHandle {
    /// Names outside `keep` vanish from `schemas()`/`visible()` and stay RESOLVABLE and
    /// EXECUTABLE. Model-visible ⟺ ledgered still holds: the surface that exposes them is a
    /// projection section, and every call is a step.
    pub async fn conceal(&self, ctx: &Context, agent: &AgentName, c: Conceal)
        -> Result<EffectHandle, PluginError>;
}
```
plus `#[serde(default)] pub sub_step: bool` on `ToolCallBody`/`ToolResultBody` and a `sub_step`
skip in `agent-loop`'s `transcript::rebuild`, so post-merge the sub-steps can carry the canonical
`tool/call` / `tool/result` kinds.

**Interim (this branch, `ConcealMode::Mirror`, the default):** `tools-codemode` installs
`Restrict { allow: {run} }` per agent as a *disposable effect it owns*, and at the start of every
`run` call it takes `snapshot_lock`, disposes its own restriction, reads the agent's full visible
set (`visible` + `schemas` + `resolve` + `render_intent` — enough to rebuild every `ToolSpec`),
re-installs the restriction, and registers the rebuilt specs into a **mirror `ToolsHandle`**
(`ToolsHandle::with_limits`, public) that the program's host functions execute against. The mirror
runs the *same* pipeline on the *same* `Context`, so all four `tools/*` events fire and a
pre-execute deny still denies. Other rows' restrictions (a lane's `deny: [bash]`) are never
disposed, so they compose exactly as before. Because the snapshot is retaken per program, a lane's
scoped extras and a leader set moved by patch mid-run are both current.

Honest cost of the interim: a `schemas(agent)` call landing inside the snapshot window (a preview
pane rendering while the same agent is mid-program) would see the unrestricted list. The window is
microseconds under one lock, and the whole mode is deleted when `conceal` lands. `ConcealMode::Seam`
is behind cargo feature `seam-conceal` (off on this branch) and is a two-line body.

`ConcealMode::None` exists for the bench's "both surfaces mounted" control run and for debugging.

---

## 1. Crates

New rows. Package names are `bough-plugin-<name>` per AGENTS.md; the row id is what a patch targets.

| package | row id | plugin name | inject | provides |
|---|---|---|---|---|
| `bough-plugin-js` | `js` | `js` | required `[]` | `js` |
| `bough-plugin-js-quickjs` | `js.quickjs` | `js-quickjs` | required `[js]` | — (sets the engine factory on `js`) |
| `bough-plugin-tools-codemode` | `tools.codemode` | `tools-codemode` | required `[tools, js, ledger, agents, projection]`, optional `[approval]` | — (Consumer) |
| `bough-plugin-tools-operator` | `tools.operator` | `tools-operator` | required `[tools, ledger, workspace]`, optional `[agents, mail]` | — (Consumer) |
| `bough-bench-tools` | — | — (a bench crate, not a row) | — | — |

Edited rows: `bough-plugin-tool-leader` (five tools → two), `bough-plugin-tui-focus` (the program
row only).

Placement:
- `js` + `js.quickjs` + `tools.codemode` go in the binary's catalog and in **no bundle** — the
  `ledger-memory` / `rollups-none` / `tui-probe` precedent. A new `bundles/bough-codemode.yml` and
  `profiles/codemode.yml` insert them; `--patch` does the same at runtime for the SWAP test.
- `tools.operator` goes in `bundles/bough-base.yml`, mounted by default: `view`/`patch`/`write`/
  `bg`/`ledger_read`/`inbox`/`schedule` are ordinary tools that the **typed** consumer benefits from
  too, and the bench compares surfaces, not tool inventories. `tools-baseline` stays mounted and
  unchanged; under code mode its `read_file`/`glob`/`grep` are simply not documented in the surface
  section (they remain callable — the sandbox injects everything visible).

Why `js` is a seam and not one crate (§0.2 forbids preemptive splitting): a second Provider is
already named — main's sidecar protocol (`git show main:crates/bough-core/src/harness/protocol.rs`,
`preflight.rs`, `harness/js/vm_worker.js`) is the documented fallback if QuickJS's stdlib proves
short of the surface. It is not built in this phase; the seam is what makes it a swap rather than a
rewrite. See §7 "Decisions for Andrey" on whether a *second provider per seam* stays a phase gate.

---

## 2. Public API

### 2.1 `bough-plugin-js` — the runtime Service Definition

```rust
pub const PLUGIN_NAME: &str = "js";

pub struct Js;
impl ServiceKey for Js { type Value = JsHandle; const NAME: &'static str = "js"; }

#[derive(Clone)]
pub struct JsHandle(pub Arc<JsInner>);

impl JsHandle {
    /// No `new()` and no `Default`: `Caps` are deployment-varying and `JsConfig` is their one
    /// source (§0.2), exactly as `ToolsHandle::with_limits` is spelled.
    pub fn with_caps(default: Caps) -> JsHandle;

    /// The factory slot, in the shape of `ctx.agents.set_factory` (§2): a SECOND engine is an
    /// error, not a silent replacement. Registration is an effect.
    pub async fn set_engine(&self, ctx: &Context, e: Arc<dyn JsEngine>)
        -> Result<EffectHandle, PluginError>;
    pub fn engine(&self) -> Option<Arc<dyn JsEngine>>;
    pub fn default_caps(&self) -> Caps;

    /// Compile-only. The parse comes from the engine that will run the program, so host and
    /// engine can never disagree about what is legal (main's `check` message, ARCHITECTURE §4.1).
    pub async fn check(&self, src: &str) -> Result<(), JsError>;
    pub async fn run(&self, p: Program) -> Result<Run, JsError>;
}

#[async_trait::async_trait]
pub trait JsEngine: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn check(&self, src: &str) -> Result<(), JsError>;
    async fn run(&self, p: Program) -> Result<Run, JsError>;
}

/// One program. The engine owns NO I/O: everything the program can reach is in `host`.
pub struct Program {
    pub source: String,
    pub caps: Caps,
    /// Injected as globals in NAME order. A dotted name builds a namespace object
    /// (`ledger.search`), and a name that is both a function and a namespace root
    /// (`bg`, `bg.output`) becomes a callable object.
    pub host: Vec<HostFn>,
    pub console: Arc<dyn ConsoleSink>,
    pub cancel: CancellationToken,
}

pub struct HostFn { pub name: String, pub arity: u8, pub body: Arc<dyn HostCall> }

#[async_trait::async_trait]
pub trait HostCall: Send + Sync + 'static {
    /// `Ok` resolves the promise, `Err` rejects it with a JS `Error` carrying `kind`.
    async fn call(&self, args: Vec<serde_json::Value>) -> Result<serde_json::Value, HostRefusal>;
}

pub struct HostRefusal { pub kind: RefusalKind, pub message: String }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind { NotFound, Denied, Blocked, Timeout, Cancelled, Error }

pub trait ConsoleSink: Send + Sync + 'static {
    /// One `console.log(...)` line, already formatted. Called on the engine's thread; the sink
    /// must not block. `tools-codemode`'s sink buffers and flushes as `program/console` steps.
    fn write(&self, line: &str);
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Caps {
    pub ops: u64,            // interrupt-handler budget
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub wall_ms: u64,
    pub console_bytes: usize,
}

pub struct Run {
    pub console: String,
    pub console_bytes_dropped: usize,
    pub ops: u64,
    pub ms: u64,
    /// The program's completion value, if it produced one. NOT model-visible: console is.
    pub value: Option<serde_json::Value>,
}

/// The taxonomy the model sees, and the one thing that lands as a `program/error` step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JsError {
    Syntax { message: String, line: Option<u32>, col: Option<u32> },
    Thrown { message: String, stack: Option<String> },
    OpsExceeded { ops: u64 },
    MemoryExceeded { bytes: usize },
    TimeExceeded { ms: u64 },
    StackExceeded,
    Cancelled,
    /// No Provider set an engine. Fail-loud: the `tools-codemode` row refuses to boot.
    NoEngine,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JsConfig { pub default_caps: Caps }
```

Invariant module (`js`): every `Program` that ends does so with exactly one terminal outcome —
`Run` or `JsError` — and never both; a cancelled program never reports `Run`.

### 2.2 `bough-plugin-js-quickjs` — the Provider

rquickjs, pinned `"0.12"` (latest 0.12.2 as of 2026-08-27; pre-1.0, so a minor pin per §13),
features `["async-std"…]` → **no** `loader`, **no** `bindgen`-generated `std`/`os` modules. Default
globals are plain ECMAScript: no `fetch`, no `process`, no `require`, no module loader, no timers
other than what we inject (we inject none).

```rust
pub const PLUGIN_NAME: &str = "js-quickjs";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuickJsConfig {
    /// How often the interrupt handler samples the wall clock, in interrupt ticks. A real
    /// tunable: too small burns time in the handler, too large loosens the wall-clock cap.
    pub interrupt_check_ops: u64,
    /// Programs that may run at once across the tree. A barrier, not a queue depth.
    pub max_concurrent_programs: usize,
}

pub struct QuickJsEngine { /* Runtime factory; one Runtime per program, dropped after */ }
impl JsEngine for QuickJsEngine { .. }
```

Caps map onto rquickjs as: `Runtime::set_memory_limit(memory_bytes)`,
`Runtime::set_max_stack_size(stack_bytes)`, `Runtime::set_interrupt_handler(..)` counting ops and
checking `Instant::now()` every `interrupt_check_ops` ticks and on `cancel`. Host functions are
`async` and resolve promises through `AsyncContext`; the program is wrapped as
`(async () => { <source> })()` and awaited, so top-level `await` works without module machinery.

Invariant (`js-quickjs`): no `Runtime` outlives its program — the count of live runtimes returns to
zero after every terminal outcome.

### 2.3 `bough-plugin-tools-codemode` — the Consumer

```rust
pub const PLUGIN_NAME: &str = "tools-codemode";
/// The ONE API tool. A protocol constant, not config: the TUI, the bench and the surface section
/// all key on it.
pub const RUN_TOOL: &str = "run";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodemodeConfig {
    /// `None` ⇒ the `js` row's `default_caps`.
    #[serde(default)] pub caps: Option<Caps>,
    #[serde(default)] pub conceal: ConcealMode,
    /// JS name → registered `ToolName`. Ships as `{claim: propose_claim, agent: spawn_worker}`.
    #[serde(default)] pub aliases: BTreeMap<String, String>,
    /// JS namespace → `ToolName` prefix. Ships as `{mcp: "mcp__", act: ""}` — see §3 `act`.
    #[serde(default)] pub namespaces: BTreeMap<String, String>,
    pub max_console_bytes: usize,
    pub max_calls_per_program: u32,
    /// `bash`/`sh` legs must carry 3–5 tags. `false` only for the bench's control arm.
    pub tags_required: bool,
    /// Register the surface documentation as a projection section. `false` for tests that build
    /// the request themselves.
    pub surface_section: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConcealMode { #[default] Mirror, Seam, None }
```

The row registers, as effects:
1. one global `ToolSpec { name: "run", render: RenderIntent::Generic, scope: ToolScope::Global }`,
   `is_concurrency_safe -> false` (always exclusive), input schema
   `{"type":"object","properties":{"program":{"type":"string"}},"required":["program"]}`;
   the description is one sentence and points at the surface section — **no per-request schemas**;
2. the concealment (§0.1), at apply for every live agent and on `agents::AgentCreated`;
3. the four step types of §4;
4. the projection section `codemode.surface` at `Position::before(identity)`,
   `DropPriority::Never` (a program surface that degrades is a program that cannot be written);
5. its invariant.

The `run` tool's `call()`:

```
1. preflight: js.check(program)  → Syntax lands as a program/error step + a failed tool/result,
   with main's unterminated-string diagnosis (ported) rather than the engine's bare message.
2. snapshot the agent's tool set and build the mirror registry (§0.1).
3. build Vec<HostFn>: one per visible ToolSpec (name → JS identifier), plus aliases and
   namespaces. Each HostCall body:
     a. mint call id  = format!("{run_call_id}.{n}")   — DETERMINISTIC, so replay reproduces it
     b. append `program/call`
     c. mirror.execute_under(ctx, vec![call], cancel)  — the SAME pipeline
     d. append `program/result`
     e. Ok(value|content) or Err(HostRefusal) mapped from FailureClass
   Concurrency inside a program: a spec with `is_concurrency_safe(args) == true` takes a read on
   the program's RwLock, everything else takes a write — the seam's barrier rule, reproduced for
   `Promise.all` and `sh()`.
4. js.run(Program { source, caps, host, console: ConsoleTee, cancel })
5. terminal outcome:
     Run   → ToolOutcome { content: rendered console, cites: union of inner cites,
                           concludes_wake: any inner result concluded }
     Error → program/error step, then ToolFailure { kind: Error|Timeout|Cancelled, message }
```

Nothing else is registered. **There is no `stop` tool** — the wake ends because the model answers in
text and calls nothing, which the loop already treats as "owes no further request" (§5's
`agent/wake-stopping`).

### 2.4 `bough-plugin-tools-operator` — the tools code mode needs that do not exist yet

Seven global `ToolSpec`s, all ordinary tools, all available to the typed consumer too.

```rust
pub const PLUGIN_NAME: &str = "tools-operator";
pub const TOOL_NAMES: [&str; 7] =
    ["view", "patch", "write", "bg", "ledger_read", "inbox", "schedule"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub max_view_bytes: usize,
    pub max_files_per_patch: usize,
    pub bg_log_dir: PathBuf,
    pub bg_max: usize,
    pub bg_poll_ms: u64,
    pub ledger_page: usize,
    pub schedule_max_horizon_days: u32,
    pub schedule_tick_ms: u64,
}

// --- the file verbs (WP-3), ported from `git show main:crates/bough-core/src/hostfn/patch.rs` ---
pub enum OpKind { Swap, Del, InsPre, InsPost, InsHead, InsTail }
pub struct PatchOp { pub path: String, pub tag: String, pub kind: OpKind,
                     pub from: usize, pub to: usize, pub body: Vec<String> }
pub struct FileOps { pub path: String, pub tag: String, pub ops: Vec<PatchOp> }

pub fn normalize(text: &str) -> String;          // CRLF + BOM do not change a file's identity
pub fn tag_of(text: &str) -> String;             // fnv1a over utf16 code units, 4 hex chars
pub fn render_numbered(path: &str, text: &str) -> String;   // `[path#TAG]` + `N:text`
pub fn parse_patch(input: &str) -> Result<Vec<PatchOp>, PatchError>;
pub fn group_by_file(ops: &[PatchOp]) -> Result<Vec<FileOps>, PatchError>;
pub fn check_ops(path: &str, ops: &[PatchOp], count: usize) -> Result<(), PatchError>;
pub fn materialize(lines: &[String], ops: &[PatchOp]) -> Vec<String>;
pub fn line_map(base: &[String], cur: &[String]) -> Vec<Option<usize>>;
pub enum RebaseResult { Unchanged, Rebased(Vec<PatchOp>), Conflict(RebaseConflict) }
pub fn rebase_ops(ops: &[PatchOp], base: &[String], cur: &[String]) -> RebaseResult;
pub fn apply_patch(input: &str, seen: &SeenFiles) -> Result<Vec<Applied>, PatchError>;

/// What `view`/`write` remember so `[path#]` means "the version you just saw". Per (agent, path):
/// the tag AND the text, because a rebase re-checks the actual lines rather than trusting the tag.
pub struct SeenFiles(parking_lot::Mutex<BTreeMap<(AgentName, PathBuf), (String, String)>>);
```

`bg` is ONE tool with `{op: "start"|"output"|"kill", name?, cmd?, id?}` — the "four kinds → one
function" shape the brief endorses for `act` — sugared in JS as `bg(name, cmd)` / `bg.output(id)` /
`bg.kill(id)`. `ledger_read` is one tool with `{op: "search"|"steps"|"tail", ...}`, sugared as the
`ledger` namespace. `inbox` takes no args. `schedule` takes `{at, intent}`.

`schedule` is §5's "own scheduled intents", which nothing exposes today and for which no
`ctx.schedule` exists yet (Phase 7). It appends a `schedule/intent` step; a due-watcher in the row
delivers, at the due time, `Agent::send(Message { from: Sender::System("schedule"), class:
MailClass::Wake, .. }, Target::NextWake, /*wake*/ true)` and appends `schedule/fired`. The clock is
a `trait Clock { fn now(&self) -> DateTime<Utc>; }` injected at construction — `SystemClock` in
production, a synthetic one in tests. `inject: optional ["schedule"]`: when Phase 7's `ctx.schedule`
appears, the watcher half is deleted and the tool registers a cron entry instead. Recorded in
`docs/codemode-merge-notes.md`.

---

## 3. The sandbox surface — documented ONCE

One namespace (`globalThis`), ~15 names, one projection section (`codemode.surface`), assembled from
files under `plugins/tools-codemode/src/surface/`. Three of them are **restored verbatim** from main:
`patch-grammar.md`, `files.md`, `shell.md` (`git show main:crates/bough-core/src/prompt/sections/…`),
with the Bun-specific paragraphs of `files.md`/`shell.md` retargeted at QuickJS + `bash` and the
`bough patterns` paragraph dropped (that binary does not exist in the rebuild).

| name | goes to | notes |
|---|---|---|
| `await bash(cmd, tags) -> string` | `bash` (tools-baseline) | tags REQUIRED, 3–5 colon-separated; a dotted tag (`pr.456`) becomes a `Ref` on the `program/call` step |
| `await sh([...]) -> [{code, out}]` | N × `bash` | concurrent; every leg must be `{cmd, tag}`; a bare-string leg is REFUSED (main only warned) |
| `await bg(name, cmd)` / `bg.output(id)` / `bg.kill(id)` | `bg` | name required and non-blank |
| `await view(path) -> string` | `view` | `[path#TAG]` + `N:text` |
| `await patch(input) -> string` | `patch` | the grammar below; echoes each file's new tag |
| `await write(path, content) -> string` | `write` | creates; echoes the new tag |
| `await ledger.search(q)` / `.steps(range)` / `.tail(n)` | `ledger_read` | drill from a tier's `notable_refs` down to raw steps |
| `await inbox()` | `inbox` | unconsumed mail this wake has not claimed |
| `await claim({kind, title, body, cites})` | `propose_claim` (`claims`; the leader's scoped one shadows it) | |
| `await act(kind, target, payload)` | `open_pr` \| `push_to_pr` \| `bot_thread_op` \| `linear_write` (`tool-actions`) | four kinds, one function; the journal and its idempotency key are unchanged |
| `await agent(prompt, opts)` | `spawn_worker` (`tool-workers`) | bounds unchanged (§7/§10) |
| `await ask(q)` | `ask` | |
| `await fork(opts)` | `fork` | |
| `await schedule(at, intent)` | `schedule` | |
| `mcp.<server>.<tool>(args)` | track-B `tool-mcp` specs matching the `mcp__` prefix | grouped by `namespaces` config |
| `console.log(...)` | — | the ONLY thing returned to the model, and itself a step |

**Dropped as separate functions** (they remain callable — the sandbox injects every visible spec —
but the surface section does not teach them): `glob`, `grep`, `read_file`; `bash rg` and `view`
cover them. **Not present at all**: `state`, `milestone`, `stop`.

The patch grammar section, verbatim from main:

```
SWAP A.=B:   replaces lines A..B          DEL A.=B      removes them
INS.PRE A:   before line A                INS.POST A:   after line A
INS.HEAD:    at the file's start          INS.TAIL:     at its end
```
Body rows are `+`-prefixed NEW text only; there are no `-` rows. Every line number is in the
coordinates of the version you VIEWED; earlier operations do not shift later numbers. `[path#]` means
"the version you just viewed or wrote"; an explicit `[path#A62C]` chains onto an echoed tag. A file
this session never saw is refused rather than applied blind. One patch may carry several files and
applies ALL of them or NONE. `edit_file(old, new)` is a regression and is not offered.

Ending a turn (this section replaces main's, which assumed a `stop` tool): **your turn ends when you
answer in text and call nothing.** There is no stop tool. A round that only calls `run` and prints is
a round the user cannot read, so end with plain text.

---

## 4. Step types

Owned by `tools-codemode` (`ledger.declare_step_types`, an effect):

```rust
/// `program/call` — Thought. One inner tool call made from inside a program.
pub struct ProgramCallBody {
    pub program: ToolCallId,   // the `run` call this is under — the nesting anchor
    pub index: u32,            // 0-based, in issue order within the program
    pub call: ToolCallId,      // `{program}.{index}`
    pub name: ToolName,
    pub args: serde_json::Value,
    pub render: RenderIntent,
    #[serde(default)] pub tags: Vec<String>,   // bash/sh only
    pub step_index: u32,       // the wake step the `run` call belongs to
}

/// `program/result` — Either (cites decide the class, as `tool/result` does).
pub struct ProgramResultBody {
    pub program: ToolCallId,
    pub index: u32,
    pub call: ToolCallId,
    pub name: ToolName,
    pub outcome: ToolOutcomeKind,
    pub content: String,
    #[serde(default)] pub value: Option<serde_json::Value>,
    #[serde(default)] pub attached: Vec<AttachedContext>,
    #[serde(default)] pub concludes_wake: bool,
    pub step_index: u32,
    pub ms: u64,
}

/// `program/console` — Thought. One flush of console output, appended AS PRODUCED so the TUI
/// streams it. The concatenation of a program's chunks IS the `tool/result` content.
pub struct ProgramConsoleBody {
    pub program: ToolCallId,
    pub chunk: u32,
    pub text: String,
    #[serde(default)] pub dropped_bytes: usize,   // > 0 on the truncation notice chunk
}

/// `program/error` — Thought. The one terminal error a program can end with.
pub struct ProgramErrorBody {
    pub program: ToolCallId,
    pub error: JsError,          // tagged enum, §2.1
    pub ops: u64,
    pub ms: u64,
}
```

Owned by `tools-operator`:

```rust
/// `schedule/intent` — Evidence (it cites the step that asked for it).
pub struct ScheduleIntentBody { pub id: ScheduleId, pub agent: AgentName,
                                pub at: DateTime<Utc>, pub intent: String }
/// `schedule/fired` — Thought.
pub struct ScheduleFiredBody { pub id: ScheduleId, pub at: DateTime<Utc>,
                               pub message: MessageId }
```

**Why the sub-steps are not spelled `tool/call` / `tool/result`.** `agent-loop`'s
`transcript::rebuild` folds *every* step of those two kinds into the request. Inner calls are
ledgered but NOT model-visible (the model sees console output), so folding them would make the
reconstruction disagree with what was sent and V2 would fail. Distinct kinds keep the fold correct
with zero edits to `agent-loop`. Post-merge, `sub_step: bool` + a fold skip (§0.1) lets them carry
the canonical kinds; the bodies are already field-compatible.

**No new events.** The phase adds one service key (`js`), four + two step types, and no event type.
Everything it needs is already dispatched: `tools/pre-execute` (waterfall), `tools/execute`
(waterfall), `tools/post-execute` (waterfall), `tools/result` (emit), `ledger/step` (emit),
`agent/created` (emit), `agent/wake-stopping` (serial).

---

## Work packages

### WP-1: `js` seam + QuickJS provider
**Files:** `plugins/js/**` (new crate), `plugins/js-quickjs/**` (new crate), `Cargo.toml`
(workspace deps: `rquickjs = "0.12"`).

The runtime, and nothing else: no ledger, no tools, no domain vocabulary. `bough-plugin-js` is
§2.1 exactly — key, handle, `JsEngine` factory slot with the second-engine error, `Program`,
`HostFn`/`HostCall`, `ConsoleSink`, `Caps`, `Run`, `JsError`, `JsConfig`, invariant. `js-quickjs`
builds one `Runtime` per program with the memory/stack limits set, an interrupt handler that counts
ops and samples the wall clock every `interrupt_check_ops` ticks and on `cancel`, no module loader
and no `std`/`os` bindings, host functions as promise-returning async closures, and the program
wrapped in an async IIFE so top-level `await` works. Port main's `preflight.rs`
unterminated-string scanner and its model-facing syntax messages verbatim into `js-quickjs`;
`check()` delegates the parse to the same engine that will run it.

Unit tests (in-crate): `set_engine` twice is an error and the first engine survives; a disposed
engine effect leaves `engine() == None`; `Caps` round-trips through schemars; `JsError` tags
serialize as `{"kind":"ops_exceeded","ops":N}`. In `js-quickjs`: `hello` runs and returns its
console; a host function's rejection surfaces as a JS `Error` with `kind`; an infinite loop hits
`OpsExceeded`; `new Array(1e9)` hits `MemoryExceeded`; a busy loop past `wall_ms` hits
`TimeExceeded`; deep recursion hits `StackExceeded`; `cancel` mid-program yields `Cancelled` and
no `Run`; `typeof fetch/require/process/Deno/globalThis.std === "undefined"` and
`import("fs")` rejects; every `Runtime` is dropped (the live count returns to zero).

### WP-2: `tools-codemode` — the consumer, the mirror, the run tool
**Files:** `plugins/tools-codemode/Cargo.toml`, `plugins/tools-codemode/src/lib.rs`,
`src/run.rs`, `src/bind.rs`, `src/conceal.rs`, `src/console.rs`, `src/vocabulary.rs`,
`src/invariant.rs`, `plugins/tools-codemode/tests/{pipeline,ledgered,surface}.rs`.

§2.3 and §0.1. `conceal.rs` owns `ConcealMode` and the snapshot-under-lock that rebuilds the
agent's `ToolSpec`s into a mirror `ToolsHandle`; `bind.rs` turns specs + aliases + namespaces into
`Vec<HostFn>` (name → JS identifier, dotted names → namespaces, callable namespace roots) and maps
`FailureClass` ↔ `RefusalKind`; `run.rs` is the `Tool` impl and the deterministic
`{run}.{n}` call ids, the read/write concurrency lock, `max_calls_per_program`, and the terminal
outcome mapping; `console.rs` is the `ConsoleSink` that flushes `program/console` steps and renders
the head/tail truncation notice at `max_console_bytes`; `vocabulary.rs` is §4's four step types.
Invariant: for every `program` id, the ordered concatenation of its `program/console` chunks equals
the `tool/result` content of the `run` call (modulo the truncation notice), and every
`program/call` has exactly one `program/result` with the same `index` before the `run` call's
`tool/result`.

Unit tests: identifier mapping refuses a name that is not a JS identifier; alias and namespace
composition; deterministic call ids; the truncation renderer keeps head+tail and names the dropped
byte count. Integration: the four `tools/*` events fire for every inner call; a `tools/pre-execute`
deny inside a program returns a rejected promise and lands a `program/result` with
`outcome: "denied"`; a lane-restricted tool is absent from the injected globals AND `NotFound` at
the mirror; a cap breach lands a `program/error` step and a failed `tool/result`; `run` never
reports `concludes_wake` unless an inner result did.

### WP-3: `tools-operator` — the file verbs and main's patch grammar
**Files:** `plugins/tools-operator/src/files/{mod,view,write,grammar,apply,rebase,seen}.rs`,
`plugins/tools-operator/tests/{patch_grammar,files}.rs`.

A port, not a design: `git show main:crates/bough-core/src/hostfn/patch.rs` is already pure Rust
(~950 lines of code, ~1000 of tests) — `normalize`, `tag_of`, `to_lines`, `join_lines`,
`render_numbered`, `parse_patch`, `group_by_file`, `check_ops`, `materialize`, `line_map`,
`rebase_ops`, `apply_patch` — and `hostfn/files.rs` is the three tool bodies. Bring both across
with their tests, retargeting `BoughError` → a local `PatchError`, the session file map →
`SeenFiles` keyed by `(AgentName, PathBuf)`, and containment onto `ctx.workspace`
(`WorkspaceRoot`, absolute + canonical). Exports `pub fn specs(cfg: &Arc<OperatorConfig>, …) ->
Vec<ToolSpec>` for `view`/`patch`/`write`; it does not touch `lib.rs` (WP-4 owns that).

Unit tests (ported wholesale, then extended): `view` returns `[path#TAG]` + numbered lines and
CRLF/BOM do not change a tag; each of SWAP/DEL/INS.PRE/INS.POST/INS.HEAD/INS.TAIL applies in viewed
coordinates and earlier ops do not shift later numbers; a multi-file patch is all-or-nothing (a
conflict in one file leaves the others byte-identical); a stale tag is refused; a file never viewed
is refused; an untouched range rebases onto a moved file and a touched range conflicts naming the
line range; `write` creates and echoes a tag that `patch` accepts without a re-view; a path outside
the workspace is `Denied`.

### WP-4: `tools-operator` — bg, ledger_read, inbox, schedule, the row
**Files:** `plugins/tools-operator/Cargo.toml`, `plugins/tools-operator/src/lib.rs`,
`src/{bg,ledger_read,inbox,schedule,clock,invariant}.rs`,
`plugins/tools-operator/tests/{bg,ledger_read,schedule}.rs`,
`bundles/bough-base.yml` (the `tools.operator` row).

`lib.rs` is the row: `OperatorConfig`, `inject`, registration of all seven specs as effects,
`schedule/*` step types, the invariant. `bg` is the one three-op tool over a detached child with
its output tee'd to `bg_log_dir/<id>.log`, bounded by `bg_max`, and killed on row disposal (unwind
leaves no orphan). `ledger_read` is `search`/`steps`/`tail` over `LedgerHandle`, paged by
`ledger_page`, results cited so a drill is evidence. `inbox` reads `unconsumed_mail` for the
agent's trajectory. `schedule` appends `schedule/intent`, and the due-watcher fires it through
`Agent::send(.., Sender::System("schedule"), MailClass::Wake, Target::NextWake, wake = true)` and
appends `schedule/fired`; the clock is injected (`trait Clock`). Invariant: every `schedule/fired`
names a `schedule/intent` that exists and is not already fired.

Unit tests: `bg` start/output/kill round-trip and `bg_max` refuses the N+1th; a killed job's log is
still readable; row disposal kills every live job; `ledger_read` paging and cites; `inbox` returns
nothing once a `wake/end` consumed the seqs; a scheduled intent at T+5m fires exactly once on a
synthetic clock advanced past it, wakes the creator, and is idempotent across a restart replay; a
horizon beyond `schedule_max_horizon_days` is refused.

### WP-5: the surface section and the prompt projection
**Files:** `plugins/tools-codemode/src/surface/{mod.rs,patch-grammar.md,files.md,shell.md,
printing.md,ending.md,ledger.md,work.md}`, `plugins/tools-codemode/tests/section.rs`.

The "documented ONCE" deliverable: a `SectionRender` that assembles §3's table and the restored
main sections into one `codemode.surface` projection section, positioned before `identity` with
`DropPriority::Never`, contributing nothing when the agent has no `run` in scope (so mounting the
row without concealment does not double-document). It renders from the LIVE registry — the function
list in the section is generated from the same snapshot the sandbox injects, so the surface the
model reads and the surface it gets cannot drift.

Unit tests: the section is byte-stable for a fixed registry (insta snapshot); the generated function
list equals the injected globals for the same agent; a restricted tool is in neither; the patch
grammar text appears exactly once in a whole assembled projection; the section is absent for an
agent that cannot see `run`; the assembled section's token estimate is recorded (it is the bench's
main lever and must not drift silently).

### WP-6: `tool-leader` — five tools collapse to two
**Files:** `plugins/tool-leader/src/tools.rs`, `plugins/tool-leader/src/lib.rs`,
`plugins/tool-leader/tests/tools.rs`, `crates/bough/tests/leader_swap.rs`,
`scripts/tui/15-leader-swap.sh`.

`TOOL_NAMES` becomes `["propose_claim", "curate"]`. `propose_claim` keeps the scoped shadowing of
the global one and absorbs `draft_requirement` (kind `requirement`, `cites` required and enforced)
and `propose_structure` (kinds `lane|split|merge|bud`); it keeps admitting `contradiction|other`
because the global one does and the scoped spec shadows it — removing them would LOSE capability,
which the brief's five-kind list does not ask for (flagged in §6). `curate` takes
`{placements?: [{step, agent}], steps?: [String], timeline?: [TimelineEntry]}` and performs
`adopt_unsorted` + `note_timeline` in one journalled pass; an empty call is refused rather than
being a silent no-op. `plugins/leader` is untouched: `AdoptRequest`, `DraftRequest` and
`TimelineEntry` stay exactly as they are, so this is a Consumer-side collapse only.

Unit tests: the set is two specs, both at `ToolScope::Agent(target)` read from
`ctx.leader.target()`; `propose_claim` with kind `requirement` and no cites is refused;
`propose_claim` with a structural kind writes `claim/proposed` and never an op; `curate` with only
placements, only a timeline, and both, each writes what the two old tools wrote; `curate` with
neither is refused; `leader_swap.rs`'s `LEADER_ONLY` list is updated to the new names and every
existing case still passes; `15-leader-swap.sh`'s `PROBE_TOOL` becomes `curate`.

### WP-7: the TUI program row
**Files:** `plugins/tui-focus/src/program.rs`, `plugins/tui-focus/src/rows.rs` (the `Row::Program`
variant and its fold ONLY), `plugins/tui-focus/src/expand.rs` (program disclosure ONLY),
`plugins/tui-focus/tests/program.rs`, `scripts/tui/30-program.sh`.

A new `Row::Program { call, source, console, subs: Vec<ProgramSub>, result, ops, ms }` folded from
the `tool/call` whose name is `RUN_TOOL`, its `program/*` steps and its `tool/result` — one row, the
existing "no step is rendered twice" rule. Collapsed it is one line
(`⏵ program · 4 calls · 1.2s`); expanded it shows the JS source as a highlighted block (syntect,
already a dependency), the console output beneath it, and the sub-calls as nested rows reusing the
existing tool-row disclosure and its ✓/✗ marks. `Row::Other` remains the fallback, so an unknown
`program/*` field never panics. Keys and mouse reuse `expand.rs`; nothing else in `tui-focus` moves.

Unit tests: the fold produces one row for a program with four sub-calls and zero orphan rows; a
program with no sub-calls folds to one row; a `program/error` renders the typed error line; an
unknown sub-step kind renders as `Other` and does not panic; the collapsed line's width is stable
under a narrow terminal. `scripts/tui/30-program.sh` (shell-use, both consumers via
`$BOUGH_CONSUMER`): `program_row_is_collapsed_by_default`, `enter_expands_the_js_block`,
`console_output_is_under_the_source`, `nested_rows_carry_check_marks`,
`collapse_restores_one_row`, and under `typed` — `no_program_row_and_plain_tool_rows_instead`.

### WP-8: the bench, the rows, the swap gate, and the record
**Files:** `bench/tools/{Cargo.toml,src/lib.rs,src/bank.rs,src/run.rs,src/report.rs}`,
`bench/tools/bank/*.yml` (≥12 tasks), `bench/tools/fixtures/{typed,codemode}/*.yml` (replay
transcripts), `bench/tools/tests/{bank,replay,live}.rs`, `Makefile` (`bench-tools`),
`Cargo.toml` (workspace member `bench/*`), `bundles/bough-codemode.yml`, `profiles/codemode.yml`,
`crates/bough/tests/{codemode_swap,codemode_wake,codemode_invariants,docs}.rs`,
`scripts/tui/31-codemode-swap.sh`, `docs/codemode-merge-notes.md`, `docs/phase-codemode-plan.md`
(the results table), `REQUIREMENTS.md` §18, `BUILD.md`.

The bank is ≥12 tasks over a fixed fixture repo, covering: three file edits (create, hash-anchored
patch, multi-file all-or-nothing), two multi-step shell tasks, a search-then-edit, a worker spawn, an
`ask`, a `claim`, an `act` against the recording `gh` shim, a `ledger` drill, and a scheduled intent.
Each task declares its pass predicate as data (files' contents, steps appended, journal rows) — never
a model judgement. The runner boots the headless profile twice per task, once per consumer patch,
against `llm-replay` transcripts recorded per consumer, and reports pass rate, steps per task, input
and output tokens, and $ from `model-policy`'s price table. `BOUGH_LIVE=1` swaps `llm-replay` for
`llm-anthropic` on haiku for both arms and prints the same table. `make bench-tools` follows the
existing `make bench` shape (`BOUGH_BENCH=1 cargo test -p bough-bench-tools -- --ignored --nocapture`);
it is NOT in `make gates`.

Unit tests: the bank has ≥12 tasks and its declared coverage set names every surface entry of §3;
each fixture transcript replays deterministically twice with identical results; the report's $
arithmetic matches the price table on a hand-computed case. Integration
(`crates/bough/tests/codemode_swap.rs`, the phase's SWAP exit gate) and `docs.rs` are the
verification map below. `docs/codemode-merge-notes.md` records the two wanted hooks of §0.1 and the
`ctx.schedule` handoff of §2.4 with file, signature and why.

---

## 5. Verification map

Every bullet of the brief → the test that proves it. A name in `backticks` is a test function; a
`.sh` name is a shell-use script whose bullets are listed.

**V1 — the row mounts by patch in place of the typed consumer, with no change to the tools seam.**
- `crates/bough/tests/codemode_swap.rs::the_codemode_row_mounts_by_patch_and_the_seam_rows_stay_active`
- `crates/bough/tests/codemode_swap.rs::the_model_is_shown_exactly_one_tool_under_code_mode`
- `plugins/tools-codemode/tests/surface.rs::the_request_shows_run_alone_and_the_program_still_reaches_everything`
- `plugins/tools-codemode/tests/surface.rs::a_lane_restricted_tool_is_absent_from_the_globals_and_not_found_at_the_mirror`
- `crates/bough/tests/codemode_swap.rs::every_visible_spec_is_a_function_in_the_sandbox_and_a_restricted_one_is_not` (the same two claims against the REAL QuickJS sandbox and the booted tree)
- `plugins/tools-codemode/tests/pipeline.rs::the_four_tools_events_fire_for_every_inner_call`
- `plugins/tools-codemode/tests/pipeline.rs::a_pre_execute_deny_rejects_the_promise_and_lands_a_denied_program_result`
- `crates/bough/tests/codemode_swap.rs::unmounting_the_row_restores_the_typed_schemas_exactly`

**V2 — model-visible ⟺ ledgered under code mode.**
- `plugins/tools-codemode/tests/ledgered.rs::the_console_chunks_reconstruct_the_tool_result_and_every_call_is_answered`
  (the program text, the sub-steps and the console are all steps under the program, and the
  reconstruction is exact)
- `plugins/tools-codemode/tests/ledgered.rs::a_truncated_program_still_reconstructs_from_its_chunks`
- `plugins/tools-codemode/tests/ledgered.rs::a_program_that_throws_still_ledgers_what_it_did`
- `plugins/tools-codemode/src/invariant.rs::the_invariant_catches_a_planted_console_divergence`
  (the pure predicate). The clause it guards was tautological until the 2026-08-28 review:
  `Run::call` built `Obs { console, result_content }` from ONE clone, so the two could never
  differ whatever the consumer did. `run.rs` now re-reads the calls, the results and the console
  from the LEDGER and records the string the model actually received, so the two observations are
  independent.

**V3 — the sandbox is closed; caps terminate; `bash` is the only command path.**
(the plan filed these under `plugins/js-quickjs/tests/{closed,caps}.rs`; they are inline in
`plugins/js-quickjs/src/engine.rs`, next to the module they cover, per AGENTS.md, plus the
booted-tree cases in `crates/bough/tests/codemode_closed.rs`)
- `plugins/js-quickjs/src/engine.rs::the_ambient_world_is_empty`
- `plugins/js-quickjs/src/engine.rs::the_whole_global_surface_is_pure_builtins_plus_the_bound_names`
- `plugins/js-quickjs/src/engine.rs::no_file_network_env_or_process_access_is_possible`
- `plugins/js-quickjs/src/engine.rs::importing_a_module_rejects`
- `plugins/js-quickjs/src/engine.rs::an_infinite_loop_hits_the_ops_cap`
- `plugins/js-quickjs/src/engine.rs::a_huge_allocation_hits_the_memory_cap`
- `plugins/js-quickjs/src/engine.rs::a_busy_loop_past_wall_ms_hits_the_time_cap`
- `plugins/js-quickjs/src/engine.rs::deep_recursion_hits_the_stack_cap`
- `crates/bough/tests/codemode_closed.rs::no_file_network_env_or_process_access_exists_except_through_injected_functions`
- `crates/bough/tests/codemode_closed.rs::a_program_past_the_ops_cap_is_terminated_and_lands_a_typed_program_error_step`
- `crates/bough/tests/codemode_closed.rs::a_program_past_the_time_cap_is_terminated_and_lands_a_typed_program_error_step`
- `plugins/tools-codemode/tests/ledgered.rs::a_cap_breach_lands_a_program_error_step_and_a_failed_tool_result`
- `crates/bough/tests/codemode_closed.rs::every_command_is_a_ledgered_program_call_with_its_tags`
- `plugins/tools-codemode/tests/pipeline.rs::an_untagged_bash_call_is_still_refused_and_lands_no_step`
- `plugins/tools-codemode/tests/pipeline.rs::a_tagged_bash_call_reaches_a_tool_that_declares_no_tags`

**V4 — main's patch grammar works verbatim.**
- `plugins/tools-operator/tests/patch_grammar.rs::view_renders_the_anchor_and_numbered_lines`
- `…::swap_replaces_the_named_range`, `…::del_removes_the_named_range`,
  `…::ins_pre_and_ins_post_land_on_the_right_side_of_a_line`,
  `…::ins_head_and_ins_tail_bracket_the_file` (the six operations, in viewed coordinates)
- `…::earlier_operations_do_not_shift_later_line_numbers`
- `plugins/tools-operator/tests/files.rs::a_multi_file_patch_is_all_or_nothing`
- `plugins/tools-operator/tests/files.rs::a_stale_tag_is_refused_and_nothing_is_written`
- `plugins/tools-operator/tests/files.rs::an_untouched_range_rebases_onto_a_file_that_moved_since_the_view`
- `plugins/tools-operator/tests/files.rs::a_file_this_agent_never_viewed_is_refused`
- `plugins/tools-operator/tests/files.rs::write_creates_a_file_and_its_tag_is_accepted_without_a_re_view`
- `plugins/tools-codemode/tests/section.rs::the_patch_grammar_appears_exactly_once_in_a_whole_assembled_projection`

**V5 — the rest of the surface works from a program.** All of these run REAL JavaScript in QuickJS
against the real tools over their real seams, in `plugins/tools-codemode/tests/v5_surface.rs`
(the plan filed them under `surface.rs`; they are their own binary because the fixture mounts
`tools-operator`, `tool-actions`, `tool-workers`, `claims`, `actions`, `workers` and `agents`):
- `…/v5_surface.rs::ledger_search_steps_and_tail_drill_from_a_tier_to_raw_steps`
- `…/v5_surface.rs::inbox_returns_the_unconsumed_mail`
- `…/v5_surface.rs::a_claim_from_a_program_lands_as_claim_proposed`
- `…/v5_surface.rs::act_open_pr_goes_through_the_actions_journal` (gh shim Provider)
- `…/v5_surface.rs::agent_ask_and_fork_go_through_the_workers_seam` (`ask` runs inside the
  worker's own program, so the caller really has a spawner)
- `…/v5_surface.rs::the_worker_bounds_refuse_the_cap_plus_one_spawn`
- `…/v5_surface.rs::a_scheduled_intent_from_a_program_fires_on_the_synthetic_clock`
- `…/v5_surface.rs::the_bundle_binds_the_documented_names` (the alias map under test IS the
  bundle's)
- `plugins/tools-operator/tests/schedule.rs::an_intent_at_t_plus_5m_fires_exactly_once_and_wakes_its_creator`

Proving V5 needed an implementation fix, not just a test: an alias was a bare rename, so the
op-discriminated tools could not be reached under the names the surface documents
(`ledger.search(q)` bound `q` to `op`, and `act(kind, …)` did not exist at all). An alias value
now reads as `tool?fixed=value#positional,names` or as an `a|b|c` DISPATCH on the first argument,
and `bundles/bough-codemode.yml` binds `ledger.search/steps/tail`, `bg`/`bg.output`/`bg.kill` and
`act` with it. `ledger_read` also learned `{op: steps, range: "1204..1230"}`, which is how a tier's
notable refs spell a range.

**V6 — turn end without a stop tool.**
- `crates/bough/tests/codemode_wake.rs::a_program_then_text_wake_ends_by_wake_stopping`
- `crates/bough/tests/codemode_wake.rs::a_program_that_calls_nothing_still_ends_its_step`
- `crates/bough/tests/codemode_wake.rs::no_stop_tool_is_registered_by_either_consumer`
- `crates/bough/tests/codemode_wake.rs::a_wake_never_hangs_waiting_for_a_stop`
- `plugins/tools-codemode/tests/pipeline.rs::run_never_reports_concludes_wake_unless_an_inner_result_did`
  and `plugins/agent-loop/tests/flow.rs::a_concludes_wake_tool_result_ends_the_wake_at_its_step` —
  the two halves of "a concluding inner result ends the wake at its step". On the real binary only
  the NEGATIVE half is decidable, and that is what
  `crates/bough/tests/codemode_wake.rs::a_non_concluding_program_does_not_end_the_wake_at_its_step`
  asserts; **no end-to-end case in this phase drives the positive one.**

**V7 — `tool-leader` collapsed to two.**
- `plugins/tool-leader/tests/tools.rs::the_set_is_propose_claim_and_curate`
- `plugins/tool-leader/tests/tools.rs::propose_claim_absorbs_draft_requirement_and_propose_structure`
- `plugins/tool-leader/tests/tools.rs::curate_absorbs_adopt_unsorted_and_note_timeline`
- `crates/bough/tests/leader_swap.rs` (all cases, `LEADER_ONLY` updated) — the Phase 5 swap test
- `scripts/tui/15-leader-swap.sh` (`PROBE_TOOL=curate`)
- `crates/bough/tests/codemode_swap.rs::the_five_old_spellings_are_gone_from_both_consumers`

**V8 — the TUI renders a program step.**
- `plugins/tui-focus/tests/program.rs::a_program_with_four_sub_calls_folds_into_one_row`
- `plugins/tui-focus/tests/program.rs::a_program_with_no_sub_calls_folds_into_one_row`
- `plugins/tui-focus/tests/program.rs::expanded_shows_the_source_then_the_console_then_the_nested_rows`
- `plugins/tui-focus/tests/program.rs::collapsing_a_program_collapses_its_sub_rows`
- `plugins/tui-focus/tests/program.rs::a_program_error_renders_the_typed_error_line`
- `plugins/tui-focus/tests/program.rs::unknown_and_orphaned_sub_steps_render_as_other`
- `scripts/tui/30-program.sh` — bullets `program_row_is_collapsed_by_default`,
  `enter_expands_the_js_block`, `console_output_is_under_the_source`,
  `nested_rows_carry_check_marks`, `collapse_restores_one_row`; run under both
  `$BOUGH_CONSUMER=codemode` and `=typed` (where `no_program_row_and_plain_tool_rows_instead`).

**V9 — the bench.**
- `bench/tools/tests/bank.rs::the_bank_has_at_least_twelve_tasks_covering_every_surface`
- `bench/tools/tests/replay.rs::bench_tools_runs_the_bank_through_both_consumers_offline`
  (the `make bench-tools` entry point; prints pass rate / steps / tokens / $ per consumer)
- `bench/tools/tests/live.rs::bench_tools_live_haiku_bank` (`#[ignore]`, `BOUGH_LIVE=1`, both arms)
- the numbers land in §8 "Bench results" of this file.

**V10 — the record.**
- `crates/bough/tests/docs.rs::requirements_18_cites_headlongs_design_docs`
- `crates/bough/tests/docs.rs::the_plan_records_the_decisions_for_andrey_with_evidence`
- `crates/bough/tests/docs.rs::the_build_row_says_the_default_consumer_is_unchanged`
- `crates/bough/tests/docs.rs::the_integration_report_is_not_the_stale_draft` (§9 is read by V10
  too — the drift the map exists to catch was in the file the map lives in)

**SWAP (the phase exit gate) — switch the consumer row while the tree runs.**
- `crates/bough/tests/codemode_swap.rs::a_patch_switches_the_consumer_and_the_next_wake_uses_the_other_surface`
- `crates/bough/tests/codemode_swap.rs::the_tools_seam_rows_stay_active_and_nothing_is_failed`
  (asserts `tools`, `tools-baseline`, `tools-operator`, `tool-mcp`, `tool-actions`, `tool-workers`,
  `tool-leader` are ACTIVE before, during and after)
- `crates/bough/tests/codemode_swap.rs::switching_back_restores_the_typed_schemas`
- `scripts/tui/31-codemode-swap.sh` — bullets `typed_rows_before_the_patch`,
  `program_row_after_the_patch`, `no_failed_row_in_the_status_line`, `typed_rows_again_after_revert`

---

## 6. Decisions taken where REQUIREMENTS is silent

Labelled as such, each with the reason it went that way.

- **D-1. Sub-steps use `program/call` / `program/result`, not `tool/call` / `tool/result`.**
  §3 leaves step-type naming to the owning plugin. The reason is mechanical (§4): `transcript::rebuild`
  folds those two kinds unconditionally, so reusing them would break V2 with no way to fix it from
  this branch. Reversible post-merge via `sub_step: bool`.
- **D-2. The `run` `tool/call` step IS the program step.** No separate `program` step type carrying
  the source a second time: the model-visible call already carries it, so duplicating it would put
  the same bytes in two places with no reader for the second.
- **D-3. Concealment is a mirror registry, not a `Restrict`.** §5's `restrict` must refuse execution;
  code mode needs prompt-invisible-but-executable. The interim (§0.1) is a snapshot under a lock; the
  end state is a `conceal` hook. Recorded in `docs/codemode-merge-notes.md`.
- **D-4. Console is streamed as `program/console` chunks AND is the `tool/result` content.** The
  duplication is deliberate and *checked* — the crate's invariant asserts the chunks reassemble into
  the result — because the TUI must show output as it is produced and the model must receive it once.
- **D-5. Inner call ids are `{run_call_id}.{n}`.** Deterministic, so a replayed transcript produces a
  byte-identical ledger, which is what the bench's two-run determinism test needs.
- **D-6. `tags` is required on `bash`/`sh` and becomes `Ref`s on the `program/call` step.** Main's
  tag memory does not exist in the rebuild; §3's `step_refs` is canonical for matching and routing,
  so a dotted tag (`pr.456`, `linear.eng-1234`) joining a command to its work is free. A bare-string
  `sh` leg is refused rather than warned about (main's warning produced untagged rows nobody found).
- **D-7. `bg`, `ledger_read` and `act` are one tool each with an `op`/`kind` discriminator.** The
  brief prescribes it for `act`; the same argument (one schema, one row in the registry, one render
  intent) applies to the other two.
- **D-8. `tools-operator` mounts in `bough-base`, not in the codemode bundle.** `view`/`patch`/
  `write` are a better editing surface than `edit_file` under BOTH consumers, and the bench compares
  surfaces, not tool inventories. If Andrey wants the arms to differ in tools too, that is one row
  moved between bundles.
- **D-9. Barriers inside a program.** §9's `is_concurrency_safe` rule is per dispatch batch; a
  program issues one call per host-fn invocation. Reproduced with a per-program `RwLock`: safe calls
  take a read, everything else takes a write. So `Promise.all([bash, bash])` serialises exactly as a
  parallel typed batch of two `bash` calls would.
- **D-10. `schedule` fires from a watcher inside `tools-operator`, with an injected clock.**
  `ctx.schedule` does not exist until Phase 7; a ledger step nobody reads would make V5 unprovable.
  The watcher is ~80 lines and is deleted when the seam arrives (`inject: optional ["schedule"]`).
- **D-11. The leader's `propose_claim` keeps `contradiction` and `other`.** The brief names five
  kinds; the global `propose_claim` admits three, and the scoped one shadows it, so dropping two
  would REMOVE capability from the leader. Union kept: `lane|split|merge|bud|requirement|
  contradiction|other`. Flagged for Andrey in §7 in case the narrowing was intended.
- **D-12. The `js` seam ships with one Provider.** §0.2 says don't split preemptively; the split is
  justified by a *named* second provider (main's sidecar) that is deliberately NOT built. This is
  also the concrete case behind the swap-gate question in §7.
- **D-13. `Row::Program` lives in `tui-focus`, keyed on `RUN_TOOL`.** `RenderIntent` has three
  variants and lives in `plugins/tools`, which this branch may not edit; keying the fold on the
  consumer's protocol constant avoids a fourth variant and keeps the render decision in the surface.

---

## 7. Decisions for Andrey (recorded, no action taken)

**A. The default consumer.** Unchanged: `tools-typed` (today's rows). `tools-codemode` mounts only
via `profiles/codemode.yml` or a `--patch`. `BUILD.md`'s phase row says so, and
`crates/bough/tests/docs.rs::the_build_row_says_the_default_consumer_is_unchanged` holds it there.
The evidence for a switch is §8's table; the GO is yours.

**B. Swap-gate policy — stop requiring a second provider per seam.** Seven crates exist only to
prove a swap, carrying ten plugin names between them: `plugins/hello` (`hello`, `greeting-echo`,
`greeting-shout`), `plugins/ledger-memory`, `plugins/llm-replay`, `plugins/agent-loop-scripted`,
`plugins/rollups-none`, `plugins/projection-probe`, `plugins/tui-probe` (`tui-probe`, `tui-never`).
Of these, `ledger-memory`, `llm-replay` and `agent-loop-scripted` **earn their keep as test
infrastructure**
(fast golden projections, the entire hermetic suite, the loop-substitution proof) and should be kept
regardless. The rest are swap-gate ballast. Proposal: keep every one of them for tests, and **retire
"a second Provider exists" as a per-phase gate**, replacing it with two checks that already have
teeth — `scripts/audit-plugins.sh` (the tree settles with any one row disabled) plus a new
Cargo-manifest assertion that no Consumer crate depends on a concrete Provider crate. This phase is
the case in point: `js`
is a real seam with one Provider and a named, unbuilt second, and building a sidecar to satisfy a
gate would cost a week and prove nothing the trait does not already prove.

**C. Workflow / deterministic-replay row.** No such row exists in the rebuild, and REQUIREMENTS never
names one — main had workflows (`ToWorkflowWorker`, journal replay, `phase`/`log` host fns) and they
were not carried over. Options: (i) leave it out permanently and let `schedule` + wards cover the
need; (ii) add it in Phase 7 as a runtime-code host row alongside `wards-rhai`; (iii) build it now as
a third consumer surface. Recommendation: **(i)**, on the evidence that nothing in Phases 0–5 wanted
it and that §9's ward host plus `schedule` covers every use main's workflows served. Decide
explicitly either way; the current silence reads as an oversight and is not.

**D. `edit_file`.** `tools-baseline`'s `edit_file(old, new)` stays registered (this branch may not
edit that crate) but is not documented in the code-mode surface. If code mode becomes the default,
removing it from `tools-baseline` is a one-row change and the right follow-up.

**E. The leader's kind set** — see D-11.

---

## 8. Bench results

`make bench-tools`, the 15-task bank, both consumers, re-run 2026-08-27 on this branch AFTER the
merge-note §9 fix (two code-mode rows moved from NO to yes: `04-multi-step-shell` and
`07-search-then-edit`).
Produced by `bench/tools/tests/replay.rs::bench_tools_runs_the_bank_through_both_consumers_offline`.

**Read the replay numbers as a SHAPE, not as a price.** The offline arm answers from recorded
transcripts, so the token counts are the ones the fixtures DECLARE — they are what a plausible
round of each shape costs, not what a model spent. What the replay run measures honestly is the
number of ROUNDS each surface needs for the same work (that is a property of the transcript's
shape, which is the thing being compared) and, above all, whether each task's data predicate holds.
The live table below is the one with real tokens in it.

| arm | pass | steps/task | in tok | out tok | $ / bank | $ / task |
|---|---|---|---|---|---|---|
| typed, replay | 12/15 | 22.7 | 222300 | 4960 | $0.2471 | $0.0165 |
| codemode, replay | 10/15 | 21.1 | 170800 | 5700 | $0.1993 | $0.0133 |

| task | arm | pass | steps | in | out | $ | note |
|---|---|---|---|---|---|---|---|
| 01-write-creates-a-file | typed | yes | 17 | 9300 | 200 | $0.0103 |  |
| 02-hash-anchored-patch | typed | yes | 23 | 15300 | 340 | $0.0170 |  |
| 03-multi-file-all-or-nothing | typed | yes | 29 | 22200 | 480 | $0.0246 |  |
| 04-multi-step-shell | typed | yes | 29 | 22200 | 480 | $0.0246 |  |
| 05-parallel-shell-legs | typed | yes | 29 | 22200 | 480 | $0.0246 |  |
| 06-background-job | typed | NO | 29 | 22200 | 480 | $0.0246 | src/bg.txt: No such file or directory (os error 2) |
| 07-search-then-edit | typed | yes | 29 | 22200 | 480 | $0.0246 |  |
| 08-spawn-a-worker | typed | NO | 17 | 6000 | 200 | $0.0070 | 0 `worker/started` steps, wanted at least 1 |
| 09-ask-a-question | typed | yes | 17 | 9300 | 200 | $0.0103 |  |
| 10-propose-a-claim | typed | yes | 18 | 9300 | 200 | $0.0103 |  |
| 11-act-open-pr | typed | NO | 17 | 9300 | 200 | $0.0103 | 0 `action/intent` steps, wanted at least 1 |
| 12-ledger-drill | typed | yes | 29 | 22200 | 480 | $0.0246 |  |
| 13-read-the-inbox | typed | yes | 23 | 15300 | 340 | $0.0170 |  |
| 14-schedule-an-intent | typed | yes | 18 | 9300 | 200 | $0.0103 |  |
| 15-fork-the-trajectory | typed | yes | 17 | 6000 | 200 | $0.0070 |  |
| 01-write-creates-a-file | codemode | yes | 20 | 12000 | 380 | $0.0139 |  |
| 02-hash-anchored-patch | codemode | yes | 23 | 12000 | 380 | $0.0139 |  |
| 03-multi-file-all-or-nothing | codemode | yes | 26 | 12000 | 380 | $0.0139 |  |
| 04-multi-step-shell | codemode | yes | 24 | 12000 | 380 | $0.0139 |  |
| 05-parallel-shell-legs | codemode | NO | 18 | 12000 | 380 | $0.0139 | src/one.txt: No such file or directory (os error 2) |
| 06-background-job | codemode | NO | 20 | 12000 | 380 | $0.0139 | src/bg.txt: No such file or directory (os error 2) |
| 07-search-then-edit | codemode | yes | 26 | 12000 | 380 | $0.0139 |  |
| 08-spawn-a-worker | codemode | NO | 20 | 7400 | 380 | $0.0093 | 0 `worker/started` steps, wanted at least 1 |
| 09-ask-a-question | codemode | yes | 20 | 12000 | 380 | $0.0139 |  |
| 10-propose-a-claim | codemode | yes | 21 | 12000 | 380 | $0.0139 |  |
| 11-act-open-pr | codemode | NO | 18 | 12000 | 380 | $0.0139 | 0 `action/intent` steps, wanted at least 1 |
| 12-ledger-drill | codemode | NO | 18 | 12000 | 380 | $0.0139 | src/drill.txt: No such file or directory (os error 2) |
| 13-read-the-inbox | codemode | yes | 22 | 12000 | 380 | $0.0139 |  |
| 14-schedule-an-intent | codemode | yes | 21 | 12000 | 380 | $0.0139 |  |
| 15-fork-the-trajectory | codemode | yes | 20 | 7400 | 380 | $0.0093 |  |

### Live haiku, both arms — SUPERSEDED (run BEFORE the merge-note §9 fix)

**Kept for the record only. The table below was measured with the shell surface broken: every
`bash`/`sh` call in the sandbox was refused, so the code-mode arm could not run a command at all.
The section AFTER it is the decision table.**

`BOUGH_LIVE=1 make bench-tools`, run 2026-08-27, 394s wall, both arms on
`claude-haiku-4-5-20251001` for sol and terra. Produced by
`bench/tools/tests/live.rs::bench_tools_live_haiku_bank`. **These are the real tokens; §7.A is
decided on this table, not on the replay one.**

| arm | pass | steps/task | in tok | out tok | $ / bank | $ / task |
|---|---|---|---|---|---|---|
| typed, live haiku | 11/15 | 48.1 | 501548 | 9099 | $0.5470 | $0.0365 |
| codemode, live haiku | 9/15 | 92.0 | 1137502 | 17646 | $1.2257 | $0.0817 |

| task | arm | pass | steps | in | out | $ | note |
|---|---|---|---|---|---|---|---|
| 01-write-creates-a-file | typed | yes | 20 | 6781 | 120 | $0.0074 |  |
| 02-hash-anchored-patch | typed | yes | 68 | 48015 | 832 | $0.0522 |  |
| 03-multi-file-all-or-nothing | typed | NO | 139 | 182504 | 2065 | $0.1928 | src/a.txt is not the expected text |
| 04-multi-step-shell | typed | yes | 28 | 11525 | 262 | $0.0128 |  |
| 05-parallel-shell-legs | typed | yes | 52 | 26316 | 676 | $0.0297 |  |
| 06-background-job | typed | yes | 36 | 17511 | 336 | $0.0192 |  |
| 07-search-then-edit | typed | yes | 37 | 16672 | 440 | $0.0189 |  |
| 08-spawn-a-worker | typed | NO | 29 | 11214 | 315 | $0.0128 | 0 `worker/started` steps, wanted at least 1 |
| 09-ask-a-question | typed | yes | 22 | 6708 | 201 | $0.0077 |  |
| 10-propose-a-claim | typed | yes | 40 | 17084 | 574 | $0.0200 |  |
| 11-act-open-pr | typed | NO | 107 | 83117 | 1778 | $0.0920 | 0 `action/intent` steps, wanted at least 1 |
| 12-ledger-drill | typed | yes | 31 | 16353 | 405 | $0.0184 |  |
| 13-read-the-inbox | typed | NO | 29 | 11492 | 204 | $0.0125 | src/inbox.txt does not contain `0` |
| 14-schedule-an-intent | typed | yes | 21 | 6927 | 173 | $0.0078 |  |
| 15-fork-the-trajectory | typed | yes | 62 | 39329 | 718 | $0.0429 |  |
| 01-write-creates-a-file | codemode | yes | 21 | 12446 | 88 | $0.0129 |  |
| 02-hash-anchored-patch | codemode | yes | 32 | 20627 | 223 | $0.0217 |  |
| 03-multi-file-all-or-nothing | codemode | yes | 36 | 21234 | 269 | $0.0226 |  |
| 04-multi-step-shell | codemode | NO | 136 | 150717 | 2350 | $0.1625 | src/counts.txt: No such file or directory (os error 2) |
| 05-parallel-shell-legs | codemode | yes | 31 | 13735 | 238 | $0.0149 |  |
| 06-background-job | codemode | yes | 475 | 323204 | 4860 | $0.3475 |  |
| 07-search-then-edit | codemode | NO | 219 | 277631 | 4841 | $0.3018 | src/notes.md does not contain `MARKER_TWO` |
| 08-spawn-a-worker | codemode | NO | 32 | 20558 | 267 | $0.0219 | 0 `worker/started` steps, wanted at least 1 |
| 09-ask-a-question | codemode | yes | 23 | 12599 | 209 | $0.0136 |  |
| 10-propose-a-claim | codemode | NO | 184 | 168298 | 2254 | $0.1796 | 0 `claim/proposed` steps, wanted at least 1 |
| 11-act-open-pr | codemode | NO | 48 | 29601 | 700 | $0.0331 | 0 `action/intent` steps, wanted at least 1 |
| 12-ledger-drill | codemode | yes | 40 | 23913 | 497 | $0.0264 |  |
| 13-read-the-inbox | codemode | NO | 35 | 21491 | 254 | $0.0228 | src/inbox.txt does not contain `0` |
| 14-schedule-an-intent | codemode | yes | 33 | 20926 | 344 | $0.0226 |  |
| 15-fork-the-trajectory | codemode | yes | 35 | 20522 | 252 | $0.0218 |  |


**The headline of THAT run, kept because it is what a broken host function costs: code mode
LOST.** 9/15 against 11/15, and $1.23 against $0.55 for the same fifteen tasks — 2.2x the money and
2.3x the tokens. That is the opposite of main's 14/16 @ $0.042 vs Claude Code's 16/16 @ $0.076, and
it is the number the GO has to answer to.

Where the money went is legible and is NOT the surface being inherently dearer. On the eleven tasks
where neither arm ran away, code mode is *cheaper per task* than typed and needs fewer rounds — 01,
02, 03, 05, 09, 12, 14, 15 all pass under code mode at $0.013–$0.027, and `03-multi-file-all-or-
nothing` is the clearest case in the bank: code mode passes it for $0.023 while the TYPED arm fails
it after 139 steps and $0.19. The whole of the arm's deficit is four runaway rows —
`06-background-job` (475 steps, $0.35), `07-search-then-edit` (219 steps, $0.30),
`10-propose-a-claim` (184 steps, $0.18) and `04-multi-step-shell` (136 steps, $0.16) — which between
them are $1.01 of the arm's $1.23. Three of the four are the model retrying a host call that keeps
failing, which is merge-note §9 (the shell surface) reached from the other side: with no working
`bash`, a program that needs the shell rewrites itself until the budget runs out, where a typed
`bash` call fails once and the model moves on. A single failing call is cheap; a failing call inside
a program the model keeps re-authoring is not.

So the honest reading is: **the comparison is not yet decidable, and the phase should not be asked
for a GO on it.** What it does establish is (a) the surface works end to end on a live model, (b) on
tasks whose host calls succeed it is cheaper and shorter than typed, and (c) code mode is far more
sensitive to a broken host function than typed tools are — which is a real finding about the design,
worth keeping whatever the fix does to the numbers. Rerun once merge-note §9 lands. — **it has, and the rerun is below.**

### Live haiku, both arms — the DECISION table

`BOUGH_LIVE=1 make bench-tools`, run 2026-08-27 after the merge-note §9 fix (the tag argument is
taken at the code-mode boundary, so `bash` is callable from the sandbox), 298s wall, both arms on
`claude-haiku-4-5-20251001` for sol and terra. Produced by
`bench/tools/tests/live.rs::bench_tools_live_haiku_bank`. **These are the real tokens, and this is
the table §7.A is decided on.**

| arm | pass | steps/task | in tok | out tok | $ / bank | $ / task |
|---|---|---|---|---|---|---|
| typed, live haiku | 11/15 | 49.6 | 760628 | 9931 | $0.8103 | $0.0540 |
| codemode, live haiku | 11/15 | 50.8 | 565613 | 8954 | $0.6104 | $0.0407 |

| task | arm | pass | steps | in | out | $ | note |
|---|---|---|---|---|---|---|---|
| 01-write-creates-a-file | typed | yes | 20 | 6821 | 120 | $0.0074 |  |
| 02-hash-anchored-patch | typed | yes | 85 | 66187 | 1036 | $0.0714 |  |
| 03-multi-file-all-or-nothing | typed | yes | 124 | 109803 | 1949 | $0.1195 |  |
| 04-multi-step-shell | typed | yes | 36 | 16822 | 337 | $0.0185 |  |
| 05-parallel-shell-legs | typed | NO | 31 | 13508 | 453 | $0.0158 | src/one.txt is not the expected text |
| 06-background-job | typed | yes | 37 | 17171 | 407 | $0.0192 |  |
| 07-search-then-edit | typed | yes | 37 | 16707 | 476 | $0.0191 |  |
| 08-spawn-a-worker | typed | NO | 28 | 11264 | 238 | $0.0125 | 0 `worker/started` steps, wanted at least 1 |
| 09-ask-a-question | typed | yes | 23 | 6735 | 212 | $0.0078 |  |
| 10-propose-a-claim | typed | yes | 46 | 24479 | 646 | $0.0277 |  |
| 11-act-open-pr | typed | NO | 144 | 407396 | 2543 | $0.4201 | 0 `action/intent` steps, wanted at least 1 |
| 12-ledger-drill | typed | yes | 37 | 22066 | 459 | $0.0244 |  |
| 13-read-the-inbox | typed | NO | 28 | 11418 | 208 | $0.0125 | src/inbox.txt does not contain `0` |
| 14-schedule-an-intent | typed | yes | 21 | 6858 | 169 | $0.0077 |  |
| 15-fork-the-trajectory | typed | yes | 47 | 23393 | 678 | $0.0268 |  |
| 01-write-creates-a-file | codemode | yes | 21 | 12438 | 96 | $0.0129 |  |
| 02-hash-anchored-patch | codemode | yes | 34 | 20788 | 237 | $0.0220 |  |
| 03-multi-file-all-or-nothing | codemode | yes | 36 | 21220 | 271 | $0.0226 |  |
| 04-multi-step-shell | codemode | NO | 116 | 106846 | 1571 | $0.1147 | src/counts.txt is not the expected text |
| 05-parallel-shell-legs | codemode | yes | 29 | 13739 | 256 | $0.0150 |  |
| 06-background-job | codemode | yes | 133 | 128069 | 2681 | $0.1415 |  |
| 07-search-then-edit | codemode | yes | 76 | 52331 | 696 | $0.0558 |  |
| 08-spawn-a-worker | codemode | NO | 65 | 50773 | 554 | $0.0535 | 0 `worker/started` steps, wanted at least 1 |
| 09-ask-a-question | codemode | yes | 23 | 12631 | 202 | $0.0136 |  |
| 10-propose-a-claim | codemode | yes | 48 | 29869 | 521 | $0.0325 |  |
| 11-act-open-pr | codemode | NO | 36 | 20345 | 355 | $0.0221 | 0 `action/intent` steps, wanted at least 1 |
| 12-ledger-drill | codemode | yes | 54 | 41633 | 684 | $0.0451 |  |
| 13-read-the-inbox | codemode | NO | 36 | 21300 | 254 | $0.0226 | src/inbox.txt does not contain `0` |
| 14-schedule-an-intent | codemode | yes | 22 | 12859 | 189 | $0.0138 |  |
| 15-fork-the-trajectory | codemode | yes | 33 | 20772 | 387 | $0.0227 |  |

**With a working shell, the two arms TIE on pass rate and code mode is 25% cheaper: 11/15 at
$0.6104 against 11/15 at $0.8103, on 26% fewer input tokens and 10% fewer output tokens, at the
same rounds per task (50.8 vs 49.6).** Every one of the four tasks that made the earlier run's
deficit — 04, 06, 07, 10 — moved: three now PASS, and the arm's most expensive row fell from $0.35
to $0.14.

Two failures are shared by both arms and are therefore about the BANK or the tree, not the surface:
`08-spawn-a-worker` (merge-note §7 — `spawn_worker` is dead under both consumers because
`agent-loop`'s `inject()` omits `workers`) and `13-read-the-inbox`. `11-act-open-pr` fails under
both too, and it is where the typed arm burns $0.42 of its $0.81 in one 144-step runaway; code mode
fails the same task in 36 steps for $0.02. The one place code mode is now clearly worse is
`04-multi-step-shell`, its single runaway (116 steps, $0.11).

**The honest reading.** This is one seed, one bank, one small model, and two of the four failures
are the harness's rather than either surface's — so it is not a mandate. What it does say, on real
tokens, is that code mode is no longer behind: it matches typed on tasks solved and does it for a
quarter less money, it is markedly cheaper on the tasks where a typed agent runs away, and its own
runaway mode is a single program re-authored against a failing call. The finding from the earlier
run stands and is now the main design caveat: **code mode is far more sensitive to a broken host
function than typed tools are**, because a failing call inside a program the model keeps
re-authoring costs a whole round each time. The GO is Andrey's; this table is the input.

### What the red rows mean

Seven of the ten failures are the SAME THREE DEFECTS, none of them in this phase's files, and all
three are written up in `docs/codemode-merge-notes.md`:

| red rows | why | note |
|---|---|---|
| `08-spawn-a-worker`, both arms | `agent-loop`'s `inject()` does not declare `workers`, and tools execute under the loop's context, so `spawn_worker` answers `workers seam unavailable`. No agent can spawn a worker through the tool surface today, under EITHER consumer. | §7 |
| `11-act-open-pr`, both arms | there is no `actions` Provider until Phase 6, so `open_pr` is not a registered tool. Red by construction; the recording `gh` shim is the guard that keeps it honest when the executor lands. | run.rs::gh_shim |
| `04`, `05`, `12`, `07`, codemode only | the code-mode SHELL surface is unsatisfiable: with `tags_required` on, no registered tool has a `tags` property (`tools-baseline`'s `bash` is `{command, cwd}`, `tools-operator` registers no `bash`/`sh`), so every command in the sandbox is refused. | §9 |
| `06-background-job`, both arms | the transcript does not wait for the background job before the answer round, so the file the predicate reads is not there yet. A bank bug, not a product one. | — |

**The comparison that is currently legible.** On the eight tasks where both arms are exercising a
surface that WORKS (01, 02, 03, 09, 10, 13, 14, 15), both arms pass all eight, and code mode does it
in fewer rounds: the replay fixtures need one `run` round where the typed surface needs two or three
typed calls. That is the CodeAct result reproduced in shape, and it is also the whole of what this
run can claim. **It is not yet an answer to §7.A**: with the shell surface dead, seven of fifteen
tasks are not comparing consumers at all. Rerun `make bench-tools` once merge-note §9 is fixed; the
numbers below are the baseline that rerun is measured against.

## 9. What actually landed (integration report, rewritten 2026-08-28)

This section is REWRITTEN. Its 2026-08-27 text was written mid-run, while a full data volume on the
shared machine was blocking six of the eight work packages, and it said the phase had delivered
almost nothing: `plugins/js`, `plugins/js-quickjs`, `plugins/tools-codemode` and `bench/tools` were
"scaffold only — signatures, doc comments and `todo!()`", "no bundle or profile row references
them", WP-6 and WP-8 were "not started", "§8's table is therefore still empty", and the code-mode
arm of `scripts/tui/30-program.sh` "has never run". All of that is false at HEAD,
and the section sat two pages below a §8 carrying two full bench tables. It is recorded here
because it is exactly the drift V10's `crates/bough/tests/docs.rs` exists to catch — and that test
read §7, §8, §18 and `BUILD.md` and never §9. `docs.rs::the_integration_report_is_not_the_stale_draft`
now reads this section too, and `docs.rs::every_test_the_verification_map_names_exists` walks §5's
55 names against the tree (19 of them named no test when the review found it).

**The state of the branch**

- **WP-1 `plugins/js`** — the seam: the `js` service key, `Program`/`HostFn`/`Caps`/`Run`/`JsError`,
  the single-engine factory slot (a second engine is an error, the disposer frees the slot), and
  the `a_cancelled_program_never_reports_a_run` invariant (renamed after the review: the
  "exactly one terminal outcome" clauses were unfalsifiable — one `Result` cannot be both or
  neither — and were replaced by the two an engine can really get wrong). Bodies, not signatures.
- **WP-2 `plugins/tools-codemode`** — the Consumer: `run(program)` as an ordinary `ToolSpec`, the
  mirror snapshot + `Restrict{allow:{run}}` concealment, the binding derivation (aliases with fixed
  arguments and positional names, `a|b|c` dispatch, `mcp__` namespacing), the `program/call`,
  `program/result`, `program/console` and `program/error` steps, the console tee, and the
  `every_program_call_is_ledgered_and_console_reconstructs_the_result` invariant.
- **WP-3 file verbs + main's patch grammar** (`plugins/tools-operator/src/files/**`) — a verbatim
  port of `main:crates/bough-core/src/hostfn/patch.rs`'s pure half: `normalize`, `tag_of` (FNV-1a
  over UTF-16 code units, 4 hex), `parse_patch` (all six ops, the lenient range spellings, the
  Codex envelope, every corrective refusal message), `check_ops`, `materialize`, `rebase.rs`
  (`line_map` prefix/suffix trim + LCS, `LCS_CAP = 400`) and an `apply` that decides every file
  before it writes any. 53 tests in `plugins/tools-operator/tests/{patch_grammar.rs,files.rs}`.
- **WP-4 `plugins/js-quickjs`** — the embedded engine over `rquickjs` (pinned `0.12`,
  `features = ["futures"]`): ops/memory/stack/wall caps, the interrupt handler, the closed-world
  globals, `preflight`'s syntax diagnostics, and the host-call bridge.
- **WP-5 the surface** — `plugins/tools-codemode/src/surface/`: one projection section assembled
  from seven prose files plus a roster GENERATED from the live registry, with main's patch grammar
  restored verbatim. The prose is GATED per binding (`<!-- needs: … -->`), so a verb the registry
  does not offer is neither listed nor taught. The roster is what
  `CodemodeConfig::surface_bindings` returns, which is also what the sandbox injects: the bundle's
  aliases and namespaces applied, and its `hide: [read_file, write_file, edit_file, glob, grep]`
  removed — the brief's "drop as separate functions", since `bash` + `rg` and `view`/`patch`/
  `write` cover them and `edit_file(old, new)` is the regression the patch grammar exists to
  avoid. `plugins/tools-codemode/tests/section.rs` pins it, byte-stable, against a roster derived
  from the tool names this tree actually registers, through the config the bundle actually ships.
- **WP-6 the leader's five → two** — `plugins/tool-leader::TOOL_NAMES` is `["propose_claim",
  "curate"]`.
- **WP-7 the TUI program row** — `plugins/tui-focus/src/program.rs`, `rows.rs`, `expand.rs`: a
  `program` step folds into one collapsible row carrying its source, its console output and its
  `tool/call` sub-rows; `check_frame`'s "no step rendered twice" holds over the fold.
  `plugins/tui-focus/tests/program.rs` is green, and the code-mode arm of
  `scripts/tui/30-program.sh` runs inside `make gates` (`Makefile`, the
  `BOUGH_CONSUMER=codemode` pass) — it is no longer typed-arm-only.
- **WP-8 the bench** — `bench/tools` with the task bank, the two arms and `make bench-tools`; §8
  above carries its two tables.

`grep -rn 'todo!\|unimplemented!' plugins/ bench/` returns nothing. `bundles/bough-codemode.yml`
(the three rows `js`, `js.quickjs`, `tools.codemode`) and `profiles/codemode.yml` exist, and
`crates/bough/tests/docs.rs::no_shipped_profile_boots_the_codemode_consumer` is what keeps the
DEFAULT profile off them: the consumer is reachable only by `--profile codemode` or an explicit
patch, which is decision A of §7.

**Fixed after the review (2026-08-28), in the surface**

The seven prose files were concatenated unconditionally while only the bullet roster was
registry-driven, so the section taught `await sh(…)` at length and `await act(…)` in full while
neither is ever injected in this tree (no row registers `sh`; no action kind has a Provider). A
model that followed the doc called a name that is not there, the ReferenceError was uncaught, and
the whole program and round were lost — the runaway §8's live table charges $0.11–$0.35 a task
for. The prose is now gated per binding (`<!-- needs: a,b -->` / `<!-- needs-any: … -->` markers
read by `surface::gate`), so a verb that is not injected is neither listed nor taught, and a lane
with `deny: [bash]` is no longer handed 100 lines about `bash`.
`tests/section.rs::every_function_the_prose_teaches_is_injected` walks the assembled body and
demands every `await name(` be a real global; `::the_roster_names_only_tools_this_tree_registers`
stops the fixture inventing one (it used to carry `sh` and `open_pr` by hand); and the fixture's
config is now DESERIALISED from `bundles/bough-codemode.yml` and derived through
`CodemodeConfig::surface_bindings`, the same call the sandbox makes. The recorded section size
moves **4257 → 3846 tokens** with the dead prose gone, and much lower for a restricted lane — so
code mode's per-request overhead in §8's tables is that much lower than they were measured at.

**What is still open** — the red bench rows of §8 (the shell surface under `tags_required`, the
missing `workers` declaration in `agent-loop`'s `inject()`, and the absent actions Provider), all
three written up in `docs/codemode-merge-notes.md`. §7.A's GO is still Andrey's.

**Also fixed after the review (2026-08-28), in the engine**

A program killed by the wall clock or a cancel used to leave its in-flight host call RUNNING: the
body is spawned on the caller's runtime and `run_one`'s `select!` only drops the future. The call
then finished and appended its `program/result` after the round's closing `tool/result` — the very
ordering D-1 calls a fact of the ledger — and left the consumer's `Obs` with a call and no result,
reported as a product violation when it was a race. `call_host` now holds the `JoinHandle` in an
abort-on-drop guard.
`plugins/js-quickjs/src/engine.rs::a_timed_out_program_leaves_no_host_call_running_behind_it`
fails on the old code and passes on the new.

**Deviations from the plan that the next run should carry**

- `files::specs(cfg, root: WorkspaceRoot, seen)` and `files::apply_patch(input, root, agent, seen)` —
  the plan omitted the workspace root (nothing in `OperatorConfig` names a directory) and the agent
  (`SeenFiles` is keyed by `(AgentName, PathBuf)`).
- `PatchError` gained `Denied { path, detail }` (containment → `FailureClass::Denied`) and `Conflict`
  gained `detail`; `PatchOp` keeps main's `a`/`b`/`at` shape, not the skeleton's `from`/`to`.
- `preflight::scan(&str) -> Option<Finding>` / `diagnose(&Finding)` **cannot** express main's
  shadowed-binding message, which depends on the engine's own error text and the injected host-fn
  names. The port needs `syntax_message(why: &str, src: &str, bound: &[String]) -> JsError`, with the
  scanner kept as an internal helper. **Done, with a second half the note missed** (2026-08-28):
  `preflight::syntax_error_message` had the signature but nothing ever passed it a roster —
  `JsEngine::check` parsed with an EMPTY bound list and the consumer preflights every program and
  returns on the error, so the branch was dead. The seam now carries
  `JsHandle::check_bound(src, caps, bound)` / `JsEngine::check_bound` (defaulting to `check`, so no
  other engine changes), QuickJS parses with the roster, and `preflight` recovers the identifier
  from the source for QuickJS's nameless "invalid redefinition of lexical identifier".
  `plugins/js-quickjs/src/engine.rs::check_bound_names_the_bound_identifier_a_program_redeclared`
  pins it. **Closed at the 2026-08-28 close**: `Run::call` preflights through
  `js.check_bound(&source, caps, &bound)` with the names it is about to inject, and
  `plugins/tools-codemode/tests/ledgered.rs::the_preflight_is_given_the_names_the_sandbox_will_inject`
  pins the wiring (it reads the roster the engine was handed, and shadowing one is refused by name).
- `Row::Program` carries `error: Option<ProgramError>` and `parts: Vec<StepId>` beyond the brief's
  field list; the second is what makes `check_frame` non-vacuous for a multi-step fold.
- `ms` is read from the closing `tool/result`'s `value.ms` and `ops` only from `program/error`;
  `tools-codemode` must put both `ms` and `ops` in a successful `run` result's `value`, or a
  successful program's collapsed line shows no duration. **Closed at the 2026-08-28 close**:
  `Run::call`'s success arm returns `value: Some({ms, ops})`, pinned by
  `plugins/tools-codemode/tests/ledgered.rs::a_successful_program_reports_its_ms_and_ops_in_the_result_value`
  — the product now writes the body `plugins/tui-focus/tests/program.rs` renders.
  `scripts/tui/30-program.sh` still asserts only `2 calls`, so the TUI half of the duration is
  proved by the unit fixture and not end to end.
- WP-8's runner should drive `target/release/bough` as a **subprocess** rather than linking the
  launcher, so the bench's compilation is not coupled to WP-1…WP-7. `bench/tools/arms/{typed,
  codemode}.yml` are named by `run.rs` and do not exist yet.

**Two `make gates` failures inherited from phase ux1, fixed here** (neither is a code-mode change;
both are assertions that could not pass whatever the product did, and the ux1 close committed "as-is
after the close agent overran"):

- `scripts/tui/19-interrupt.sh` — `the_farewell_is_one_line_and_the_screen_is_not_blank` counted
  `bough: bye.` over the whole screen, and the script exits bough **twice** (the Ctrl+C path, then
  `/quit`). Two real farewells from two real exits read as a banner. The PTY's primary buffer is
  cleared between the two sessions now.
- `scripts/tui/23-commands.sh` — three of them. `slash_opens_a_palette_that_filters_and_moves`
  asserted that Down moves the selection *in the filtered list*, but `he` narrows the palette to
  exactly one row; the move half now runs on the unfiltered list, before the filter half, and Up
  restores the selection. Its filter half asserted `/quit` disappears — `/quit` sorts below the
  ten-row cut and was never on screen. `enter_accepts_the_palette_selection` pressed Enter on a
  palette whose selected row was `/accept` and looked for `help`. `an_unknown_command_suggests_and_
  keeps` pressed Enter inside the window where `/` had just opened the palette, so the miss was
  never submitted. All twelve bullets are green, stable over three runs.

**Known flake, not fixed here**: `scripts/tui/03-scroll-and-copy.sh`'s
`the_wheel_scrolls_the_trajectory` failed once under load with a wholly blank screen (the binary had
not painted its first frame yet), and passed four consecutive runs alone afterwards. It is a boot
race in the script's settle, not a scroll bug; whoever owns `03` should give it the same
`shell-use wait idle` the later scripts use before the first wheel event.

## 10. Deviations and open items (close, 2026-08-28)

Everything below is HONEST about what a test proves. A bullet that names no test is a claim about
the tree, not about behaviour.

### Fixed at the close

- **`cargo fmt`** on `plugins/js-quickjs/src/engine.rs` (two call sites the parallel fixers left
  unformatted; `make lint` was red on them).
- **A test-isolation bug the review's own fix introduced.**
  `plugins/tools-codemode/tests/support/mod.rs` recorded the preflight roster in a single
  last-one-wins cell, and the test binary runs its cases concurrently, so
  `the_preflight_is_given_the_names_the_sandbox_will_inject` read another case's roster (`["slow"]`).
  It is a LOG of `(src, bound)` now, read back by `support::preflighted_with(src)`. Green:
  `cargo test -p bough-plugin-tools-codemode --test ledgered` (8 passed).
- **`plugins/js-quickjs/src/engine.rs::every_runtime_is_dropped` was racy against the process-global
  live count.** It captured `before` while other cases had runtimes alive and then demanded the
  count come back to that transient number. It now waits for a settled baseline and waits for the
  count to RETURN to it (`wait_for_live`), which is the falsifiable half — a runtime that outlives
  its program never lets the count come back. Green: `cargo test -p bough-plugin-js-quickjs`
  (37 passed).
- **A duplicate race test removed.** Two parallel fixers each wrote the concealment race case;
  `crates/bough/tests/codemode_race.rs` was deleted and
  `crates/bough/tests/codemode_conceal_race.rs::the_first_request_of_a_freshly_created_agent_is_already_concealed`
  kept, because that one was verified RED on the old `agent/created` wiring while the other's own
  header admits it "does not reliably go red on the old wiring".
- **`run.rs`'s no-op `drop(tee.clone())`** and the comment that described an effect it did not have
  are gone; the comment now says what actually closes the channel. Behaviour-neutral, covered by
  the existing `ledgered.rs` drain cases.

### Low-severity findings recorded, NOT acted on

Each is a real observation from the review; none is fixed, and none is covered by a test.

1. **`plugins/tools-codemode/src/lib.rs` leaks the four `program/*` step-type declarations on
   purpose** (the token is dropped so the declaration outlives an unload), which is a stated
   exception to §0.2's "unloading a plugin unwinds its effects LIFO". The reason is sound — a
   trajectory that once ran a program cannot be rebuilt by a binary that has forgotten the type,
   which made the consumer swap one-way — but it is recorded only in a code comment and
   `docs/codemode-merge-notes.md` §10. **Wants Andrey's explicit blessing at merge**, not a silent
   precedent.
2. **`Cargo.toml`'s `rquickjs` contradicts REQUIREMENTS §13's Avoid list** ("mlua/piccolo/wasm
   runtimes and rune … embedded-VM isolation solves a problem this single-user harness does not
   have"). §18 was amended for the code-mode references and §0.4's "Not taken from dsh: … the Code
   Mode SDK" was left standing; §13 was not touched. Per AGENTS.md, REQUIREMENTS wins over code:
   **the Avoid clause should be amended as part of the GO**, not left contradicting the tree. The
   pin itself (`"0.12"`) is the minor pin §13 asks for.
3. **Two boundary slips in `run.rs`.** `Run::caps` defaults with `unwrap_or_else(|| self.js.
   default_caps())` inside the call path rather than in a named `resolve`; and the `program/console`
   drain (`run.rs`) plus `append_error` swallow ledger append failures with `let _ =`, so a chunk
   the model receives can fail to reach the ledger with no signal — the one direction
   "model-visible ⟺ ledgered" forbids. The inner-call path gets this right (a refused append is a
   `HostRefusal`); the console path does not.
4. **Three silent skips decide the model's whole surface.** An alias naming an absent tool is
   dropped without a warning (`bind.rs`), and `conceal::visible_specs` `continue`s past a name whose
   `resolve` fails or whose schema will not parse. A typo in a bundle therefore removes a function
   from both the sandbox and the documented roster, silently.
5. **`Concealment` is never pruned.** `live` (an `EffectHandle`) and `cached` (a cloned
   `Vec<ToolSpec>`) are keyed by `AgentName`; the row does not listen for `AgentDisposed`, which the
   agents seam does emit. Every `agent()`/`fork()` worker leaves both behind for the life of the row.
6. **The bench arms differ by more than the consumer row.** `bench/tools/arms/{typed,codemode}.yml`
   are byte-identical, but the code-mode arm runs under `tags_required: true`, so its `bash` refuses
   an untagged call and the typed arm's does not. It is defensible as part of the surface, but §8's
   numbers are read as like-for-like and `bench/tools/src/run.rs` claims the arms "differ by ONE row
   … and by nothing else". **Read §8 with this in mind.**
7. **`js-quickjs`'s live-runtime invariant is a process-global `AtomicI64`** with no fiber or kernel
   scoping, checked at `Cadence::OnQuiesce`. Two kernels in one binary, or a program still on its
   detached `bough-js` thread while another part of the tree quiesces, are counted against each
   other. The close fixed the TEST that reads it (above); the registered invariant itself is still
   unscoped.

### Still open from the build (unchanged by the close)

- `ConcealMode::Seam` is rejected at load behind the `seam-conceal` feature because the seam call it
  needs does not exist; `docs/codemode-merge-notes.md` names the hook.
- The mirror's JS caps (`inner_deadline_ms`, `max_parallel_calls`) are validated config set by hand
  in `bundles/bough-codemode.yml` to match the seam's values. There is no
  `ToolsHandle::default_deadline_ms()` / `max_parallel()` to read them from, and the deadline
  plumbing and the parallelism semaphore have **no direct runtime test** — only
  `tests::the_tunables_are_bounded_at_load` covers the config. Merge notes §12.
- `tools-operator`'s `schedule` still runs a 100 ms polling watcher instead of registering against a
  kernel timer: there is no `ctx.schedule` and no `schedule-cron` Provider in the tree, and
  `crates/bough-kernel` is off-limits to this track. Merge notes §3 carries the wanted signature;
  deleting the loop is a one-file change once the hook lands.
- Two bugs the bench found in rows this track may not edit: `agent-loop` reads `workers` without
  declaring it (so `spawn_worker` is dead under BOTH consumers), and there is no `actions` Provider
  until Phase 6 (so the `act` bank task is red by construction).
- **Known flake**: `crates/bough/tests/codemode_wake.rs::a_program_then_text_wake_ends_by_wake_stopping`
  failed once and passed on rerun; recorded in merge notes §11.
- **A second boot-race flake, pre-existing and not code-mode's**:
  `crates/bough/tests/exec_headless.rs::exec_exits_with_the_ledger_intact` failed one `make gates`
  run with `bough exec: no agent factory is set; mount an `agent-loop` row` while three tracks were
  building on the same machine, and passed alone immediately after (5 passed). It is the same boot
  race `bench/tools/src/run.rs` already classifies and retries
  (`a_run_that_died_before_the_agent_factory_mounted_is_a_boot_race`); `exec_headless.rs` has no
  such retry and should get the same settle.
- `scripts/tui/03-scroll-and-copy.sh`'s `the_wheel_scrolls_the_trajectory` is a pre-existing boot
  race inherited from phase ux1, not a code-mode change.
