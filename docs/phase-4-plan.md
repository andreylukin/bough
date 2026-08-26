# Phase 4 — memory: design and work breakdown

REQUIREMENTS §8 (the three governance rows), §3 (sealed rollups, pins, digests, seal-once), §5
(tiers in the projection and the degradation ladder), §15 item 5 (the kernel decision), §17 Phase 4.

Phase 3 shipped the interface. It also shipped a hole the cutover note names: from Phase 3 the
harness runs with **no memory tiers of its own**, softened only by `old-feed-adapter` sealing
jungler's `nodes.summary` / `lane_story` rows as interim tier-1 blocks. Phase 4 closes the hole:
a real summarizer that seals tiers from the agent's own trajectory, a reconsolidation pass that
adds without editing, and a drift watch with a reset that rebuilds identity from raw evidence and
never touches a sealed row.

What this phase is NOT is a new mechanism in the projection. Phase 1 already built the tiers band,
the `notable_refs` filter, and the degradation ladder that drops fine tiers first; it built them
against zero rollups because none existed. Phase 4's projection work is therefore two things: the
goldens that finally run over REAL sealed tiers, and one honest addition — the projector honouring
the expiry marker §8 demands.

The standing decisions of Phases 2 and 3 carry unchanged (`docs/phase-2-plan.md` §7,
`docs/phase-3-plan.md` §6). Three of them shape this phase directly:

- P1-D13: an empty `notable_refs` means "notable to everyone". The summarizer writes a real set
  whenever the covered steps carry refs, and empty only when they carry none.
- P1-D21: the degradation ladder is TOTAL — rung 3 exists so a draft always fits. Real tiers make
  rungs 1 and 3 reachable for the first time.
- P3's open item: the old-feed bridge borrows a foreign row id into the ledger's seq namespace
  (`from_seq = to_seq = Seq(row.id)`). Phase 4 must coexist with those blocks without letting them
  poison seal-once arithmetic. See P4-D13.

---

## 1. Crates

Four new plugin crates, each `bough-plugin-<name>` under `plugins/` (AGENTS.md layout), plus
named edits to three existing files.

| package | path | role | provides | injects | row |
|---|---|---|---|---|---|
| `bough-plugin-rollups` | `plugins/rollups` | **Service Definition** for `ctx.rollups`: the seam trait, the block vocabulary, the pure windowing/planning/expiry algorithms, the provider conformance suite, and the seal-once + tiers-are-an-index invariants. No `Plugin` impl, no row. | — | — | none |
| `bough-plugin-rollups-summarizer` | `plugins/rollups-summarizer` | **Provider**: recap-style map/reduce through `ctx.llm`, sealed blocks stamped with `prompt_ver` and `sealed_at`, the tier tree, supersession, digest rebuild, `/seal`. | `rollups` | required `ledger`, `llm`, `agents`; optional `commands` | `rollups` in `bough-base` |
| `bough-plugin-rollups-none` | `plugins/rollups-none` | **Provider (stub)**: satisfies the key and seals nothing. The SWAP subject. In the catalog, in NO bundle — the `ledger-memory` / `projection-probe` / `tui-probe` precedent. | `rollups` | required `ledger` | none (swap patch only) |
| `bough-plugin-reconsolidation` | `plugins/reconsolidation` | **Definition + Provider** of `ctx.reconsolidation`: batched distillation, contradiction detection, stale-evidence expiry, `/reconsolidate`. | `reconsolidation` | required `ledger`, `llm`, `agents`, `rollups`; optional `commands` | `reconsolidation` in `bough-base` |
| `bough-plugin-drift-watch` | `plugins/drift-watch` | **Definition + Provider** of `ctx.drift`: per-agent stability signals from the ledger, `/drift`, `/reset <agent>`, `/supersede <rollup> <reason>`. | `drift` | required `ledger`, `agents`, `rollups`; optional `commands` | `drift.watch` in `bough-base` |

`reconsolidation` and `drift-watch` each own live state (a pass registry, the signal window cache)
and one conceivable provider apiece, so each is ONE crate that provides its own key — the
`commands` / `llm` precedent, and §0.2's "don't split preemptively". `rollups` splits three ways
because the phase's swap gate *mandates* a second provider selectable by patch with no compile
(P4-D1).

**Edits to existing files** (each owned by exactly one work package, listed there):

- `bundles/bough-base.yml` — three rows appended (§8: "these three rows are in `bough-base`").
- `Cargo.toml` — four `workspace.dependencies` path entries.
- `crates/bough/Cargo.toml` — four link-only dependencies, so the `inventory` registrations land.
- `plugins/llm-replay/src/transcript.rs` — one additive `RecordedChunk::Usage` variant (P4-D10).
- `plugins/projection-assembler/src/{lib,bands,degrade}.rs` + a new `expiry.rs` — the projector
  honouring the expiry marker (§8) and the tier goldens.
- `scripts/tui/`, `Makefile`, `BUILD.md`.

Nothing in Phase 4 touches `plugins/ledger`, `plugins/projection` (the Definition),
`crates/bough-kernel`, or any Phase-2 crate. The ledger vocabulary already carries `RollupKind`,
`NewRollup`, `Rollup`, `RollupQuery`, `seal_rollup`, `supersede_rollup`, `row_hashes` and the
`rollup/sealed` step type; Phase 1 built them for exactly this phase, and Phase 4 spends them.

---

## 2. Public API

### 2.1 The rollups seam — `plugins/rollups/src/…`

```rust
// lib.rs
pub struct Rollups;
impl ServiceKey for Rollups {
    type Value = RollupsHandle;
    const NAME: &'static str = "rollups";
}

/// The concrete handle newtype the key's value is (Decision D5, the `LedgerHandle` precedent).
#[derive(Clone)]
pub struct RollupsHandle(pub Arc<dyn Summarizer>);

/// What a rollups provider does. Every method is idempotent under a repeated call with the same
/// request: `seal` re-run over an unchanged ledger seals nothing and reports `Stop::NothingToDo`.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync + 'static {
    /// Catalog name of the plugin behind this binding; the swap test reads it.
    fn provider(&self) -> &'static str;
    /// The `prompt_ver` this provider stamps on what it seals. `""` iff it seals nothing.
    fn prompt_ver(&self) -> &str;

    /// PURE with respect to the world: reads the ledger, calls no model, writes nothing.
    /// What a `seal` would do, and why each skipped range was skipped.
    async fn plan(&self, req: &SealRequest) -> Result<SealPlan, RollupsError>;

    /// Execute the plan: map over episode windows, reduce to themes, seal each block, append
    /// one `rollup/request` per model call and one `rollup/sealed` per block.
    async fn seal(&self, req: &SealRequest) -> Result<SealReport, RollupsError>;

    /// The relief valve (§3, §8): mint generation n+1 over the SAME range, set `superseded_by`
    /// on generation n, append the expiry note. Refused when the block is already superseded.
    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError>;

    /// Rebuild an agent's standing digest FROM RAW EVIDENCE. Sealed tiers are read, never
    /// re-summarized and never re-sealed (§8). Supersedes the previous digest and repoints
    /// `agents.digest_rollup`.
    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError>;
}
```

Requests and reports (`request.rs`):

```rust
bough_util::brand_id!(
    /// One governance pass. Also the synthetic wake id every step of the pass carries (P4-D2).
    pub struct PassId;
);

/// Who a pass is attributed to. Phase 4 always writes `System`; Phase 5's leader writes
/// `Agent(leader)` with no shape change (§8: "leader-attributed once the leader exists").
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase", tag = "by")]
pub enum Attribution {
    Andrey,
    Agent { name: AgentName },
    System,
}

#[derive(Clone, Debug)]
pub struct SealRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    /// Injected, never read from a clock inside the provider (AGENTS.md).
    pub at: DateTime<Utc>,
    /// Seal nothing above this seq. `None` ⇒ `head - seal_lag_steps` (P4-D11).
    pub upto: Option<Seq>,
    /// Cap on model calls for this pass. `None` ⇒ the row's `max_calls_per_pass`.
    pub max_calls: Option<usize>,
    pub attribution: Attribution,
}

/// What a pass WOULD do. Deterministic and total: every candidate range is either in `blocks`
/// or in `skipped` with a reason.
#[derive(Clone, Debug, PartialEq)]
pub struct SealPlan {
    pub traj: TrajId,
    pub head: Seq,
    pub upto: Seq,
    pub blocks: Vec<PlannedBlock>,
    pub skipped: Vec<Skip>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlannedBlock {
    /// The id this block WILL carry. Deterministic; the seal-once guard is its existence (P4-D4).
    pub id: RollupId,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub inputs: Inputs,
    /// The episode windows this block reduces. One window at tier 1, `fanout` children above.
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inputs {
    /// Tier 1: the raw steps beneath.
    Raw(Vec<StepId>),
    /// Tier k>1: the tier k-1 blocks beneath.
    Blocks(Vec<RollupId>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skip {
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub why: SkipReason,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkipReason {
    /// A `tier:`-namespace block already covers this range at this tier. THE seal-once refusal.
    AlreadySealed,
    /// Inside the `seal_lag_steps` window below the head; the verbatim tail still shows it.
    TooCloseToHead,
    /// Fewer than `min_window_steps` steps; a window this thin is not worth a model call.
    TooShort,
    /// Fewer than `fanout` children exist at the tier below.
    NotEnoughChildren,
    /// The pass hit `max_calls`.
    CallBudget,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SealReport {
    pub pass: PassId,
    pub planned: usize,
    pub sealed: Vec<RollupId>,
    pub skipped: Vec<Skip>,
    pub calls: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub stop: Stop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop { Complete, CallBudget, NothingToDo }

#[derive(Clone, Debug)]
pub struct SupersedeRequest {
    pub block: RollupId,
    pub reason: String,
    pub at: DateTime<Utc>,
    pub attribution: Attribution,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SupersedeReport {
    pub old: RollupId,
    /// Generation n+1 over the same `(traj, tier, from_seq, to_seq)`.
    pub new: RollupId,
    /// The appended `memory/expired` marker naming `old`.
    pub note: StepId,
}

#[derive(Clone, Debug)]
pub struct DigestRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    pub at: DateTime<Utc>,
    pub attribution: Attribution,
    /// `true` ⇒ ignore the existing digest entirely and read raw evidence only. `/reset` sets it.
    pub from_raw: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigestReport {
    pub digest: RollupId,
    pub replaced: Option<RollupId>,
    /// Sealed tier rows READ while building it. Named so a test can assert none were written.
    pub tiers_read: usize,
    pub calls: usize,
}
```

Episode windows (`window.rs`) — PURE, `now` never read:

```rust
/// Why a window ended. `Head` is the last, still-open window and is never sealed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Cut { Gap, MaxSteps, Head }

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Window {
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub from_at: DateTime<Utc>,
    pub to_at: DateTime<Utc>,
    pub steps: Vec<StepId>,
    pub cut: Cut,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowCfg {
    /// A gap this long or longer between consecutive steps ENDS the window (§8: "episode windows
    /// cut at time gaps").
    pub gap: Duration,
    pub max_steps: usize,
    pub min_steps: usize,
}

/// Cut a step run into episode windows. Total, order-preserving, and a pure function of the
/// steps' `at` and `seq` alone.
pub fn windows(steps: &[Step], cfg: &WindowCfg) -> Vec<Window>;
```

Tier planning (`plan.rs`) — PURE:

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierCfg {
    /// §3: fanout ~10. Tier k+1 reduces exactly `fanout` tier-k blocks.
    pub fanout: usize,
    /// The highest tier this deployment builds.
    pub max_tier: u8,
    /// Never seal within this many steps of the head (P4-D11).
    pub lag: usize,
}

/// The deterministic id of a tier block. EXCLUDES `prompt_ver` on purpose (P4-D4): a prompt bump
/// must not re-open a sealed range. `gen` 0 is the original; n>0 is the nth supersession.
pub fn tier_id(traj: &TrajId, tier: u8, from: Seq, to: Seq, gen: u32) -> RollupId;

/// `true` iff `id` is in this crate's namespace. Bridge blocks (`old-feed:…`) are not, and are
/// therefore invisible to the overlap check (P4-D13).
pub fn is_ours(id: &RollupId) -> bool;

/// §3: "tier k covers ~10^k steps". Exact statement of the arithmetic, so the property is a
/// unit test rather than a comment.
pub fn coverage(tier: u8, cfg: &TierCfg) -> usize;   // == cfg.max_window_steps * fanout^(tier-1)

/// The whole plan, from the ledger's own rows. `existing` is every rollup on the trajectory,
/// superseded ones INCLUDED — a superseded range is still sealed and is never re-planned.
pub fn plan(
    existing: &[Rollup],
    windows: &[Window],
    head: Seq,
    upto: Seq,
    traj: &TrajId,
    cfg: &TierCfg,
    wcfg: &WindowCfg,
) -> SealPlan;
```

Block bodies (`block.rs`) — the JSON a `rollups` row's `body` column carries. The assembler's
`rollup_text` already reads an object's `text` field, so `text` is the rendered surface and
everything else is structure the index needs:

```rust
/// The body of a `tier` rollup.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TierBlock {
    /// The recap prose. What the projection renders.
    pub text: String,
    pub themes: Vec<Theme>,
    /// Refs INTO THE LAYER BENEATH (§3: "every block carries refs into the raw beneath it").
    pub beneath: Beneath,
    /// A bounded set of RAW step ids the block's claims rest on, drawn from the layer beneath,
    /// so a projected coarse block resolves to raw in one hop (P4-D5).
    pub evidence: Vec<StepId>,
    pub windows: Vec<WindowRef>,
    pub tier: u8,
    pub prompt_ver: String,
}

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "layer", rename_all = "lowercase")]
pub enum Beneath {
    Raw { steps: Vec<StepId> },
    Blocks { rollups: Vec<RollupId> },
}

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Theme {
    pub title: String,
    pub text: String,
    pub refs: Vec<Ref>,
    pub evidence: Vec<StepId>,
}

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WindowRef { pub from_seq: Seq, pub to_seq: Seq, pub cut: Cut }

/// The body of a `digest` rollup this crate seals.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DigestBlock {
    pub text: String,
    pub standing: Vec<Standing>,
    pub evidence: Vec<StepId>,
    pub from_blocks: Vec<RollupId>,
    pub replaces: Option<RollupId>,
    pub prompt_ver: String,
}

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Standing { pub text: String, pub evidence: Vec<StepId> }

/// Every ref a block names, as the index check reads them. Total over both `Beneath` shapes.
pub fn refs_of(block: &TierBlock) -> (Vec<StepId>, Vec<RollupId>);

/// The `notable_refs` column for a block: the domain refs of the covered steps, most frequent
/// first, capped at `max`. EMPTY when the covered steps carry none (P1-D13).
pub fn notable_refs(steps: &[Step], max: usize) -> BTreeSet<Ref>;
```

Expiry (`expiry.rs`) — the vocabulary of §8's "APPENDED marker the projector honors". The step
type is owned by `reconsolidation` (2.4); the SET is computed here so the projector and the
governance rows read one implementation:

```rust
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Expired {
    pub steps: BTreeSet<StepId>,
    pub rollups: BTreeSet<RollupId>,
}

/// PURE: fold `memory/expired` steps into the set. A marker naming a target that is not a step
/// or rollup ref is ignored, never an error — a marker is data.
pub fn parse(markers: &[Step]) -> Expired;

/// The step kinds an expiry pass may EVER name. Pins and claims are absent by construction
/// (§3, V7): a pin's only relief valve is supersession.
pub const NEVER_EXPIRABLE: &[&str] =
    &["pin/set", "pin/retire", "claim/proposed", "claim/accepted", "claim/rejected"];
```

Errors (`error.rs`):

```rust
#[derive(Debug, thiserror::Error)]
pub enum RollupsError {
    #[error("tier {tier} range {from}..{to} of `{traj}` is already sealed as `{existing}`")]
    AlreadySealed { traj: TrajId, tier: u8, from: Seq, to: Seq, existing: RollupId },
    #[error("rollup `{0}` is not in the ledger")]
    NotFound(RollupId),
    #[error("rollup `{0}` is already superseded by `{1}`")]
    AlreadySuperseded(RollupId, RollupId),
    #[error("`{0}` is not a block this provider sealed; supersession is namespaced")]
    NotOurs(RollupId),
    #[error("the model returned no usable block: {0}")]
    BadBlock(String),
    #[error("the model call failed: {0}")]
    Model(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}
```

Conformance (`conformance.rs`) — the suite every provider must pass, the `plugins/ledger`
precedent. Both `rollups-summarizer` and `rollups-none` run it from their own test targets, with
the stub's expectations parameterised by `seals: bool`:

```rust
pub struct Conformance { pub seals: bool }
impl Conformance {
    pub async fn run(&self, handle: &RollupsHandle, ledger: &LedgerHandle) -> Result<(), String>;
}
```

Invariants (`invariant.rs`) — returned by both providers' `Plugin::invariants()`:

1. **`seal_once`** — over the observed `ledger/step` stream filtered to `rollup/sealed`: no two
   observations name the same `(traj, tier, from_seq, to_seq, gen)`, and no observation names a
   `(traj, tier, from_seq, to_seq)` whose generation is not exactly one above the highest already
   seen for it. This is the event-stream half V1 asks for; the ledger's own `seal_once` (a
   `superseded_by` transition happens at most once) is the row half and already exists.
2. **`tiers_are_an_index`** — for every `rollup/sealed` observed, every id in the block's
   `beneath` and `evidence` resolves to a row that exists in the store at quiesce.

Cadence `OnQuiesce` for both (P1-D14; the kernel dispatches no other).

### 2.2 The summarizer Provider — `plugins/rollups-summarizer/src/…`

```rust
pub const PLUGIN_NAME: &str = "rollups-summarizer";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SummarizerConfig {
    /// Stamped on every block. Bumping it does NOT re-open a sealed range (P4-D4).
    pub prompt_ver: String,
    /// The episode cut (§8). Minutes, not a Duration: a bundle patch is YAML.
    pub gap_minutes: u64,
    pub max_window_steps: usize,
    pub min_window_steps: usize,
    /// §3: ~10.
    pub fanout: usize,
    pub max_tier: u8,
    /// P4-D11: never seal within this many steps of the head.
    pub seal_lag_steps: usize,
    /// A pass makes at most this many model calls (P4-D16).
    pub max_calls_per_pass: usize,
    pub max_notable_refs: usize,
    pub max_evidence_refs: usize,
    pub max_block_chars: usize,
    pub map_max_tokens: i64,
    pub reduce_max_tokens: i64,
}
```

`validate()` (pure, synchronous, §0.5) refuses: an empty `prompt_ver`; `fanout < 2`;
`max_tier == 0`; `min_window_steps > max_window_steps`; `max_calls_per_pass == 0`;
`gap_minutes == 0`.

The model path (`call.rs`) — this is the one place a governance row reaches a model, and it does
it the way the loop does, so nothing here names a model (P4-D3):

```rust
/// Build the read-only facts for a governance call. `answers_andrey` is FALSE and `wake_kind` is
/// `Scheduled`, so `model-policy`'s prepend listener chooses terra and an agent's
/// `model_override` applies exactly as §12 says it does for unattended work.
pub fn facts(agent: &AgentName, traj: &TrajId, pass: &PassId, cfg: &SummarizerConfig, composition: &str)
    -> RequestFacts;

/// One model call: run `agent/request` for the call config, `llm.stream` for the answer, append
/// the `rollup/request` step, return the assembled text and the token counts.
pub async fn call(ctx: &Context, llm: &LlmHandle, ledger: &LedgerHandle, req: CallRequest)
    -> Result<CallOutcome, RollupsError>;

pub struct CallRequest {
    pub phase: Phase,
    pub facts: Arc<RequestFacts>,
    pub system: String,      // the versioned recap prompt
    pub user: String,        // the rendered window or the rendered children
    pub max_tokens: i64,
    pub tier: u8,
    pub range: SeqRange,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Phase { Map, Reduce, Digest }

pub struct CallOutcome { pub text: String, pub tokens_in: u64, pub tokens_out: u64, pub source: TokenSource }

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource { Provider, Estimate }
```

Rendering (`render.rs`) — PURE, and the reason the offline suite is deterministic:

```rust
/// The recap prompt, versioned. `prompt_ver` in the config must match `PROMPTS`'s key or
/// `validate()` refuses the row: a stamp that names a prompt the binary does not have is a lie.
pub fn system_prompt(phase: Phase, ver: &str) -> Option<&'static str>;
/// One episode window as the model sees it. Steps render as `[seq] kind: one line`, thoughts
/// marked as thoughts, evidence carrying its cites.
pub fn render_window(steps: &[Step], w: &Window) -> String;
/// `fanout` child blocks as the reduce sees them.
pub fn render_children(children: &[Rollup]) -> String;
/// Parse the model's answer into a block. A model that returns prose and no structure still
/// yields a block: `text` is the prose, `themes` is empty, and `evidence` comes from the
/// window, never from the model — the index must not depend on the model's discipline.
pub fn parse_block(answer: &str, inputs: &Inputs, steps: &[Step], cfg: &SummarizerConfig)
    -> Result<TierBlock, RollupsError>;
```

The `/seal` command (`command.rs`), registered only when `commands` is present (P4-D8):

```
/seal [agent] [--plan]      run (or, with --plan, only report) a seal pass for the agent
```

`--plan` calls `Summarizer::plan` and renders the plan with every skip reason. `/seal` with no
agent uses the focused agent from `CommandCx`.

### 2.3 The stub Provider — `plugins/rollups-none/src/…`

```rust
pub const PLUGIN_NAME: &str = "rollups-none";

/// Seals nothing, ever. `plan` returns a plan whose every candidate is `Skip { why: Refused }`;
/// `seal` returns `Stop::NothingToDo`; `supersede` and `rebuild_digest` return
/// `RollupsError::Refused`. It appends NO step and makes NO model call, which is what makes it
/// a truthful stub rather than a slow one.
pub struct RollupsNonePlugin;
```

Config: `struct NoneConfig {}` (`deny_unknown_fields`), so the swap patch is
`config: {}`. It injects `ledger` only — it needs the store to answer `plan` honestly — so the
swap changes no other row's satisfaction.

### 2.4 The reconsolidation row — `plugins/reconsolidation/src/…`

```rust
pub struct Reconsolidation;
impl ServiceKey for Reconsolidation {
    type Value = ReconHandle;
    const NAME: &'static str = "reconsolidation";
}

#[derive(Clone)]
pub struct ReconHandle(pub Arc<ReconInner>);

impl ReconHandle {
    /// What a pass WOULD do. No model call, no write.
    pub async fn plan(&self, req: &PassRequest) -> Result<PassPlan, ReconError>;
    /// Run it. ADDS ONLY (§8): distilled blocks are appended, contradictions become
    /// `claim/proposed` steps, stale evidence becomes `memory/expired` markers. No sealed row
    /// and no raw step is modified or deleted.
    pub async fn run(&self, req: &PassRequest) -> Result<PassReport, ReconError>;
}

bough_util::brand_id!(pub struct ReconPassId;);

#[derive(Clone, Debug)]
pub struct PassRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    pub at: DateTime<Utc>,
    /// Distil from this seq onward. `None` ⇒ the newest `batch_steps`.
    pub since: Option<Seq>,
    /// Phase 4 always `System`; Phase 5's leader writes `Agent { name }` with no shape change.
    pub attribution: Attribution,
    pub max_calls: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PassPlan {
    pub range: SeqRange,
    pub distil: bool,
    pub contradiction_candidates: Vec<Pair>,
    pub expiry_candidates: Vec<Candidate>,
}

/// Two EVIDENCE steps sharing at least one ref, ordered oldest-first. The pure half of
/// contradiction detection: the pairing is arithmetic, the judgement is the model's.
#[derive(Clone, Debug, PartialEq)]
pub struct Pair { pub older: StepId, pub newer: StepId, pub shared: Vec<Ref> }

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate { pub step: StepId, pub kind: StepType, pub age_days: i64, pub why: StaleReason }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StaleReason {
    /// Older than `stale_after_days` and of an expirable kind.
    Age,
    /// A newer EVIDENCE step on the same ref contradicts it (the model said so).
    Contradicted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PassReport {
    pub pass: ReconPassId,
    /// The distilled digest, when the pass produced one.
    pub distilled: Option<RollupId>,
    /// The `claim/proposed` steps the contradictions became.
    pub contradictions: Vec<StepId>,
    /// The `memory/expired` markers appended.
    pub expired: Vec<StepId>,
    pub calls: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconConfig {
    pub batch_steps: usize,
    pub stale_after_days: i64,
    /// The ONLY kinds a pass may expire. `NEVER_EXPIRABLE` is intersected out in code, so a
    /// misconfiguration cannot expire a pin (V7).
    pub expirable_kinds: Vec<String>,
    pub max_contradiction_pairs: usize,
    pub max_calls_per_pass: usize,
    pub distill_max_tokens: i64,
}
```

Distillation calls `ctx.rollups.rebuild_digest(DigestRequest { from_raw: false, .. })`: the
summarizer owns every seal in the tree, so "reconsolidation adds a block" and "the summarizer
seals a block" are one code path and cannot disagree about `prompt_ver`, `sealed_at` or the
`rollup/sealed` step (P4-D6).

Pure algorithms (`detect.rs`), each a unit test rather than a model run:

```rust
/// Evidence steps sharing a ref, newest-vs-older, capped and deterministic.
pub fn pairs(steps: &[Step], max: usize) -> Vec<Pair>;
/// Stale by age. Never returns a `NEVER_EXPIRABLE` kind, whatever the config says.
pub fn stale(steps: &[Step], now: DateTime<Utc>, cfg: &ReconConfig) -> Vec<Candidate>;
/// The `claim/proposed` body for a judged contradiction. Cites BOTH steps, so the claim is
/// evidence-backed the moment it is appended.
pub fn contradiction_claim(pair: &Pair, verdict: &str) -> (ClaimProposed, Vec<Cite>);
```

`/reconsolidate [agent] [--plan] [--since <seq>]` on `ctx.commands`, registered only when
`commands` is present.

Invariant: **`a_pass_adds_and_never_edits`** — over the observed stream, every step a pass
appends is of a kind in `{claim/proposed, memory/expired, rollup/sealed, about/line}`; and at
quiesce, no `steps`/`edges` row hash observed before the first pass has changed. Planted-violation
unit tests over the pure `evaluate`.

### 2.5 The drift-watch row — `plugins/drift-watch/src/…`

```rust
pub struct Drift;
impl ServiceKey for Drift {
    type Value = DriftHandle;
    const NAME: &'static str = "drift";
}

#[derive(Clone)]
pub struct DriftHandle(pub Arc<DriftInner>);

impl DriftHandle {
    /// Per-agent stability signals, computed from the ledger. Reads only; appends nothing.
    pub async fn signals(&self, agent: &AgentName, at: DateTime<Utc>) -> Result<Signals, DriftError>;
    /// §8's one-command reset. Rebuilds digest + identity + the about-line's STATE half from raw
    /// evidence; the intent half starts empty; sealed tiers are read and never written.
    pub async fn reset(&self, req: &ResetRequest) -> Result<ResetReport, DriftError>;
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Signals {
    pub agent: AgentName,
    pub window: SeqRange,
    pub samples: usize,
    /// Thought-length variance (§8), over `thought/text` step bodies, in o200k tokens.
    pub thought_len: Stat,
    /// Tool-use distribution, over `tool/call` steps: share per tool, most-used first.
    pub tool_use: Vec<ToolShare>,
    /// Normalised Shannon entropy of `tool_use`, 0.0 (one tool only) .. 1.0 (uniform).
    pub tool_entropy: f64,
    /// Wired, INACTIVE until Phase 5's accept/reject surface exists (§8).
    pub claim_rejection: SignalState,
    pub flags: Vec<DriftFlag>,
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Stat { pub n: usize, pub mean: f64, pub variance: f64, pub cv: f64, pub p50: f64, pub p95: f64 }

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ToolShare { pub tool: String, pub calls: usize, pub share: f64 }

/// A signal that exists but cannot be computed yet says SO, rather than reporting a zero that
/// reads like "no rejections" (§16: uncertainty never becomes assertion).
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SignalState {
    Inactive { since: String },
    Active { value: f64, n: usize },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriftFlag { ThoughtLengthUnstable, ToolUseCollapsed, TooFewSamples }

#[derive(Clone, Debug)]
pub struct ResetRequest {
    pub agent: AgentName,
    pub traj: TrajId,
    pub at: DateTime<Utc>,
    pub attribution: Attribution,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResetReport {
    /// The rebuilt digest (`Summarizer::rebuild_digest` with `from_raw: true`).
    pub digest: RollupId,
    pub replaced_digest: Option<RollupId>,
    /// The fresh `about/line` step: state half from raw evidence, intent half EMPTY.
    pub about_line: StepId,
    /// The `drift/reset` step recording the act.
    pub reset_step: StepId,
    /// Sealed tier rows on the trajectory, before and after. Equal, by construction (§8).
    pub tiers_before: usize,
    pub tiers_after: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftConfig {
    pub window_steps: usize,
    pub min_samples: usize,
    /// Coefficient of variation above which `ThoughtLengthUnstable` is raised.
    pub thought_len_cv_flag: f64,
    /// Normalised entropy below which `ToolUseCollapsed` is raised.
    pub tool_entropy_flag: f64,
}
```

Pure algorithms (`signals.rs`): `stat(&[usize]) -> Stat`, `shares(&[Step]) -> Vec<ToolShare>`,
`entropy(&[ToolShare]) -> f64`, `flags(&Signals, &DriftConfig) -> Vec<DriftFlag>`. Every one takes
its inputs as data, so the whole signal surface is unit-tested without a ledger.

Commands (registered only when `commands` is present):

```
/drift [agent]                      render the signals and any flags
/reset <agent>                      §8's one-command reset
/supersede <rollup-id> <reason>     supersede a suspected-bad tier block
```

`/supersede` is a thin call to `ctx.rollups.supersede`. It lives here, not on the summarizer,
because §8 puts "if a tier block itself is suspected bad" inside the drift-watch paragraph and the
suspicion is what drift-watch surfaces.

Invariant: **`a_reset_rebuilds_and_never_reseals`** — for every `drift/reset` observed, the
`about/line` it names has an empty intent half, and the count of `rollup/sealed` observations of
kind `tier` is unchanged across the reset. Planted-violation unit tests over the pure `evaluate`.

### 2.6 The projection edits — `plugins/projection-assembler/src/…`

New `expiry.rs`:

```rust
/// Load the expiry set for one assembly. Honours `as_of` exactly as every band does: a marker
/// appended after the request being reproduced did not exist for it (§2.7 item 3 of Phase 2).
pub async fn load(req: &SectionRequest) -> Result<Expired, ProjectionError>;
```

Three band edits, each one line of filtering plus its test:

- `bands::tiers` — drop a rollup whose id is in `expired.rollups`. (Superseded rollups are already
  excluded: `RollupQuery::include_superseded` defaults to false.)
- `bands::tail` — drop a step whose id is in `expired.steps`, and count the `tail_floor_steps`
  floor over SURVIVING steps.
- `bands::digest` — render nothing when the pointed digest is expired.

Two bands that deliberately do NOT honour expiry, each with a test that says so:

- `bands::pins` — §3: pins are never expired by reconsolidation. The relief valve is supersession,
  which `live_pins` already implements. An expiry marker naming a pin is IGNORED (V7).
- `bands::mail` — unconsumed mail has its own consumption mechanism (§5's union of `wake/end`
  sets); an expiry marker must not silently un-deliver mail.

### 2.7 Step types added by this phase

| type | owner | class | body | why |
|---|---|---|---|---|
| `rollup/request` | `rollups-summarizer` | Thought | `{ pass, phase, prompt_ver, model, tier, from_seq, to_seq, input_digest, tokens_in, tokens_out, token_source }` | Model-visible ⟺ ledgered (§0.2): the summarizer's request is reconstructible from `(range, prompt_ver, model)`, and this is the row that records the last two. It is also where V4's bench reads its token counts. |
| `memory/expired` | `reconsolidation` | Evidence | `{ targets: Vec<Ref>, reason, kind: expiry \| supersession }` | §8's "APPENDED marker the projector honors". Evidence, so the ledger itself refuses one with no cites: an expiry that cannot say what justified it is not appendable. |
| `drift/reset` | `drift-watch` | Evidence | `{ agent, digest, about_line, signals, attribution }` | The reset is an act on the agent's identity; §3's two-entry-class rule makes it evidence, citing the raw steps the rebuild read. |

`rollup/sealed` is NOT new — Phase 1's `RollupSealed` already exists and is what both governance
rows append for every sealed row, digests included. Phase 4 adds no new `RollupKind`: a distilled
block is a `digest` (P4-D6).

### 2.8 Events added by this phase: none

Every durable fact this phase produces is a STEP, and every step already broadcasts `ledger/step`
(emit, post-commit). A consumer that wants to react to a seal filters that stream on
`kind == "rollup/sealed"`. Adding a `rollups/sealed` event would be a second channel for a fact
that already has one, which §3 forbids in spirit ("a new model-visible input is a new step type,
never a side channel") and §15 item 7's event-catalog gate would count against us for nothing
(P4-D18).

### 2.9 Bundle rows

Appended to `bundles/bough-base.yml` (§8: "these three rows are in `bough-base`"):

```yaml
# Phase 4 (§8, §17 Phase 4): memory governance, non-optional. `rollups-none` is in the catalog
# and in NO bundle — the `ledger-memory` / `projection-probe` precedent; the swap patch names it.
- id: rollups
  plugin: rollups-summarizer
  config:
    prompt_ver: "r4.1"
    gap_minutes: 45
    max_window_steps: 10
    min_window_steps: 2
    fanout: 10
    max_tier: 3
    seal_lag_steps: 20
    max_calls_per_pass: 8
    max_notable_refs: 12
    max_evidence_refs: 24
    max_block_chars: 1200
    map_max_tokens: 1024
    reduce_max_tokens: 1536
- id: reconsolidation
  plugin: reconsolidation
  config:
    batch_steps: 400
    stale_after_days: 90
    expirable_kinds: ["mail/delivered", "tool/result"]
    max_contradiction_pairs: 24
    max_calls_per_pass: 6
    distill_max_tokens: 2048
- id: drift.watch
  plugin: drift-watch
  config:
    window_steps: 500
    min_samples: 20
    thought_len_cv_flag: 1.2
    tool_entropy_flag: 0.35
```

`max_window_steps: 10` with `fanout: 10` is §3's arithmetic: tier 1 covers ~10 steps, tier 2 ~100,
tier 3 ~1000. `seal_lag_steps: 20` is twice the assembler's `tail_floor_steps: 10`, so a sealed
tier and the verbatim tail never describe the same steps even after the tail has degraded to its
floor.

The swap fixture, `scripts/tui/fixtures/rollups-none.patch.yml`:

```yaml
entries:
  rollups:
    plugin: rollups-none
    config: {}
```

---

## Work packages

Six packages, disjoint file sets. WP-1 is the only one every other package depends on; WP-2..WP-5
can proceed in parallel against WP-1's signatures as written above, which is why they are written
above in this much detail.

### WP-1: the `rollups` Service Definition

**Files:** `plugins/rollups/` (`Cargo.toml`, `src/lib.rs`, `src/request.rs`, `src/window.rs`,
`src/plan.rs`, `src/block.rs`, `src/expiry.rs`, `src/error.rs`, `src/conformance.rs`,
`src/invariant.rs`).

The key, the trait, the vocabulary of 2.1, and the four pure algorithms every consumer shares:
windowing, tier planning, block-ref extraction, expiry folding. No `Plugin` impl, no `register_plugin!`,
no row — the `plugins/ledger` and `plugins/projection` precedent. The conformance suite is written
here so both providers are judged by one statement of the contract, parameterised by `seals: bool`.
The invariant module owns the event-stream half of seal-once and the index check; the providers
return its specs.

Unit tests it must ship: `window::tests::{a_gap_longer_than_the_cut_ends_the_window,
max_steps_ends_a_window_with_no_gap, the_last_window_is_cut_head_and_is_never_sealed,
a_run_shorter_than_min_steps_yields_no_window, windows_partition_the_run_with_no_overlap}`;
`plan::tests::{tier_one_covers_one_episode_window, tier_k_reduces_exactly_fanout_children,
coverage_is_max_window_steps_times_fanout_to_the_k_minus_one, a_range_already_sealed_is_never_planned_again,
a_superseded_block_still_counts_as_sealed, a_bridge_namespace_block_does_not_block_a_plan,
nothing_within_seal_lag_steps_of_the_head_is_planned, tier_id_is_deterministic_and_excludes_prompt_ver,
a_supersession_id_carries_the_next_generation, the_plan_is_total_every_candidate_is_planned_or_skipped}`;
`block::tests::{refs_of_is_total_over_both_beneath_shapes, notable_refs_caps_by_frequency,
notable_refs_is_empty_when_the_covered_steps_carry_none}`;
`expiry::tests::{parse_folds_markers_into_a_set, a_marker_naming_an_unknown_scheme_is_ignored,
a_pin_kind_is_never_expirable}`;
`invariant::tests::{a_planted_reseal_of_the_same_range_is_reported,
a_generation_that_skips_a_number_is_reported, a_block_naming_a_missing_step_is_reported,
a_clean_stream_passes}`.

### WP-2: `rollups-summarizer` — the recap Provider, and the cost bench

**Files:** `plugins/rollups-summarizer/` (`Cargo.toml`, `src/lib.rs`, `src/resolve.rs`,
`src/call.rs`, `src/render.rs`, `src/prompts.rs`, `src/seal.rs`, `src/digest.rs`,
`src/command.rs`, `src/invariant.rs`, `tests/seal_once.rs`, `tests/tiers.rs`,
`tests/supersede.rs`, `tests/digest.rs`, `tests/conformance.rs`, `tests/cost_bench.rs`),
plus ONE additive edit to `plugins/llm-replay/src/transcript.rs`: a `RecordedChunk::Usage
{ input_tokens, output_tokens, cache_read_tokens?, cache_write_tokens?, delay_ms }` variant
mapping to `Chunk::Usage`, so the offline bench measures provider-shaped numbers (P4-D10).

The map/reduce pass, the deterministic ids, the `rollup/request` + `rollup/sealed` appends under a
synthetic pass wake (P4-D2), the model reached through the `agent/request` waterfall so
`model-policy` picks terra (P4-D3), supersession at generation n+1, and `rebuild_digest`. The
`/seal` command registers only when `commands` is provided.

Unit and integration tests it must ship: `render::tests::{a_window_renders_deterministically,
a_prose_only_answer_still_yields_a_block_whose_evidence_comes_from_the_window,
a_block_is_truncated_to_max_block_chars}`; `resolve::tests::{validate_refuses_a_prompt_ver_the_binary_does_not_have,
validate_refuses_a_fanout_below_two}`; `tests/seal_once.rs::{a_range_is_summarised_exactly_once,
re_sealing_the_same_range_is_refused, a_second_pass_over_an_unchanged_ledger_seals_nothing,
a_prompt_ver_bump_does_not_re_open_a_sealed_range, a_sealed_row_hash_is_unchanged_after_a_supersession,
superseded_by_is_set_once_and_a_second_supersession_is_refused}`;
`tests/tiers.rs::{tier_one_blocks_cover_about_ten_steps, ten_tier_one_blocks_reduce_to_one_tier_two_block,
every_sealed_block_names_refs_into_the_layer_beneath_it,
every_ref_in_a_sealed_block_resolves_to_an_existing_step_or_rollup,
notable_refs_are_the_covered_steps_domain_refs, blocks_are_stamped_with_prompt_ver_and_sealed_at,
the_pass_never_seals_within_seal_lag_steps_of_the_head}`;
`tests/supersede.rs::a_superseded_block_gets_a_generation_and_an_expiry_note`;
`tests/digest.rs::{rebuild_digest_supersedes_and_repoints_the_agent_row,
rebuild_digest_reads_sealed_tiers_and_writes_none}`; `tests/conformance.rs` runs the WP-1 suite
with `seals: true`; `tests/cost_bench.rs::cost_per_lived_day_bench` (`#[ignore]`, run by
`make bench`) — see V4.

Every test mounts `ledger-memory` + `llm-replay` and is offline. One `#[ignore]`d live test,
`tests/seal_once.rs::a_live_haiku_pass_seals_a_readable_block` (`BOUGH_LIVE=1`), proves the recap
prompt produces something a human would keep.

### WP-3: `reconsolidation`

**Files:** `plugins/reconsolidation/` (`Cargo.toml`, `src/lib.rs`, `src/resolve.rs`,
`src/detect.rs`, `src/pass.rs`, `src/vocabulary.rs`, `src/command.rs`, `src/invariant.rs`,
`tests/contradiction.rs`, `tests/distill.rs`, `tests/expiry.rs`).

The pass of 2.4: pair candidates by shared ref, ask the model for a verdict, append a
`claim/proposed` step per confirmed contradiction citing BOTH steps; call
`ctx.rollups.rebuild_digest` for the distilled block; append `memory/expired` markers for stale
evidence. It never calls `seal_rollup`, `supersede_rollup` or any write on a step directly — every
write is either an append of its own three kinds or a call through the rollups seam.

Unit and integration tests it must ship: `detect::tests::{pairs_are_evidence_steps_sharing_a_ref,
pairs_are_capped_and_deterministic, stale_never_returns_a_pin_whatever_the_config_says,
stale_never_returns_a_claim, age_is_measured_against_the_injected_now}`;
`tests/contradiction.rs::{a_planted_contradiction_is_recorded_as_a_claim_step,
the_claim_cites_both_conflicting_steps, a_pair_the_model_clears_produces_no_claim}`;
`tests/distill.rs::{a_pass_adds_a_distilled_block, a_pass_changes_no_sealed_row_hash,
a_pass_changes_no_raw_step_hash, a_pass_deletes_nothing}`;
`tests/expiry.rs::{stale_evidence_is_expired_by_an_appended_marker,
the_marker_cites_what_justified_it, a_pin_is_never_an_expiry_candidate,
expiring_the_same_step_twice_appends_one_more_marker_and_changes_nothing_else}`;
`invariant::tests::{a_planted_edit_is_reported, a_clean_pass_passes}`.

### WP-4: `drift-watch`

**Files:** `plugins/drift-watch/` (`Cargo.toml`, `src/lib.rs`, `src/resolve.rs`, `src/signals.rs`,
`src/reset.rs`, `src/vocabulary.rs`, `src/command.rs`, `src/invariant.rs`, `tests/reset.rs`,
`tests/supersede.rs`, `tests/signals.rs`).

The signals of 2.5 as pure functions over step data, the three commands, and the reset: rebuild the
digest from raw evidence (`from_raw: true`), append a fresh `about/line` whose state half cites the
raw steps it summarises and whose intent half is the empty string, append `drift/reset`. Sealed
tiers are counted before and after and reported; nothing writes them.

Unit and integration tests it must ship: `signals::tests::{stat_computes_variance_and_cv,
entropy_is_zero_for_one_tool_and_one_for_uniform_use, flags_need_min_samples,
thought_length_variance_is_computed_from_thought_text_steps,
tool_use_distribution_is_computed_from_tool_call_steps,
claim_rejection_is_inactive_and_says_since_phase_5}`;
`tests/signals.rs::signals_are_read_only_and_append_nothing`;
`tests/reset.rs::{reset_rebuilds_the_digest_from_raw_evidence,
reset_appends_an_about_line_whose_state_half_cites_raw_steps,
reset_leaves_the_intent_half_empty, reset_leaves_every_sealed_tier_untouched,
reset_repoints_the_agent_row_at_the_new_digest}`;
`tests/supersede.rs::{a_suspected_bad_block_is_superseded_with_an_expiry_note,
seal_once_survives_the_supersession, superseding_a_block_this_provider_did_not_seal_is_refused}`;
`invariant::tests::{a_reset_that_reseals_a_tier_is_reported, a_reset_with_a_non_empty_intent_is_reported}`.

### WP-5: the projection consumes real tiers, and honours expiry

**Files:** `plugins/projection-assembler/src/expiry.rs` (new),
`plugins/projection-assembler/src/{lib.rs, bands.rs, degrade.rs}` (edits),
`plugins/projection-assembler/tests/` (`tier_budget.rs`, `expiry.rs`, `pins.rs`, `goldens.rs`),
`crates/bough/tests/projection_tiers.rs`.

The three band edits and the two deliberate non-edits of 2.6, plus the goldens Phase 1 could not
write because no tier existed. The goldens run against BOTH ledger providers, the Phase-1
precedent (`projection_swap.rs`), with tiers sealed by a scripted `rollups-summarizer` over
`llm-replay` so the fixture is byte-stable.

Unit and integration tests it must ship: `expiry::tests::{load_honours_as_of,
a_marker_above_as_of_is_invisible}`;
`tests/tier_budget.rs::{coarse_survives_and_fine_is_dropped_first,
the_verbatim_tail_shrinks_to_its_floor_before_a_coarse_tier_goes,
pins_digest_and_mail_degrade_last_and_never_silently,
a_tier_whose_notable_refs_miss_the_agent_never_reaches_the_draft}`;
`tests/expiry.rs::{an_expired_step_leaves_the_verbatim_tail,
the_tail_floor_counts_surviving_steps, an_expired_tier_block_leaves_the_tiers_band,
an_expired_digest_renders_nothing, an_expiry_marker_naming_a_pin_is_ignored,
an_expiry_marker_naming_mail_is_ignored}`;
`tests/pins.rs::{a_pin_covered_by_a_sealed_tier_still_rides_the_projection_verbatim,
a_pin_is_never_a_degradation_rungs_first_casualty}`;
`tests/goldens.rs::a_projection_over_real_sealed_tiers_matches_its_golden`;
`crates/bough/tests/projection_tiers.rs::the_golden_matches_on_both_ledger_providers`.

### WP-6: integration — the stub, the rows, the wiring, the swap, the TUI suite

**Files:** `plugins/rollups-none/` (whole crate), `Cargo.toml`, `crates/bough/Cargo.toml`,
`bundles/bough-base.yml`, `crates/bough/tests/rollups_swap.rs`,
`crates/bough/tests/memory_invariants.rs`, `scripts/tui/10-memory.sh`,
`scripts/tui/11-swap-rollups.sh`, `scripts/tui/fixtures/rollups-none.patch.yml`,
`scripts/tui/fixtures/memory.patch.yml`, `scripts/tui/fixtures/seed-trajectory.sql`, `Makefile`
(only if the bench filter needs a target of its own), `BUILD.md`, this file's V4 line.

The stub provider, the three bundle rows, the workspace and launcher wiring, and every screen-level
bullet: §17's testing policy says every TUI-visible behaviour in this phase gets a shell-use script
under `scripts/tui/` run by `make tui-test`, and this phase makes four commands visible.

Tests it must ship: `crates/bough/tests/rollups_swap.rs::{the_stub_provider_seals_nothing,
the_projection_degrades_to_the_verbatim_tail_with_the_stub,
reconsolidation_and_drift_watch_stay_active_with_the_stub,
nothing_in_the_tree_is_failed_after_the_swap, swapping_back_restores_the_tiers_band}`;
`crates/bough/tests/memory_invariants.rs::{the_three_rows_activate_in_the_default_profile,
the_three_rows_activate_headless_without_commands, every_phase_four_invariant_runs_at_quiesce}`;
`scripts/tui/10-memory.sh` bullets `{seal_renders_a_report_in_the_pane, seal_appended_rollup_sealed_steps,
seal_started_no_wake, reconsolidate_renders_a_report, drift_renders_the_signals,
drift_reports_claim_rejection_as_inactive, reset_renders_a_report_and_the_strip_about_line_changes,
reset_left_the_tier_count_unchanged}`; `scripts/tui/11-swap-rollups.sh` bullets
`{tiers_are_on_screen_before_the_patch, the_stub_row_took_over_without_a_restart,
seal_reports_nothing_to_do_under_the_stub, removing_the_patch_restores_the_summarizer}`.

---

## 3. Verification map

Each §17 Phase 4 bullet, and the brief's V1..V7 + SWAP, against the test that proves it. A bullet
is DONE only when the named test has run green.

### V1 — seal-once

| claim | test |
|---|---|
| a raw segment is summarized exactly once | `bough-plugin-rollups-summarizer` `tests/seal_once.rs::a_range_is_summarised_exactly_once` |
| re-sealing the same range is refused | `…seal_once.rs::re_sealing_the_same_range_is_refused` (returns `RollupsError::AlreadySealed`) |
| a second pass over an unchanged ledger is a no-op | `…seal_once.rs::a_second_pass_over_an_unchanged_ledger_seals_nothing` (`Stop::NothingToDo`) |
| a prompt bump does not re-open a range | `…seal_once.rs::a_prompt_ver_bump_does_not_re_open_a_sealed_range` |
| a sealed row cannot change | `…seal_once.rs::a_sealed_row_hash_is_unchanged_after_a_supersession` (via `LedgerStore::row_hashes`, which excludes `superseded_by` by design) |
| …except `superseded_by`, set once | `…seal_once.rs::superseded_by_is_set_once_and_a_second_supersession_is_refused`; ledger's own `invariant::seal_once` |
| the rollups invariant module asserts it over the event stream | `bough-plugin-rollups` `invariant::tests::{a_planted_reseal_of_the_same_range_is_reported, a_generation_that_skips_a_number_is_reported, a_clean_stream_passes}`, mounted live by `crates/bough/tests/memory_invariants.rs::every_phase_four_invariant_runs_at_quiesce` |
| the planner refuses before the store does | `bough-plugin-rollups` `plan::tests::{a_range_already_sealed_is_never_planned_again, a_superseded_block_still_counts_as_sealed}` |

### V2 — the tier tree

| claim | test |
|---|---|
| fanout as configured | `bough-plugin-rollups` `plan::tests::tier_k_reduces_exactly_fanout_children`; `…-summarizer` `tests/tiers.rs::ten_tier_one_blocks_reduce_to_one_tier_two_block` |
| per-tier coverage as configured (tier k ≈ 10^k steps) | `plan::tests::coverage_is_max_window_steps_times_fanout_to_the_k_minus_one`; `tests/tiers.rs::tier_one_blocks_cover_about_ten_steps` |
| both are config, not constants | `resolve::tests::validate_refuses_a_fanout_below_two`; `crates/bough/tests/dump_config.rs` already asserts the composed row is what boots |
| every block carries refs into the raw beneath it | `tests/tiers.rs::every_sealed_block_names_refs_into_the_layer_beneath_it`; `block::tests::refs_of_is_total_over_both_beneath_shapes` |
| tiers are an INDEX: every ref in a projected block resolves | `tests/tiers.rs::every_ref_in_a_sealed_block_resolves_to_an_existing_step_or_rollup`; the runtime half is `bough-plugin-rollups` `invariant::tiers_are_an_index`, plus Phase 1's `projection::invariant::model_visible_is_ledgered` for the cited ids of a rendered section |
| the episode cut is the time gap | `window::tests::{a_gap_longer_than_the_cut_ends_the_window, max_steps_ends_a_window_with_no_gap}` |

### V3 — projection consumes tiers within budget

| claim | test |
|---|---|
| coarse survives, fine is dropped first | `projection-assembler` `tests/tier_budget.rs::coarse_survives_and_fine_is_dropped_first` |
| the verbatim tail shrinks to its floor | `…tier_budget.rs::the_verbatim_tail_shrinks_to_its_floor_before_a_coarse_tier_goes` |
| pins / digest / mail headers degrade last, never silently | `…tier_budget.rs::pins_digest_and_mail_degrade_last_and_never_silently` (asserts the `> DEGRADED:` line) |
| the `notable_refs` filter | `…tier_budget.rs::a_tier_whose_notable_refs_miss_the_agent_never_reaches_the_draft` (Phase 1's pure `bands::tests::a_tier_whose_notable_refs_miss_the_agent_is_filtered_out` stays; this is its end-to-end twin) |
| golden tests against BOTH ledger providers | `projection-assembler` `tests/goldens.rs::a_projection_over_real_sealed_tiers_matches_its_golden` and `crates/bough/tests/projection_tiers.rs::the_golden_matches_on_both_ledger_providers` |

### V4 — summarizer cost measured per lived day

The bench is `bough-plugin-rollups-summarizer` `tests/cost_bench.rs::cost_per_lived_day_bench`,
`#[ignore]`d and run by `make bench` (which filters on `bench`). It is OFFLINE: `ledger-memory` +
`llm-replay`, with `RecordedChunk::Usage` supplying provider-shaped token counts and
`bough_llm::pricing::usage_cost_usd` supplying dollars from the vendored catalog. It:

1. generates a **synthetic day**: 6 wakes spread over 8 laptop-hours, ~35 steps each (a message,
   thoughts, 3-6 tool call/result pairs, a wake end), with inter-wake gaps above `gap_minutes` and
   intra-wake gaps below it, so the episode cut lands on wake boundaries the way a real day does;
2. runs seal passes to `Stop::NothingToDo` (the `max_calls_per_pass` cap means several passes);
3. reads every `rollup/request` step, sums `tokens_in` / `tokens_out` per model, prices each with
   `usage_cost_usd`, and prints one line:
   `cost_per_lived_day steps=… windows=… calls=… tier1=… tier2=… tokens_in=… tokens_out=… usd=…`;
4. asserts a ceiling — `calls <= steps / min_window_steps` and `usd < CEILING` — so the bench is
   also a regression guard against a summarizer that starts calling the model per step.

**The measured number: NOT YET MEASURED.** WP-2 records it here, in this paragraph, on the first
green run, replacing this sentence with the printed line and the date. The design PREDICTION,
recorded now so a wrong order of magnitude is visible immediately: ~210 steps/day → ~21 tier-1
windows → ~21 map calls + ~3 tier-2 reduce calls ≈ 24 calls; ~83k input and ~6.5k output tokens; at
`claude-haiku-4-5-20251001`'s catalogued $1/M in and $5/M out, **≈ $0.12 per lived day**. Tier 3
costs nothing on a single day (it needs ten tier-2 blocks, i.e. ~10 days). If the measurement lands
above ~$0.50/day the design is wrong, not the bench.

### V5 — reconsolidation

| claim | test |
|---|---|
| a planted contradiction becomes a claim step | `bough-plugin-reconsolidation` `tests/contradiction.rs::a_planted_contradiction_is_recorded_as_a_claim_step` |
| the claim is evidence-backed | `…contradiction.rs::the_claim_cites_both_conflicting_steps` |
| distillation ADDS blocks | `tests/distill.rs::a_pass_adds_a_distilled_block` |
| no sealed row modified (row hashes unchanged) | `tests/distill.rs::a_pass_changes_no_sealed_row_hash` |
| no raw step modified | `tests/distill.rs::a_pass_changes_no_raw_step_hash`; `…::a_pass_deletes_nothing` |
| stale evidence expires by an APPENDED marker | `tests/expiry.rs::stale_evidence_is_expired_by_an_appended_marker` |
| the projector honours the marker | `projection-assembler` `tests/expiry.rs::{an_expired_step_leaves_the_verbatim_tail, an_expired_tier_block_leaves_the_tiers_band, an_expired_digest_renders_nothing}` |
| never silent | `tests/expiry.rs::the_marker_cites_what_justified_it` (the marker is EVIDENCE and the ledger refuses an uncited one) |
| the invariant says so at runtime | `reconsolidation` `invariant::tests::{a_planted_edit_is_reported, a_clean_pass_passes}` |

### V6 — drift-watch

| claim | test |
|---|---|
| thought-length variance from the ledger | `drift-watch` `signals::tests::thought_length_variance_is_computed_from_thought_text_steps` + `stat_computes_variance_and_cv` |
| tool-use distribution from the ledger | `signals::tests::tool_use_distribution_is_computed_from_tool_call_steps` + `entropy_is_zero_for_one_tool_and_one_for_uniform_use` |
| claim-rejection wired but inactive until Phase 5 | `signals::tests::claim_rejection_is_inactive_and_says_since_phase_5` |
| signals write nothing | `tests/signals.rs::signals_are_read_only_and_append_nothing` |
| `/reset` rebuilds the digest from raw evidence | `tests/reset.rs::reset_rebuilds_the_digest_from_raw_evidence` |
| …and identity | `tests/reset.rs::reset_repoints_the_agent_row_at_the_new_digest` (§3: identity renders from the agents row + digest + the about-line's state half; it is not stored) |
| …and the about-line STATE half from evidence | `tests/reset.rs::reset_appends_an_about_line_whose_state_half_cites_raw_steps` |
| the intent half starts empty | `tests/reset.rs::reset_leaves_the_intent_half_empty` |
| sealed tiers untouched | `tests/reset.rs::reset_leaves_every_sealed_tier_untouched` |
| a suspected-bad block is superseded, never re-summarized in place | `tests/supersede.rs::a_suspected_bad_block_is_superseded_with_an_expiry_note` |
| seal-once preserved through it | `tests/supersede.rs::seal_once_survives_the_supersession` |
| the invariant says so at runtime | `drift-watch` `invariant::tests::{a_reset_that_reseals_a_tier_is_reported, a_reset_with_a_non_empty_intent_is_reported}` |

### V7 — pins

| claim | test |
|---|---|
| never demoted into tiers | `projection-assembler` `tests/pins.rs::a_pin_covered_by_a_sealed_tier_still_rides_the_projection_verbatim` |
| never a degradation casualty | `tests/pins.rs::a_pin_is_never_a_degradation_rungs_first_casualty` (Phase 1's ladder already puts pins at rung 4 with a flag; this asserts it with real tiers present) |
| never expired by reconsolidation — the pass side | `reconsolidation` `detect::tests::stale_never_returns_a_pin_whatever_the_config_says` |
| never expired by reconsolidation — the projector side | `projection-assembler` `tests/expiry.rs::an_expiry_marker_naming_a_pin_is_ignored` |
| supersession is the only relief valve, and the projector honours it | Phase 1's `live_pins` tests stand; `tests/pins.rs::a_pin_covered_by_a_sealed_tier_still_rides_the_projection_verbatim` includes a superseded pin that does NOT ride |

### SWAP

| claim | test |
|---|---|
| `rollups` becomes a stub by patch, no compile | `crates/bough/tests/rollups_swap.rs::the_stub_provider_seals_nothing`; live, `scripts/tui/11-swap-rollups.sh::the_stub_row_took_over_without_a_restart` |
| the projection degrades to the verbatim tail without error | `rollups_swap.rs::the_projection_degrades_to_the_verbatim_tail_with_the_stub` |
| reconsolidation and drift-watch settle ACTIVE or PENDING, nothing FAILED | `rollups_swap.rs::{reconsolidation_and_drift_watch_stay_active_with_the_stub, nothing_in_the_tree_is_failed_after_the_swap}` |
| swapping back restores tiers | `rollups_swap.rs::swapping_back_restores_the_tiers_band`; `11-swap-rollups.sh::removing_the_patch_restores_the_summarizer` |

### The phase's own gates

`make gates` (build + lint + test + the replay half of the shell-use suite) green; `make bench`
run once and its number recorded in V4; `make live` run once with haiku for
`tests/seal_once.rs::a_live_haiku_pass_seals_a_readable_block`.

---

## 4. The kernel decision (§15 item 5)

**Decision: KEEP the hand-rolled kernel. `cordis-core` is not adopted in this build.**

§15 item 5 says to decide "on the crate's track record, not on features". The track record, read
on 2026-08-26 from `crates.io/api/v1/crates/cordis-core` and the GitHub API:

- First publish **2026-08-19**, seven days ago. Three versions — 0.0.1, 0.0.2, 0.0.4 — the last
  two published within 31 minutes of each other on 2026-08-21, and 0.0.3 never published.
- **42 total downloads**, all of them recent; no reverse dependencies.
- Repository `github.com/dshbox/cordis-rs`, created 2026-08-15, last push 2026-08-25: 28 stars,
  5 forks, 0 subscribers, 1 open issue. Active, and eleven days old.
- MIT, Rust 1.85 / edition 2024. Not archived.

That is a promising project with no track record at all, and the kernel is the one thing in this
tree that everything else is a consequence of: `bough-kernel` already reproduces §0.3's semantics
and is proven by Phase 0's verification list, every Phase 1-3 suite that runs on top of it, and
every green swap gate so far (`swap.rs`, `ledger_swap.rs`, `projection_swap.rs`, `loop_swap.rs`,
`tui_swap.rs`). A
0.0.x dependency at the center would put the harness's composition semantics on a version stream
that can break weekly, in exchange for deleting code that is already written and already green.

**Re-evaluation trigger, recorded so this is a decision and not a refusal:** revisit at Phase 8's
granularity review (§15 item 6) if by then `cordis-core` has reached 0.1+ with a written stability
statement, at least one reverse dependency that is not `dshbox`'s own, and a semantics document
that can be diffed against §0.3. §13's "do not depend on the Cordis ports" stands regardless for
Phases 4-7; the ports remain what they have been all along — algorithm references.

---

## 5. What Phase 4 does NOT build

Named so a reviewer does not look for them:

- **No scheduler.** §8 is explicit: "until the Phase 7 scheduler and lid listener exist,
  reconsolidation runs by manual command". `ctx.schedule` arrives in Phase 6 with the collectors.
  The seal hook is `Summarizer::seal` plus `/seal`; Phase 7's schedule row calls the same method
  and adds no seam.
- **No accept/reject surface.** Contradictions become `claim/proposed` steps and stop there;
  §17 Phase 5 owns the surface that accepts or rejects them, which is also what turns the
  claim-rejection-rate signal from `Inactive` to `Active`.
- **No leader.** `Attribution::System` is what Phase 4 writes. The field exists so Phase 5's
  leader is a value change, not a shape change.
- **No inheritance or reconciliation digests for SPLIT/MERGE.** §4's graph ops are Phase 5.
  `rebuild_digest` builds an agent's STANDING digest; the `src_trajs`-named inheritance digest a
  split writes per child is Phase 5's caller of the same seam method.
- **No drift dashboard.** §17 Phase 8. `/drift` renders text; there is no pane.
- **No projection preview pane.** §17 Phase 8.
- **No change to the old-feed adapter.** It keeps sealing its interim tier-1 blocks until Phase 6
  disables the row. Coexistence is P4-D13.

---

## 6. Decisions taken where REQUIREMENTS is silent

- **P4-D1 — the rollups seam is three crates: Definition, summarizer Provider, `rollups-none`
  stub.** §0.2 forbids splitting preemptively, but this phase's swap gate mandates a second
  provider selectable by patch with no compile, which is the "second provider appears" condition
  the rule names. `reconsolidation` and `drift-watch` stay ONE crate each for exactly the opposite
  reason.
- **P4-D2 — a governance pass runs under a SYNTHETIC wake id (`PassId`, `pass:<uuid7>`), not a
  wake.** §3 requires every step to carry a wake id; §5's wake is an agent TURN and a governance
  pass is not one. The ledger's `wake_step_enclosure` invariant constrains only `step/start` /
  `step/end`, which a pass never appends, so the synthetic id is legal and the pass is greppable
  in the ledger by its own prefix. Consequence, stated: a pass's steps appear in the trajectory
  between wakes and belong to no wake; the projector's de-interleave by wake_id handles them the
  way it handles any other wake id.
- **P4-D3 — the summarizer reaches the model through the `agent/request` waterfall.** It builds
  `RequestFacts { answers_andrey: false, wake_kind: WakeKind::Scheduled, .. }` and dispatches, so
  `model-policy`'s prepend listener chooses terra (or the agent's `model_override`) and NOTHING in
  the summarizer names a model. §12's policy is then one implementation, not two. `model-policy`'s
  invariant tolerates this by construction: a decision with no matching `request/header` is
  explicitly "not this invariant's business".
- **P4-D4 — a tier block's rollup id is deterministic and EXCLUDES `prompt_ver`:**
  `tier:{traj}:{k}:{from}-{to}` for the original, `…#g{n}` for the nth supersession. Including the
  prompt version would let a prompt bump seal the same range twice, which is exactly what §3's
  "summarized exactly once" forbids. The consequence is deliberate: improving the recap prompt does
  NOT retroactively re-summarize history; supersession does, one block at a time, on purpose.
- **P4-D5 — "refs into the raw beneath it" means one hop, not transitive closure.** Tier 1 cites
  raw step ids. Tier k>1 cites its `fanout` children AND carries a bounded `evidence` list of raw
  step ids drawn from them (`max_evidence_refs`), so a projected COARSE block still resolves to raw
  without walking the tree. Carrying every raw id at tier 3 would be thousands of ids per block.
- **P4-D6 — a distilled block is a `digest` rollup, not a fourth `RollupKind`.** §3 fixes the three
  kinds and says a digest is precisely the non-contiguous, FOR-an-agent summary that distillation
  produces. A fourth variant would edit the ledger Definition, which this phase otherwise does not
  touch. Distillation therefore goes through `Summarizer::rebuild_digest`, so every seal in the
  tree has one author.
- **P4-D7 — the projector honours expiry in the ASSEMBLER, not in a `projection/assemble`
  listener.** A waterfall listener can add or drop whole sections; expiry removes one block from
  inside a section, and dropping the whole tiers band because one block expired is a different
  behaviour. The filter is three lines in `bands.rs` plus one pure `expiry.rs`, and the set is
  computed by `bough-plugin-rollups`'s `expiry::parse`, so the projector and the governance rows
  read one implementation.
- **P4-D8 — the three governance rows inject `commands` OPTIONALLY.** They live in `bough-base`;
  `commands` lives in `bough-tui-app`. A required inject would make `bough --profile headless` a
  boot failure (§0.2: an enabled row that never activates is a boot failure), and §8 says memory
  governance is non-optional — so the seam must work with no surface, and the commands are what is
  optional. `memory_invariants.rs::the_three_rows_activate_headless_without_commands` pins it.
- **P4-D9 — drift signals are NOT ledgered; the reset IS.** The signals are a pure function of the
  ledger, rendered by `/drift` into a terminal that no model reads, so §0.2's model-visible ⟺
  ledgered does not oblige a step type (P3-D8's reasoning, unchanged). The RESET changes the
  agent's identity and about-line, both of which reach every future request, so it appends
  `drift/reset` as evidence.
- **P4-D10 — token counts prefer the provider's and fall back to an o200k estimate, and say
  which.** `rollup/request.token_source` is `provider` when the stream yielded a `Chunk::Usage`
  and `estimate` when the count came from `bough_plugin_projection::tokens::count` over the request
  and answer text. `llm-replay` grows a `RecordedChunk::Usage` variant so the offline bench measures
  provider-shaped numbers rather than the harness's own tokenizer twice.
- **P4-D11 — `seal_lag_steps`: nothing within N steps of the head is ever sealed.** Default 20,
  twice the assembler's `tail_floor_steps`. Without it a tier block and the verbatim tail describe
  the same steps and the projection pays for both. It is config, not a constant, because the right
  value follows the assembler's tail configuration and both are deployment-varying.
- **P4-D12 — the kernel decision.** Keep the hand-rolled kernel; see §4 above for the evidence and
  the re-evaluation trigger.
- **P4-D13 — coexistence with the old-feed bridge is by ID NAMESPACE, not by range.** The seal-once
  overlap check considers only rollups whose id is in the `tier:` namespace (`plan::is_ours`).
  Bridge blocks are `old-feed:{source}:{id}` and are invisible to it, so the Phase-3 open item —
  the bridge borrowing foreign row ids into the seq namespace — cannot make the summarizer refuse a
  legitimate range. Consequence, stated plainly: while the adapter is mounted an agent may see BOTH
  a bridge tier-1 block and a real tier-1 block over overlapping material. That is acceptable for a
  row that is throwaway by design and goes `disabled: true` in Phase 6, and both blocks stay sealed
  and valid after it does. The vocabulary is shared — both are `RollupKind::Tier` at tier 1 with a
  `prompt_ver` stamp — so the projection renders them identically and seal-once is respected on both
  sides.
- **P4-D14 — `/seal` is the schedule hook, and the hook is the seam method.** §8 wants governance
  "available from Phase 4, just hand-cranked". Rather than inventing an event or a scheduler stub,
  the hook is `Summarizer::seal(SealRequest)`; `/seal` is its one caller today and Phase 7's
  schedule row is its second. Nothing about the seam changes when the scheduler arrives.
- **P4-D15 — a governance pass runs SYNCHRONOUSLY inside the command dispatch, capped by
  `max_calls_per_pass`.** Consequence, stated: the TUI is unresponsive for the duration of a pass —
  at most `max_calls_per_pass` terra calls, a few seconds at haiku speed. The alternative (spawn the
  pass, add a `--status` command, add a progress event and a pane that listens) is three surfaces
  for a problem Phase 7's scheduler removes entirely by running the pass off the TUI. The shell-use
  suite runs against `llm-replay`, so it does NOT measure the freeze and does not claim to.
- **P4-D16 — stale-evidence expiry has two rules and a hard floor.** A step is a candidate when it
  is EVIDENCE, its kind is in `expirable_kinds` (default `mail/delivered`, `tool/result`), and
  either it is older than `stale_after_days` or the model judged a newer evidence step on a shared
  ref to contradict it. `NEVER_EXPIRABLE` (`pin/*`, `claim/*`) is intersected out in code, so a
  misconfigured `expirable_kinds` cannot reach a pin. REQUIREMENTS says only "stale-evidence expiry
  with a note"; this is the conservative reading, and §15 item 4 ("start conservative, adjust on
  evidence") is the mandate to revisit the numbers in daily use.
- **P4-D17 — a block's `evidence` list comes from the WINDOW, never from the model.** `parse_block`
  takes the raw step ids from the inputs it was given and ignores any the model invents. The index
  property (V2) must not depend on a small model's discipline with ids; the prose may be the
  model's, the index may not be.
- **P4-D18 — Phase 4 adds no events.** Every fact it produces is a step, and steps already
  broadcast `ledger/step`. A `rollups/sealed` event would be a second channel for a fact that has
  one, and §15 item 7's event-catalog gate counts events. The running count after Phase 4 is
  unchanged from Phase 3.
