# Phase c — digging panes, the plugin audit, hardening: design and work breakdown

**Scope.** REQUIREMENTS §17 Phase 8, minus the FTS search pane (shipped in Phase 3 as `tui-search`)
and minus the adversarial boundary review (Phase 6's, and there is no `actions-github` on this
branch to review). What is left, and what this phase builds:

1. `tui-preview` — the byte-exact projection preview pane (§11 "Digging", §5).
2. `tui-timeline` — the cross-agent chronological timeline with composable filters (§11, §17 Phase 5's
   "the timeline surface arrives Phase 8").
3. `tui-drift` — the drift dashboard over `drift-watch`'s per-agent signals, with the reset reachable
   from it (§8).
4. `scripts/audit-plugins.sh` + `make audit-plugins` — the EVERYTHING-IS-A-PLUGIN AUDIT (§17 Phase 8,
   §16).
5. Hardening: crash-during-a-wake reconciliation, failure injection, spawn-bound storms, llm failure
   as a terminal chunk (§17 Phase 8, §7, §12).
6. The event catalog gate (§15 item 7).
7. shell-use scripts for the three panes (§11 "Testing discipline").

**Track rules this plan obeys.** This branch (`rebuild-c`) runs in parallel with the track-B merge
and the code-mode track. Everything below is a NEW crate, a NEW file, a NEW row in an existing
bundle, or a NEW test. **No existing crate's `src/` is edited.** Where a pane wants a seam method
that does not exist, this plan builds against what exists and records the exact hook in
`docs/track-c-merge-notes.md` (§7 of this document). The three shared files this phase touches, and
the whole of the edit: `Cargo.toml` (one `members` line for `crates/xtask`), `bundles/bough-tui-app.yml`
(three rows appended), `Makefile` (`audit-plugins` re-pointed, one `events` target). WP-7 owns all
three, so no two packages ever edit one file.

---

## 1. Crate list

| Crate (package) | Catalog name | Row id | Bundle | `inject` | provides |
|---|---|---|---|---|---|
| `plugins/tui-preview` (`bough-plugin-tui-preview`) | `tui-preview` | `tui.preview` | `bough-tui-app` | req `tui`, `projection`, `ledger`; opt `agents`, `commands` | — (Consumer of the `projection` seam) |
| `plugins/tui-timeline` (`bough-plugin-tui-timeline`) | `tui-timeline` | `tui.timeline` | `bough-tui-app` | req `tui`, `ledger`; opt `agents`, `commands` | — (Consumer of `ledger`) |
| `plugins/tui-drift` (`bough-plugin-tui-drift`) | `tui-drift` | `tui.drift` | `bough-tui-app` | req `tui`, `drift`; opt `agents`, `commands` | — (Consumer of `drift`) |
| `plugins/fault-inject` (`bough-plugin-fault-inject`) | `fault-inject` | — | **catalog only, no bundle** | opt `projection`, `tools`, `agents` | — |
| `plugins/actions-shim` (`bough-plugin-actions-shim`) | `actions-shim` | — | **catalog only, no bundle** | req `actions` | registers four `ActionKind`s on the `actions` Provider registry |
| `crates/xtask` (`xtask`) | — (not a plugin) | — | — | — | `cargo xtask events`; a lib + a bin, no `ctx` key |

`fault-inject` and `actions-shim` follow the `ledger-memory` / `projection-probe` / `agent-loop-scripted`
precedent: compiled into the binary's catalog, named by **no** bundle, mounted by a test's or a
script's own `--patch`. `--dump-config` on a shipped profile never shows them.

**Seam roles (§0.2).** None of the three panes is a Service Definition or a Provider. Each is a
**Consumer** of an existing seam, which is why none of them owns a `ctx` key: `tui-preview` consumes
`projection`, `tui-timeline` consumes `ledger`, `tui-drift` consumes `drift`. `actions-shim` is a
second **Provider** on the `actions` seam (the first real one arrives in Phase 6), which is what
gives the audit's provider half something to swap there.

**Invariant modules (§0.2).** `tui-preview/src/invariant.rs`: every `preview/taken` render whose
`as_of` names a `request/header` in the ledger carries that header's `projection_digest` — the pane
cannot render bytes the ledger does not describe. `tui-timeline/src/invariant.rs`: the rendered row
set is a subset of the queried step set and is strictly non-decreasing in `(at, traj, seq)` —
a timeline that invented or reordered a row is reported. `tui-drift/src/invariant.rs`:
`No runtime invariant: the pane owns no event stream and no data relation; every number it renders
is `drift-watch`'s, and `drift-watch` already checks them.` `fault-inject`: `No runtime invariant:
the row exists to violate things on purpose; an invariant over it would assert its own faults.`
`actions-shim/src/invariant.rs`: one `gh` invocation per `action/intent` idem key over the process's
whole life — the "never re-executed" fact, checked continuously rather than only by V3.

---

## 2. Public API

Everything below is what independent implementers program against. Types not shown are imported
unchanged from the crates that already own them.

### 2.1 `tui-preview`

```rust
pub const PLUGIN_NAME: &str = "tui-preview";
pub const PANE_ID: &str = "tui.preview";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewConfig {
    /// Rows the pane asks for when the Aux band has room.
    pub height: u16,
    /// Terminal ROWS below which this pane costs zero (SlotSize::Responsive's `collapse`).
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Debounce on `ledger/step` before re-assembling. Assembly is deterministic but not free.
    pub refresh_ms: u64,
    /// Hard cap on rendered characters, so a 160k-token projection cannot stall a frame.
    pub max_chars: usize,
}

/// WHICH ledger high-water the preview assembles at. The pane's whole honesty question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewAt {
    /// `as_of = ledger.head_seq(traj)`: what the agent would see if it woke this instant —
    /// before the wake writes its own `wake/start`, mail deliveries and `step/start`.
    Head,
    /// A named high-water: exactly the value a past wake's `request/header.as_of` carries.
    Seq(bough_plugin_ledger::Seq),
}

/// One taken preview. `text` is THE byte-exact surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub agent: AgentName,
    pub at: PreviewAt,
    pub as_of: Seq,
    /// `Assembled::to_text()`, and nothing else — the same bytes `agent-loop`'s
    /// `request::build` puts in `LlmRequest::system`.
    pub text: String,
    pub tokens: usize,
    pub budget: usize,
    pub flags: std::collections::BTreeSet<bough_plugin_projection::Flag>,
    /// `(section id, tokens)`, render order.
    pub sections: Vec<(SectionId, usize)>,
    /// sha256 hex of `text`; equals `request/header.projection_digest` for the same `as_of`.
    pub digest: String,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// Take a preview. The ONLY call that reaches the seam; every other function here is pure.
pub async fn snapshot(
    projection: &ProjectionHandle,
    ledger: &LedgerHandle,
    agent: &AgentName,
    at: PreviewAt,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Snapshot, PreviewError>;

/// PURE: the system prefix a request built from `a` carries. One line, and it exists so the
/// claim "the pane and the loop spell this the same way" is a call and not a comment.
pub fn system_prefix(a: &Assembled) -> String;              // == a.to_text()

/// PURE: sha256 hex. Same spelling as `agent_loop::request::digest`.
pub fn digest(text: &str) -> String;

/// The step kinds a wake appends BEFORE it assembles, in order (§5's wake flow steps 3–5).
/// The one place the preview's stated caveat is spelled.
pub const WAKE_PREFACE_KINDS: [&str; 3] = ["wake/start", "mail/delivered", "step/start"];

/// PURE: the lines a later assembly added over an earlier one, oldest first. Used by the header
/// ("+3 preface rows at wake") and by the V1b test.
pub fn added_lines(before: &str, after: &str) -> Vec<String>;

/// PURE: whether every added line is a tail line for one of [`WAKE_PREFACE_KINDS`].
pub fn only_preface(added: &[String]) -> bool;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("no agent named `{0}`")] NoSuchAgent(String),
    #[error("agent `{0}` has no trajectory")] NoTrajectory(String),
    #[error(transparent)] Projection(#[from] bough_plugin_projection::ProjectionError),
    #[error(transparent)] Ledger(#[from] bough_plugin_ledger::LedgerError),
}
```

Pane behaviour: `Slot::Aux`, `order: 10`, `SlotSize::Responsive { collapse: collapse_rows,
preferred: height, min: min_rows, max: max_rows }`, `focusable: true`, title `preview`. Header line:
`preview · <agent> · as_of <seq> · <tokens>/<budget> tok · <digest[..8]> · +N preface rows at wake`.
Keys: `↑/↓/PgUp/PgDn` scroll, `t` toggles `PreviewAt::Head` ⇄ `PreviewAt::Seq(anchored step's
header as_of)`, `y` copies the whole text through `TuiHandle::copy`, `Esc` → `PaneOutcome::Handled`
(the shell then returns the keyboard to the composer; §ux1's Esc rule, unchanged). Command:
`/preview [agent]` → `TuiHandle::focus_pane(PANE_ID)` and a fresh snapshot.

### 2.2 `tui-timeline`

```rust
pub const PLUGIN_NAME: &str = "tui-timeline";
pub const PANE_ID: &str = "tui.timeline";

#[derive(...)] #[serde(deny_unknown_fields)]
pub struct TimelineConfig {
    pub height: u16, pub collapse_rows: u16, pub min_rows: u16, pub max_rows: u16,
    /// Newest steps read PER TRAJECTORY before filtering. The read bound.
    pub window: usize,
    /// Rows rendered after filtering. The render bound.
    pub limit: usize,
    pub debounce_ms: u64,
    /// `chrono` format for the time column.
    pub time_format: String,
}

/// One row of the timeline: a step, and whose it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Row { pub agent: AgentName, pub traj: TrajId, pub step: Step }

/// The composable filter. EVERY populated field is a CONJUNCT; an empty field is "no filter"
/// (the `StepQuery` precedent). Within a field the members are a disjunction, so
/// `agent:sol agent:terra type:tool/call` means (sol ∨ terra) ∧ tool/call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub agents: BTreeSet<AgentName>,
    pub refs:   BTreeSet<Ref>,
    pub kinds:  BTreeSet<StepType>,
    pub class:  Option<Class>,
    pub since:  Option<DateTime<Utc>>,   // inclusive
    pub until:  Option<DateTime<Utc>>,   // exclusive
}

impl Filter {
    /// PURE: the ∧ of the five dimensions.
    pub fn matches(&self, row: &Row) -> bool;
    pub fn is_empty(&self) -> bool;
    /// What the pane's header prints: `agent:sol ∧ ref:pr/1204 ∧ type:tool/call ∧ since:2h`.
    pub fn describe(&self) -> String;
    /// The parts that can be pushed into a `StepQuery` (trajs, kinds, class). `since`/`until`
    /// are NOT pushed: `StepQuery` has no time bounds (decision D-C4).
    pub fn to_query(&self, trajs: Vec<TrajId>, window: usize) -> StepQuery;
}

/// PURE — **the** timeline. A total order over rows from any number of trajectories:
/// `step.at` ascending, ties broken by `(traj, seq)`. Filtered, then truncated to the NEWEST
/// `limit` rows, then returned oldest-first. A pure function of `(rows, filter, limit)`.
pub fn timeline(rows: &[Row], f: &Filter, limit: usize) -> Vec<Row>;

/// PURE: the filter grammar.
/// `agent:sol ref:pr/1204 type:tool/call class:evidence since:2h until:2026-08-27T10:00:00Z`
/// `since`/`until` take an RFC3339 instant or a relative span (`15m`, `2h`, `3d`).
/// An unknown word is an ERROR naming the word — never a silently ignored filter (§16).
pub fn parse_filter(q: &str, now: DateTime<Utc>) -> Result<Filter, FilterError>;
/// PURE: round-trips with [`parse_filter`] for every filter [`parse_filter`] can produce.
pub fn render_filter(f: &Filter, now: DateTime<Utc>) -> String;

/// PURE: one rendered line, clipped to `cols`:
/// `12:04:31  sol   tool/call     bash(cargo test -p bough)      pr/1204`
pub fn line(row: &Row, cols: u16, time_format: &str) -> String;
/// PURE: the hit id a row records. `tl:<step id>`.
pub fn hit_of(row: &Row) -> HitId;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum FilterError {
    #[error("`{0}` is not a filter; try agent:/ref:/type:/class:/since:/until:")] UnknownWord(String),
    #[error("`{word}`: {detail}")] BadValue { word: String, detail: String },
    #[error("since {since} is after until {until}")] EmptyWindow { since: String, until: String },
}
```

Pane behaviour: `Slot::Aux`, `order: 20`, the same `Responsive` size shape, title `timeline`.
The pane owns a one-line filter editor (the `tui-search` query precedent): typing edits the filter
string, `Enter` parses it (a parse error renders in the header, in the theme's error role, and the
previous filter stays live), `Esc` clears the editor if it is non-empty and otherwise dismisses the
pane. Click on a row → `PaneOutcome::Focus(FocusRequest { agent: Some(id), step: Some(step_id),
pane: transcript })`, exactly as `tui-search` focuses a hit. Command: `/timeline [filter…]`.

### 2.3 `tui-drift`

```rust
pub const PLUGIN_NAME: &str = "tui-drift";
pub const PANE_ID: &str = "tui.drift";

#[derive(...)] #[serde(deny_unknown_fields)]
pub struct DriftPaneConfig {
    pub height: u16, pub collapse_rows: u16, pub min_rows: u16, pub max_rows: u16,
    /// Most agents shown; the rest are a `… N more` line.
    pub agents_shown: usize,
    pub refresh_ms: u64,
    /// Columns the tool-share bar gets.
    pub bar_cols: u16,
    /// Milliseconds the reset stays armed after the first `r` (decision D-C5).
    pub arm_ms: u64,
}

/// PURE: one dashboard row, from `Signals` alone. No clock, no ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct DashRow {
    pub agent: AgentName,
    pub samples: usize,
    pub thought_cv: f64,
    pub tool_entropy: f64,
    pub top_tools: Vec<ToolShare>,
    pub claim_rejection: SignalState,
    pub flags: Vec<DriftFlag>,
    pub verdict: Verdict,
}

/// What the glyph column says. `TooFewSamples` is NOT `Steady` (§16: uncertainty never becomes
/// assertion) and is not `Flagged` either.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict { Steady, Watch, Flagged, TooFewSamples }

pub fn dash_row(s: &Signals) -> DashRow;
pub fn verdict(s: &Signals) -> Verdict;
/// PURE: the rendered line, clipped to `cols`. `∅` for an inactive signal, never `0.00`.
pub fn line(r: &DashRow, cols: u16, bar_cols: u16) -> String;
/// PURE: `share` as a `cols`-wide bar. Total: 0.0 and 1.0 both render.
pub fn bar(share: f64, cols: u16) -> String;
/// PURE: the exact command line the pane dispatches for a row's reset. THE reachability of §8's
/// one-command reset from the dashboard, spelled once so the test and the pane agree.
pub fn reset_command(agent: &AgentName) -> String;   // "/reset sol"
/// PURE: the two-step arm. `Arm` on the first `r`, `Fire` on the second within `arm_ms`.
pub fn arm(prev: Option<(AgentName, DateTime<Utc>)>, agent: &AgentName,
           now: DateTime<Utc>, arm_ms: u64) -> ResetStep;
pub enum ResetStep { Arm, Fire }
```

Pane behaviour: `Slot::Aux`, `order: 30`, same size shape, title `drift`. Keys: `↑/↓` move the row
focus, `r` arms then fires the reset, `Esc` disarms if armed and otherwise dismisses. Firing returns
`PaneOutcome::Command(reset_command(&agent))`, which the shell dispatches through `ctx.commands` to
`drift-watch`'s existing `/reset` — the pane adds no new write path. Clicking the `[reset]` region
(hit id `drift:reset:<agent>`) does the same, also two-step. Command: `/driftboard [agent]` focuses
the pane (`/drift` already belongs to `drift-watch` and is left alone).

### 2.4 `fault-inject`

```rust
pub const PLUGIN_NAME: &str = "fault-inject";

#[derive(...)] #[serde(deny_unknown_fields)]
pub struct FaultConfig {
    /// WHERE. Exactly one site per row, so a test names what it broke.
    pub at: FaultSite,
    /// HOW.
    pub how: FaultKind,
    /// Fire on the Nth hit of the site, 1-based. What makes "and the loop CONTINUES" observable:
    /// fail wake 1, pass wake 2. A protocol counter, not a deployment tunable.
    pub after: u32,
    /// Fire this many times then stop. `0` = forever.
    pub times: u32,
    /// Restrict to one agent. `None` = every agent.
    pub agent: Option<AgentName>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaultSite {
    /// The row's own `apply` fails: the fiber goes FAILED. §7's "a row whose fiber FAILS is
    /// reported, not retried into a loop".
    Apply,
    /// A contributed projection section whose render returns `Err` — a plugin fiber failing
    /// mid-wake, at the point the wake is assembling its request.
    ProjectionSection,
    /// A registered tool whose execute returns `Err` / panics.
    ToolExecute,
    /// A `agent/wake-stopping` serial listener that fails.
    WakeStopping,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind { Error, Panic }

/// Process-global counters the tests read. `applies()` is the "not retried" evidence.
pub fn hits(site: FaultSite) -> u32;
pub fn applies() -> u32;
pub fn reset();
/// A test that mounts this row holds this for its whole body (the `hello::trace::test_lock`
/// precedent): the counters are process-global.
pub fn test_lock() -> std::sync::MutexGuard<'static, ()>;
```

### 2.5 `actions-shim`

```rust
pub const PLUGIN_NAME: &str = "actions-shim";

#[derive(...)] #[serde(deny_unknown_fields)]
pub struct ShimConfig {
    /// The binary invoked for GitHub kinds. `gh` — a test puts a RECORDING SHIM first on PATH
    /// (AGENTS.md: tests never call the real `gh`).
    pub gh: String,
    /// Which of the four kinds this Provider claims. Default: all four.
    pub kinds: Vec<ActionKind>,
    /// A sleep INSIDE `execute`, before the outward call. The window `kill -9` lands in for the
    /// "killed between the intent row and the outward act" half of V3.
    pub delay_before_ms: u64,
    /// A sleep after the outward call and before `action/done`. The other half of V3.
    pub delay_after_ms: u64,
}

/// The Provider. Registers through `ActionsHandle::provider` like any Phase-6 row will.
pub struct GhShimProvider { cfg: Arc<ShimConfig> }

#[async_trait::async_trait]
impl bough_plugin_actions::ActionProvider for GhShimProvider {
    fn kinds(&self) -> Vec<ActionKind>;
    /// Embeds `req.marker` in the artifact (PR body / commit trailer / comment suffix) exactly as
    /// §7 requires, so reconciliation is a lookup against the world.
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError>;
}
```

### 2.6 `xtask events` (§15 item 7)

```rust
// crates/xtask — a LIB plus a bin. `cargo xtask events [--check] [--write <path>]`.

#[derive(Clone, Debug, PartialEq)]
pub struct EventDecl {
    pub name: String,                        // the `const NAME` literal
    pub ty: String,                          // the impl's Self type
    pub trait_mode: DispatchMode,            // from the trait: Emit/Parallel/Serial/Waterfall
    pub declared_mode: Option<DispatchMode>, // an explicit `const MODE = …`, when present
    pub krate: String, pub file: PathBuf, pub line: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum SiteKind { Dispatch, Listen }

#[derive(Clone, Debug, PartialEq)]
pub struct DispatchSite { pub ty: String, pub mode: DispatchMode, pub kind: SiteKind,
                          pub file: PathBuf, pub line: usize }

#[derive(Clone, Debug, PartialEq)]
pub struct Catalog { pub decls: Vec<EventDecl>, pub sites: Vec<DispatchSite> }

#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// `impl EmitEvent for X { const MODE = DispatchMode::Serial; }` — the catalog surface and
    /// the dispatcher would disagree, silently. The mismatch the compiler CANNOT catch.
    ModeOverrideDisagreesWithTrait { decl: EventDecl },
    /// Two types declare the same `NAME` under different modes.
    NameDeclaredTwiceWithDifferentModes { name: String, a: EventDecl, b: EventDecl },
    /// `.waterfall::<X>()` where `X` declares Emit (a type impl'ing two event traits).
    DispatchModeDiffersFromDeclaration { site: DispatchSite, decl: EventDecl },
    ListenModeDiffersFromDeclaration   { site: DispatchSite, decl: EventDecl },
    /// A dispatch site whose type declares no event trait anywhere in the tree.
    UndeclaredDispatch { site: DispatchSite },
}

/// Parse every `.rs` under `roots` with `syn` and collect declarations and sites.
pub fn scan(roots: &[&Path]) -> Result<Catalog, ScanError>;
/// PURE: the five checks. Empty = the gate passes.
pub fn check(c: &Catalog) -> Vec<Finding>;
/// The committed table: name | mode | type | crate | dispatch sites | listen sites.
pub fn table(c: &Catalog) -> String;
/// §15 item 7's threshold: the gate is worth having past ~30 events.
pub const CATALOG_FLOOR: usize = 30;
```

The tree carries 31 event impls in `plugins/` plus 7 in the kernel, so the catalog is past the floor
and the gate is due now.

### 2.7 Bundle rows (`bundles/bough-tui-app.yml`, appended)

```yaml
- id: tui.preview
  plugin: tui-preview
  config: { height: 14, collapse_rows: 34, min_rows: 8, max_rows: 24, refresh_ms: 250, max_chars: 200000 }
- id: tui.timeline
  plugin: tui-timeline
  config: { height: 12, collapse_rows: 46, min_rows: 6, max_rows: 20, window: 400, limit: 500,
            debounce_ms: 150, time_format: "%H:%M:%S" }
- id: tui.drift
  plugin: tui-drift
  config: { height: 8, collapse_rows: 54, min_rows: 4, max_rows: 14, agents_shown: 8,
            refresh_ms: 1000, bar_cols: 12, arm_ms: 3000 }
```

---

## 3. Design notes that decide the hard parts

### 3.1 What "byte-exact" can honestly mean (V1)

`agent-loop`'s `request::build` sets `system: Assembled::to_text()`, and nothing else, so the pane's
bytes and the loop's system prefix are the same function of the same `Assembled`. The catch is
`as_of`. §5's wake flow appends `wake/start`, then any `mail/delivered`, then `step/start`
**before** step 6 assembles, and the projection's tail band reads every step on the chain — so a
preview taken at the head one instant before a wake is NOT the wake's prefix; it is the wake's
prefix minus that wake's own preface rows.

The pane therefore has two modes and states which it is in, on its header line:

- **`PreviewAt::Seq(s)`** — assembled at a named high-water. For `s` = a past wake's
  `request/header.as_of`, the bytes are byte-identical to what that wake sent. **This is V1's
  assertion**, and it is the strongest available: it proves the pane calls the same
  `ctx.projection` the loop does, with the same request, producing the same bytes.
- **`PreviewAt::Head`** — "if it woke now". The header says `+N preface rows at wake`, computed from
  the pending inbox and `WAKE_PREFACE_KINDS`, so the pane never claims a byte-exactness it does not
  have. The second V1 test pins the delta: what a real next wake adds over a Head preview is
  *exactly* preface rows and nothing else.

Hook **H2** in the merge notes: if `agent-loop` grows a `dry_run_prefix(agent) -> Assembled` on the
`agent_loop` seam (assemble against a simulated preface without appending), `PreviewAt::Head` becomes
byte-exact too and the second test collapses into the first.

### 3.2 Aux-slot pressure and the open/close question

`register_pane` fixes a pane's `SlotSize` at registration; there is no runtime resize, and
re-registering per toggle would push a dead `EffectHandle` onto the fiber's accumulator every time
(`Accumulator::push` only grows) — a slow leak, in the phase whose whole job is proving nothing
leaks. So the three panes do **not** toggle their size. They register once, with
`SlotSize::Responsive { collapse, preferred, min, max }` — the variant `layout` already resolves
against the **available rows** in the Aux band. Chosen breakpoints (§2.7) mean: at 34 rows nothing
digging is laid out, at 40 the preview is, at 46 the timeline joins it, at 54 the drift board does.
The commands FOCUS a pane; `Esc` returns the keyboard to the composer, per the ux1 rule. This is
also what gives the shell-use "resize" bullet real teeth: shrinking the terminal reflows a digging
pane to zero rows and growing it back restores it, with no patch and no restart.

Hook **H1** in the merge notes: one additive `TuiHandle::set_pane_size(&PaneId, SlotSize) -> bool`
(mutate `PaneEntry::info.size` under the existing `RwLock`, then `redraw()`; no effect, no
accumulator growth, ~10 lines). With H1 the panes register at `Cells(1)` — a discoverable collapsed
header — and `/preview` becomes a true toggle.

### 3.3 What the audit script can see, and what it cannot

`bough --check` exits non-zero when an enabled row never activates, which is exactly what disabling
a row with dependents produces. The script therefore does not treat exit 1 as failure; it parses the
report `describe_unresolved` prints (`  <id> (plugin `<p>`) is <FiberState>; unmet: <keys>`) and
requires: every listed row is `Pending`, **none** is `Failed`, none carries an `error:` line, and
the disabled row itself is not listed. Anything else — a panic, a missing report, a `Failed` — is a
FAIL row in the table.

The script cannot see binding or listener counts. That half is asserted **in-process**, by
`crates/bough/tests/audit_leaks.rs`, which for every `bough-base` row: boots, records
`kernel.core().binding_count()` and `listener_count(e)` for every event name in the committed
catalog, disables the row through the launcher's own live-recompose path, re-enables it, and asserts
every count returns to the pre-disable baseline. The script runs that test as its Phase C and folds
the result into the table, so the printed table covers all three claims (settles / nothing FAILED /
nothing leaked) with the strongest available evidence for each.

### 3.4 The two-provider seams on this branch

| Seam | Providers | Suite the audit runs under each |
|---|---|---|
| `ledger` | `ledger-sqlite`, `ledger-memory` | `cargo test -p bough --test ledger_swap`; `cargo test -p bough-plugin-ledger` (the conformance suite runs both) |
| `projection` | `projection-assembler`, `projection-probe` | `--test projection_swap`, `--test projection_tiers` |
| `rollups` | `rollups-summarizer`, `rollups-none` | `--test rollups_swap`, `--test memory_invariants` |
| `llm` | `llm-anthropic`, `llm-replay` | `--test agent_scripted` under each patch (the anthropic arm is `BOUGH_LIVE=1`-gated and SKIPs otherwise, printed as SKIP, never as ok) |
| `agent_loop` | `agent-loop`, `agent-loop-scripted` | `--test loop_swap`, `--test agent_invariants` |
| `tui` | `tui-shell`, `tui-probe` | `--test tui_swap`, `--test tui_boot` |
| `workers` | `worker-spawn`, `worker-fork` | `--test worker_spawn` |
| `actions` | `actions-shim` (this phase's) | `--test crash_reconcile` — **one provider today**, named in `docs/plugin-audit-c.md` as the seam whose second Provider arrives in Phase 6 |

---

## 4. Work packages

Seven packages, disjoint file sets. WP-1..WP-3 program against §2 and never see each other. WP-7
owns every shared file (`Cargo.toml`, `Makefile`, `bundles/bough-tui-app.yml`, `scripts/`,
`docs/`), so no two packages ever edit one file.

### WP-1: `tui-preview` — the byte-exact projection preview

**Files:** `plugins/tui-preview/` (`Cargo.toml`, `src/lib.rs`, `src/snapshot.rs`, `src/pane.rs`,
`src/command.rs`, `src/delta.rs`, `src/error.rs`, `src/invariant.rs`, `tests/snapshot.rs`).

`snapshot()` resolves the agent's trajectory through the ledger's mutable `agents` row (never a
`?? default` trajectory), reads `head_seq` for `PreviewAt::Head`, and calls
`ProjectionHandle::assemble` with `wake: None`, `budget: None` and the `as_of` the mode names — the
same call `agent-loop` makes, with the same defaults, so the bytes are the loop's by construction.
`system_prefix` and `digest` are one line each and exist so nothing re-spells them. `delta.rs` owns
`added_lines`/`only_preface`, which is how the Head mode states its caveat instead of hiding it. The
pane renders from a `Snapshot` it already holds (`Pane::render` is synchronous); refreshes happen in
`handle` on `Tick` and on the debounced `ledger/step` listener.

Unit tests it must ship: `snapshot::tests::{head_uses_the_ledger_head_as_as_of,
seq_mode_uses_the_seq_it_was_given, an_agent_with_no_trajectory_is_refused_not_defaulted,
the_digest_is_sha256_of_the_text}`; `delta::tests::{added_lines_are_the_suffix_the_later_text_gained,
only_preface_accepts_the_three_wake_preface_kinds,
only_preface_rejects_a_tool_result_line, a_shrinking_projection_reports_no_added_lines}`;
`pane::tests::{render_is_a_pure_function_of_the_snapshot, esc_is_handled_and_dismisses,
the_header_names_the_mode_and_the_as_of, a_snapshot_over_max_chars_is_clipped_and_says_so}`;
`invariant::tests::{a_render_whose_digest_mismatches_its_headers_is_reported, a_clean_stream_passes}`;
`tests/snapshot.rs::{two_snapshots_at_one_seq_are_byte_identical,
a_snapshot_at_a_seq_ignores_every_row_above_it}`.

### WP-2: `tui-timeline` — the cross-agent timeline and its filters

**Files:** `plugins/tui-timeline/` (`Cargo.toml`, `src/lib.rs`, `src/filter.rs`, `src/order.rs`,
`src/render.rs`, `src/pane.rs`, `src/command.rs`, `src/error.rs`, `src/invariant.rs`,
`tests/filters.rs`, `tests/purity.rs`).

`order.rs` owns `timeline()`: the total order, the truncation to the newest `limit`, and nothing
else — no clock, no ledger, no I/O, which is what makes "a pure function of the ledger stream" a
property a test can hold. `filter.rs` owns `Filter`, `matches`, `parse_filter`, `render_filter` and
`to_query`; the read is `window` steps per trajectory and every time bound is applied in
`matches`, never pushed into `StepQuery` (D-C4). `pane.rs` owns the filter editor line, the row
focus, the click→`FocusRequest`, and the `Esc` two-step (clear the editor, then dismiss).

Unit tests it must ship: `filter::tests::{an_empty_filter_matches_everything,
the_five_dimensions_are_conjoined, members_within_one_dimension_are_disjoined,
since_is_inclusive_and_until_is_exclusive, a_relative_span_resolves_against_the_now_it_is_given,
an_unknown_word_is_an_error_naming_the_word, since_after_until_is_refused,
render_filter_round_trips_through_parse_filter, to_query_pushes_trajs_kinds_and_class_only}`;
`order::tests::{rows_from_two_trajectories_interleave_by_at,
a_tie_on_at_is_broken_by_traj_then_seq, the_limit_keeps_the_NEWEST_rows_and_returns_them_oldest_first,
timeline_is_a_pure_function_of_its_input_slice, the_same_input_in_a_shuffled_order_yields_the_same_output}`;
`render::tests::{a_line_is_clipped_to_cols_and_never_wraps, hit_of_is_the_step_id}`;
`invariant::tests::{a_rendered_row_that_is_not_in_the_queried_set_is_reported,
an_out_of_order_render_is_reported, a_clean_render_passes}`;
`tests/filters.rs::{agent_and_ref_and_type_and_time_compose,
narrowing_one_dimension_never_widens_the_result}`;
`tests/purity.rs::{the_same_ledger_yields_the_same_timeline_twice}`.

### WP-3: `tui-drift` — the drift dashboard

**Files:** `plugins/tui-drift/` (`Cargo.toml`, `src/lib.rs`, `src/dash.rs`, `src/render.rs`,
`src/pane.rs`, `src/command.rs`, `src/invariant.rs`, `tests/reset_reachable.rs`).

`dash.rs` is pure over `Signals`: `dash_row`, `verdict`, and the arming state machine. `verdict`
never turns `TooFewSamples` into `Steady` (§16). `render.rs` owns `line` and `bar`, both total.
`pane.rs` polls `DriftHandle::signals` per resident on a `refresh_ms` tick — never in `render` —
and returns `PaneOutcome::Command(reset_command(agent))` when the arm fires. The pane writes
nothing itself: the whole reset path is `drift-watch`'s existing `/reset`.

Unit tests it must ship: `dash::tests::{a_flagged_signal_is_not_steady,
too_few_samples_is_its_own_verdict_not_steady, an_inactive_claim_signal_renders_as_unknown,
the_first_r_arms_and_the_second_fires, an_arm_expires_after_arm_ms,
arming_a_different_agent_replaces_the_arm}`; `render::tests::{a_zero_share_bar_and_a_full_bar_both_render,
a_line_is_clipped_to_cols, the_flag_column_names_every_flag}`;
`pane::tests::{r_twice_returns_the_reset_command_for_the_focused_row,
the_reset_command_is_exactly_slash_reset_agent, esc_disarms_before_it_dismisses,
render_is_a_pure_function_of_the_held_signals}`;
`tests/reset_reachable.rs::{the_command_the_pane_returns_is_registered_by_drift_watch}`.

### WP-4: `fault-inject` + `actions-shim` — the two test rows

**Files:** `plugins/fault-inject/` (`Cargo.toml`, `src/lib.rs`, `src/sites.rs`, `src/invariant.rs`,
`tests/sites.rs`); `plugins/actions-shim/` (`Cargo.toml`, `src/lib.rs`, `src/provider.rs`,
`src/invariant.rs`, `tests/provider.rs`).

`fault-inject` registers exactly one site per row, through declared keys only, and counts every hit
in a process-global cell behind `test_lock()` (the `hello::trace` precedent). `FaultSite::Apply`
returns `Err` from `apply`, which is the only way to produce a FAILED fiber on purpose.
`actions-shim` is an ordinary `actions` Provider: intent row → `delay_before_ms` → `gh` with the
idempotency marker embedded in the artifact → `delay_after_ms` → `action/done`, which is precisely
§7's ordering and precisely the two windows V3 kills inside.

Unit tests it must ship: `sites::tests::{after_n_fires_on_the_nth_hit_and_not_before,
times_zero_fires_forever, times_one_fires_once_then_passes,
an_agent_filter_leaves_other_agents_alone, panic_and_error_are_distinct_kinds}`;
`fault-inject/tests/sites.rs::{an_apply_fault_leaves_the_fiber_failed_and_apply_ran_once,
a_projection_section_fault_returns_err_from_the_section_and_not_from_assemble_itself}`;
`provider::tests::{the_marker_is_embedded_in_every_artifact,
the_four_kinds_are_exactly_the_sanctioned_ones, an_unregistered_kind_is_refused,
the_shim_binary_name_comes_from_config_and_is_never_hardcoded}`;
`actions-shim/tests/provider.rs::{one_execute_is_one_gh_invocation,
a_failing_gh_marks_the_row_failed_and_still_writes_action_done}`.

### WP-5: hardening tests (V3, V4)

**Files:** `crates/bough/tests/crash_reconcile.rs`, `crates/bough/tests/failure_injection.rs`,
`crates/bough/tests/spawn_storm.rs`, `crates/bough/tests/audit_leaks.rs`,
`crates/bough/tests/fixtures/crash.patch.yml`, `crates/bough/tests/fixtures/fault.patch.yml`,
`crates/bough/tests/fixtures/gh-shim.sh`.

`crash_reconcile.rs` spawns the real `bough exec` binary with a scripted loop, `actions-shim` and a
recording `gh` shim first on `PATH`, waits (by polling the sqlite ledger, not by sleeping) until an
`action/intent` row exists, sends `SIGKILL`, then restarts the same `$BOUGH_HOME` and asserts, in
order: the orphaned wake now carries `wake/end { reason: "interrupted" }`; every step appended
before the kill is still there and only the in-flight `thought/text` is missing; every unanswered
`tool/call` has a `tool/result { outcome: "unknown" }`; `ActionsHandle::pending()` lists the intent;
and the `gh` shim's call log has **exactly one** line for that idem key — across the crash and the
restart. Two variants: killed before the outward call and killed after it (the second is the one
that would re-execute if reconciliation guessed).

`failure_injection.rs` covers the four V4 bullets: a `ProjectionSection` fault on wake 1 ends that
wake with `wake/end { reason: "error" }` while wake 2 completes; an `Apply` fault applied by a LIVE
patch reload leaves that row `Failed`, broadcasts `kernel/rows-unresolved` once, leaves every other
row `Active`, and `fault_inject::applies()` stays at its pre-fault value forever (not retried); a
`Panic` listener is contained (`kernel/listener-failed`) and the dispatch continues; and
`llm-replay` with `strict: true` and no matching round delivers `Chunk::Failed` as a **terminal
chunk**, which the loop records as `wake/end { reason: "error" }` and never as a thrown error.

`spawn_storm.rs`: a scripted wake requesting 50 workers gets exactly `per_wake_spawn_cap` starts and
46 `WorkerError::BoundsExceeded { bound: "per_wake_spawn_cap" }`; `WorkersHandle::in_flight()` never
exceeds `max_in_flight` under a concurrent storm from three agents; a depth-4 spawn is refused with
`bound: "max_depth"`; and every refusal reaches the model as a `tool/result` failure rather than a
silent no-op.

`audit_leaks.rs`: §3.3's baseline/disable/re-enable/compare loop, one case per `bough-base` row,
plus the three new pane rows.

Unit tests it must ship: the four files above ARE the tests; each names its cases as above. No
production code is added by this package.

### WP-6: `xtask events` — the event catalog gate (§15 item 7)

**Files:** `crates/xtask/` (`Cargo.toml`, `src/lib.rs`, `src/scan.rs`, `src/check.rs`,
`src/table.rs`, `src/main.rs`, `tests/planted.rs`, `tests/catalog.rs`,
`tests/fixtures/planted/{mode_override.rs, duplicate_name.rs, wrong_dispatch.rs, clean.rs}`).

`scan.rs` parses every `.rs` under `crates/` and `plugins/` with `syn` and visits `ItemImpl` whose
trait path ends in one of the four event traits (reading `const NAME` and any `const MODE`) plus
every method call named `emit`/`parallel`/`serial`/`waterfall`/`on`/`on_with` carrying a turbofish.
`check.rs` is pure over a `Catalog` and produces the five `Finding`s. `table.rs` writes
`docs/event-catalog.md`. `main.rs` is `cargo xtask events [--check] [--write <path>]`.

Unit tests it must ship: `scan::tests::{an_impl_of_each_trait_is_found_with_its_mode,
a_const_mode_override_is_recorded_separately_from_the_trait,
a_turbofish_dispatch_site_records_its_mode, a_listener_registration_records_a_listen_site,
a_file_that_does_not_parse_is_an_error_naming_the_file}`;
`check::tests::{a_clean_catalog_has_no_findings,
a_mode_override_that_disagrees_with_its_trait_is_reported,
one_name_under_two_modes_is_reported, a_dispatch_site_whose_type_declares_another_mode_is_reported,
an_undeclared_dispatch_is_reported}`; `table::tests::{every_declared_event_gets_a_row,
the_table_is_sorted_by_name_and_is_stable}`;
`tests/planted.rs::{the_planted_mode_override_fails_the_gate,
the_planted_duplicate_name_fails_the_gate, the_planted_wrong_dispatch_fails_the_gate,
the_clean_fixture_passes}`;
`tests/catalog.rs::{this_tree_has_no_event_findings,
the_catalog_is_past_the_thirty_event_floor,
the_committed_catalog_matches_the_tree}`.

### WP-7: integration — rows, the audit script, the shell-use suite, the swap, the docs

**Files:** `Cargo.toml` (one `members` line), `.cargo/config.toml` (new: the `xtask` alias),
`Makefile` (`audit-plugins`, `events`), `bundles/bough-tui-app.yml` (three rows),
`scripts/audit-plugins.sh` (rewritten), `scripts/tui/33-preview.sh`, `scripts/tui/34-timeline.sh`,
`scripts/tui/35-drift.sh`, `scripts/tui/36-swap-digging.sh`,
`crates/bough/tests/preview_bytes.rs`, `docs/plugin-audit-c.md`, `docs/event-catalog.md`,
`docs/track-c-merge-notes.md`, `BUILD.md` (one row).

`scripts/audit-plugins.sh` gains four phases and a table: **A** every shipped profile composes and
boots (today's script, kept); **B** every `bough-base` row disabled one at a time, classified by
§3.3's rule; **C** `cargo test -p bough --test audit_leaks`; **D** every two-provider seam of §3.4,
booted under each provider with that seam's suite. It prints `row | disabled | dependents pending |
failed | leaked | verdict`, supports `--json` and `--bundle <name>`, exits non-zero on any FAIL, and
its committed run is `docs/plugin-audit-c.md`.

`crates/bough/tests/preview_bytes.rs` is V1: boot the tree with `agent-loop-scripted`, `llm-replay`
and `tui-probe`, run one wake, read the sent request from
`bough_plugin_agent_loop::invariant::seen()` and the wake's `request/header`, take a
`PreviewAt::Seq(header.as_of)` snapshot, and `assert_eq!` the two strings byte for byte.

The shell-use scripts follow `scripts/tui/lib.sh` conventions (`t`, `tui_open`, `see`, `wheel`,
`write_patch`, `t_size`, `row_with`), each bullet named, each name cited by §5.

The bullets the scripts actually carry (MERGE: the names below are the SCRIPTS' own, re-pointed
from the plan's drafted spellings — `docs/track-c-merge-notes.md` asked for exactly that, and
`crates/bough/tests/docs.rs::every_shell_bullet_the_phase_c_map_names_exists` is now the gate that
keeps the map and the scripts in step):

`scripts/tui/33-preview.sh` → `the_command_opens_the_preview_pane`,
`the_header_names_the_agent_and_the_high_water`, `t_moves_between_head_and_anchored`,
`esc_gives_the_keyboard_back_to_the_composer`;
`34-timeline.sh` → `the_command_opens_the_timeline_pane`,
`a_filter_typed_in_the_pane_narrows_the_rows`,
`an_unknown_filter_word_is_refused_with_the_usage`,
`esc_clears_the_editor_and_then_gives_the_keyboard_back`,
`clicking_a_row_focuses_that_agent_and_step`, `the_pane_collapses_below_its_breakpoint`;
`35-drift.sh` → `the_command_opens_the_drift_board`, `the_header_counts_every_agent`,
`the_first_r_arms_the_reset_on_a_row_with_a_verdict`, `the_second_r_dispatches_the_reset_command`,
`esc_disarms_without_resetting`;
`36-swap-digging.sh` → `all_three_digging_panes_are_on_screen_before_the_patch`,
`disabling_the_preview_row_removes_it_and_the_layout_reflows`,
`disabling_the_timeline_row_removes_it_and_the_layout_reflows`,
`disabling_the_drift_row_removes_it_and_the_layout_reflows`,
`re_enabling_all_three_restores_them_without_a_restart`.

STILL NOT SHIPPED, and named so nobody has to rediscover it: `the_pane_scrolls_with_the_wheel` and
`at_34_rows_the_preview_costs_nothing_and_the_layout_reflows` (the preview mounts at
`collapse_rows: 24`, so 34 rows is ABOVE its breakpoint — `34-timeline.sh` carries the breakpoint
bullet instead, at 20 rows), and `two_filters_compose` at the screen level (composition is proven
offline by `plugins/tui-timeline/tests/filters.rs::agent_and_ref_and_type_and_time_compose`).

---

## 5. Verification map

| Bullet | Proven by |
|---|---|
| **V1** the preview shows byte-exact wake context: the bytes it renders equal the request the loop sends, both captured | `crates/bough/tests/preview_bytes.rs::the_preview_bytes_equal_the_system_prefix_the_loop_sent` (primary: `PreviewAt::Seq(header.as_of)` vs `agent_loop::invariant::seen().last().request.system`, byte for byte) and `::the_preview_digest_equals_the_request_headers_projection_digest`; the "if it woke now" half by `::a_head_preview_and_the_next_wake_differ_only_by_that_wakes_preface_rows` over `tui_preview::only_preface`; supporting purity by `plugins/tui-preview/tests/snapshot.rs::two_snapshots_at_one_seq_are_byte_identical`; on screen by `scripts/tui/33-preview.sh::{the_command_opens_the_preview_pane, the_header_names_the_agent_and_the_high_water, t_moves_between_head_and_anchored, esc_gives_the_keyboard_back_to_the_composer}` |
| **V2** timeline filters compose (agent ∧ ref ∧ type ∧ time) and the timeline is a pure function of the ledger stream; shell-use drives open/filter/click/Esc/resize | `plugins/tui-timeline/tests/filters.rs::{agent_and_ref_and_type_and_time_compose, narrowing_one_dimension_never_widens_the_result}`; `order::tests::{timeline_is_a_pure_function_of_its_input_slice, the_same_input_in_a_shuffled_order_yields_the_same_output, rows_from_two_trajectories_interleave_by_at, a_tie_on_at_is_broken_by_traj_then_seq}`; `plugins/tui-timeline/tests/purity.rs::the_same_ledger_yields_the_same_timeline_twice`; `scripts/tui/34-timeline.sh::{the_command_opens_the_timeline_pane, a_filter_typed_in_the_pane_narrows_the_rows, an_unknown_filter_word_is_refused_with_the_usage, clicking_a_row_focuses_that_agent_and_step, esc_clears_the_editor_and_then_gives_the_keyboard_back, the_pane_collapses_below_its_breakpoint}` (the SCRIPTS' names; `two_filters_compose` has no screen bullet and is proven offline by `filters.rs`) |
| **V3** kill -9 during a wake loses nothing but the in-flight thought AND replays no outward action | `crates/bough/tests/crash_reconcile.rs::{a_killed_wake_reopens_closed_as_interrupted, only_the_in_flight_thought_is_missing_after_the_restart, every_unanswered_tool_call_gets_an_unknown_result, the_pending_intent_is_listed_after_the_restart_and_never_re_executed, a_kill_after_the_outward_call_still_yields_exactly_one_gh_invocation}`; supported by `plugins/actions-shim/tests/provider.rs::one_execute_is_one_gh_invocation` and `actions-shim`'s invariant |
| **V4** a plugin fiber failing mid-wake ends that wake with reason error and the loop continues; a FAILED row is reported and not retried; a fan-out storm is held by the spawn bounds; an llm failure arrives as a terminal chunk | `crates/bough/tests/failure_injection.rs::{a_section_fault_ends_that_wake_with_reason_error, the_next_wake_after_a_faulted_one_completes, a_failed_row_is_reported_once_and_apply_is_never_called_again, a_failed_row_leaves_every_other_row_active, a_panicking_listener_is_contained_and_the_dispatch_continues, an_unmatched_replay_arrives_as_a_terminal_failed_chunk}`; `crates/bough/tests/spawn_storm.rs::{fifty_spawns_in_one_wake_stop_at_the_per_wake_cap, in_flight_never_exceeds_max_in_flight_under_a_three_agent_storm, a_depth_four_spawn_is_refused, every_refusal_reaches_the_model_as_a_tool_result_failure}` |
| **V5** `scripts/audit-plugins.sh` runs against this branch's `bough-base`: every row disabled one at a time settles, and every two-provider seam runs its suite under each provider; the table is committed with zero exceptions beyond §0.1 (or each exception named) | `make audit-plugins` exits 0; its committed run is `docs/plugin-audit-c.md` (phases A–D, one line per row, one line per seam×provider, every SKIP named with its reason); the leak column is `crates/bough/tests/audit_leaks.rs::{disabling_a_row_and_re_enabling_it_returns_every_binding_count_to_baseline, …_every_listener_count_to_baseline}`; the script's own classification rule is unit-tested by `scripts/audit-plugins.sh --self-test` against recorded `--check` reports in `scripts/fixtures/check-reports/` |
| **V6** the drift dashboard renders drift-watch's per-agent signals and the reset command is reachable from it | `scripts/tui/35-drift.sh::{the_command_opens_the_drift_board, the_header_counts_every_agent, the_first_r_arms_the_reset_on_a_row_with_a_verdict, the_second_r_dispatches_the_reset_command, esc_disarms_without_resetting}`; `plugins/tui-drift/tests/reset_reachable.rs::the_command_the_pane_returns_is_registered_by_drift_watch`; `pane::tests::{r_twice_returns_the_reset_command_for_the_focused_row, the_reset_command_is_exactly_slash_reset_agent}` |
| **V7** the event catalog test lists every event with its dispatch mode and fails on a planted mismatch | `crates/xtask/tests/catalog.rs::{this_tree_has_no_event_findings, the_catalog_is_past_the_thirty_event_floor, the_committed_catalog_matches_the_tree}`; `crates/xtask/tests/planted.rs::{the_planted_mode_override_fails_the_gate, the_planted_duplicate_name_fails_the_gate, the_planted_wrong_dispatch_fails_the_gate, the_clean_fixture_passes}`; the catalog itself is `docs/event-catalog.md`, regenerated by `make events` |
| **SWAP** each of the three new pane rows disabled by patch while the TUI runs disappears and the layout reflows; re-enabling restores it; the audit script exercises every `bough-base` row | `scripts/tui/36-swap-digging.sh::{all_three_digging_panes_are_on_screen_before_the_patch, disabling_the_preview_row_removes_it_and_the_layout_reflows, disabling_the_timeline_row_removes_it_and_the_layout_reflows, disabling_the_drift_row_removes_it_and_the_layout_reflows, re_enabling_all_three_restores_them_without_a_restart}`; the audit half by `make audit-plugins` phase B, one table row per `bough-base` row, plus `crates/bough/tests/audit_leaks.rs` for the leak column |

Every name above is a test that must exist and run green before this phase is DONE. A bullet with no
green named test is reported as NOT verified, never as done.

---

## 6. Decisions taken where REQUIREMENTS is silent

- **D-C1 — "byte-exact" is `PreviewAt::Seq`, and `PreviewAt::Head` states its delta.** §11 asks for
  "exactly what the agent would see if it woke now, byte-exact". A wake appends three kinds of row
  before it assembles, so the two cannot both be true at once with today's seam. The pane offers both
  modes, V1 asserts byte-exactness where it is real, and the Head mode's header prints the preface
  delta rather than claiming an exactness it does not have (§16). Hook H2 removes the split.
- **D-C2 — the digging panes are `Responsive`-sized and never resize themselves.** Re-registration
  per toggle would grow the fiber's effect accumulator without bound; in the phase that proves
  nothing leaks, that is not an acceptable mechanism. Breakpoints decide which digging panes a given
  terminal can afford. Hook H1 is the additive fix.
- **D-C3 — the three panes register no `ctx` key and own no write path.** Each is a Consumer. The
  drift board's reset goes out as `PaneOutcome::Command("/reset <agent>")` through the existing
  `commands` seam, so §8's one-command reset stays one implementation.
- **D-C4 — the timeline's time window is applied in the pure filter, not pushed into `StepQuery`.**
  `StepQuery` has seq bounds and no time bounds; inventing one would be a ledger change on a
  parallel track. The read is bounded by `window` steps per trajectory and the time filter runs over
  what came back — which is also what keeps `timeline()` a pure function of a slice.
- **D-C5 — the reset is two-step.** One keystroke rebuilding an agent's digest and about-line is not
  a surface a daily driver should have. `r` arms with a visible notice; a second `r` within `arm_ms`
  fires; `Esc` disarms.
- **D-C6 — the audit's leak column is asserted in-process, not by the shell.** The launcher prints no
  binding or listener counts, and adding a flag to it would be an edit to `crates/bough/src`. The
  script runs `audit_leaks.rs` and reports its verdict; the Rust test is the stronger statement
  anyway (it compares counts across a live disable/re-enable, not across two processes).
- **D-C7 — the event catalog gate is a source scan with `syn`, in an `xtask`.** The four event traits
  already make the mode compile-checked, so the residual risks are a `const MODE` override that
  disagrees with its trait, one `NAME` declared twice under two modes, and a type impl'ing two event
  traits dispatched under the wrong one. All three are lexical, none is catchable by the type system,
  and a regex scanner would miss exactly the cases that matter. `syn` is a build-tool-only dependency
  of a non-shipping crate.
- **D-C8 — `fault-inject` and `actions-shim` are catalog-only rows.** They follow the
  `ledger-memory` / `agent-loop-scripted` precedent: in the binary, in no bundle, mounted by a test's
  own patch, invisible to `--dump-config` on every shipped profile.
- **D-C9 — the `llm` seam's live arm SKIPs rather than passes.** The audit's `llm-anthropic` arm needs
  a key; without `BOUGH_LIVE=1` it prints one SKIP line per bullet with its reason (the `skip_all`
  precedent), never an `ok`.
- **D-C10 — `/driftboard`, not `/drift`.** `drift-watch` already owns `/drift`; the pane takes a name
  of its own rather than shadowing a registered command.

---

## 7. Merge hooks (also written to `docs/track-c-merge-notes.md`)

- **H1 — `TuiHandle::set_pane_size(&PaneId, SlotSize) -> bool`** (`plugins/tui-shell/src/lib.rs`).
  Mutates `PaneEntry::info.size` under the existing `RwLock` and calls `redraw()`. No effect, no
  accumulator growth. Turns `/preview`, `/timeline`, `/driftboard` into true open/close toggles and
  lets the three panes default to a one-row collapsed header. ~10 lines.
- **H2 — a dry-run prefix on the `agent_loop` seam**: `dry_run_prefix(&AgentName) -> Assembled`,
  assembling against the preface rows the next wake WOULD append without appending them. Makes
  `PreviewAt::Head` byte-exact and collapses V1's second test into its first.
- **H3 — `Kernel::event_catalog() -> Vec<(&'static str, DispatchMode)>`**, populated at listener
  registration. Would turn WP-6's source scan into a runtime assertion and let the gate also catch an
  event that is declared but never dispatched in a given composition. Not required for V7.
- **H4 — a `--check --tolerate-pending` (or a `--dump-state` after quiesce) on the launcher.** Would
  let the audit script read structured row states instead of parsing `describe_unresolved`'s prose.
  The parse is pinned by `scripts/audit-plugins.sh --self-test` against recorded reports, so the
  script fails loudly if that prose ever changes.
