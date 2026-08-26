# Phase 1 — the ledger and the projection seam: design and work breakdown

Authority: `REQUIREMENTS.md` §0, §3 (all), §5 (the "Context = projection" paragraph), §13, §17
Phase 1. Where this document and REQUIREMENTS disagree, REQUIREMENTS wins and this document is the
bug. Everything REQUIREMENTS does not settle is listed in §6 as a labelled decision (`P1-Dn`);
Phase 0's decisions keep their `D`n numbering and are not restated.

Phase 0's center is a given, including the three deferrals Phase 1 **lives with and does not fix**
(`BUILD.md`, `docs/phase-0-plan.md` §7): the fiber lifecycle is a poll loop; `emit` dispatch is
spawned and never awaited at shutdown (so **every test that observes `ledger/step` awaits a
receipt, never a sleep**); `Cadence::Interval` / `Cadence::OnEvent` are still not dispatched, so
every Phase 1 invariant is `Cadence::OnQuiesce` over a stream its own listener recorded (P1-D14).

Reference implementations (§13) are algorithm sources only and Phase 1 depends on none of them.

---

## 1. Crates

Five product crates and one fixture, all under `plugins/`. Package names are exact.

| path | package | catalog rows (`plugin:`) | provides | injects |
|---|---|---|---|---|
| `plugins/ledger` | `bough-plugin-ledger` | **none** — Service Definition (§0.2) | — | — |
| `plugins/ledger-sqlite` | `bough-plugin-ledger-sqlite` | `ledger-sqlite` | `ledger` | — |
| `plugins/ledger-memory` | `bough-plugin-ledger-memory` | `ledger-memory` | `ledger` | — |
| `plugins/projection` | `bough-plugin-projection` | **none** — Service Definition | — | — |
| `plugins/projection-assembler` | `bough-plugin-projection-assembler` | `projection-assembler` | `projection` | `ledger` |
| `plugins/projection-probe` | `bough-plugin-projection-probe` | `projection-probe` | — | `ledger`, `projection` |

Two service keys are defined in this phase and no others:

```rust
Ledger      => ctx key "ledger",     value LedgerHandle(Arc<dyn Ledger>)          // owned by bough-plugin-ledger
Projection  => ctx key "projection", value ProjectionHandle(Arc<dyn Projection>)  // owned by bough-plugin-projection
```

**A Service Definition crate registers no catalog row** (P1-D1). It is a library that owns the key,
the vocabulary types, the events, the pure algorithms both providers must agree on, the
provider-conformance suite, and the `invariant` module whose specs *the providers* return from
`Plugin::invariants()`. It has no `Plugin` impl, no `register_plugin!`, and no row in any bundle.
Consumers depend on the Definition crate, never on a provider crate (§0.2) — the one exception is
`[dev-dependencies]`, where the assembler's own golden tests link both providers on purpose.

`crates/bough` gains four dependency lines (`ledger-sqlite`, `ledger-memory`,
`projection-assembler`, `projection-probe`) for the single Phase 0 reason: linking them so their
`inventory::submit!` registrations land in the binary. It names no type from any of them.

New workspace dependencies (root `Cargo.toml`, each with a comment naming the row that uses it):

| crate | pin | used by |
|---|---|---|
| `rusqlite` (bundled) | already pinned | `ledger-sqlite` |
| `tiktoken-rs` | already pinned | `projection` (o200k_base budget estimator, §5) |
| `jsonschema` | already pinned, **first real user** | `ledger` (step-type body validation) |
| `schemars`, `serde_json`, `chrono`, `sha2`, `uuid` | already pinned | all of the above |
| `tempfile` | **new**, dev-only | every provider test that wants a real db file |

Nothing else is added. `sqlite-vec` stays dropped (§13).

---

## 2. Public API

Normative. An implementer may add private items freely and may not change a signature here without
editing this document first. Everything is `Send + Sync + 'static`; one tokio runtime (D13).

### 2.1 Ids and scalars (`plugins/ledger/src/id.rs`)

```rust
bough_util::brand_id!(pub struct TrajId;);      // one trajectory (a lane's chain, or a fork)
bough_util::brand_id!(pub struct StepId;);      // uuid v7 by default, caller-supplied in tests
bough_util::brand_id!(pub struct WakeId;);
bough_util::brand_id!(pub struct RollupId;);
bough_util::brand_id!(pub struct ActionId;);
bough_util::brand_id!(pub struct IdemKey;);
bough_util::brand_id!(pub struct AgentName;);
bough_util::brand_id!(pub struct StepType;);    // "wake/start", "probe/note"; dynamic, hence branded
bough_util::brand_id!(pub struct Ref;);         // a routing/matching ref: "gh:o/r#12", "step:<id>"

impl Ref {
    /// The one canonical spelling of an intra-ledger citation (P1-D5).
    pub fn step(id: &StepId) -> Ref;            // "step:<id>"
    pub fn rollup(id: &RollupId) -> Ref;        // "rollup:<id>"
}
impl WakeId { pub fn seed(child: &TrajId) -> WakeId; }   // "seed:<traj>", the fork seed wake

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct Seq(pub u64);                        // 1-based, per trajectory, no gaps

pub const LEDGER_FORMAT_VERSION: u32 = 1;
/// sha256 over the declared ENVELOPE only (table + column names of steps/edges/rollups, in order).
/// Changing it without bumping LEDGER_FORMAT_VERSION fails a test. Step types are not in it.
pub fn envelope_fingerprint() -> &'static str;
```

### 2.2 Entries, cites, refs (`plugins/ledger/src/step.rs`, `refs.rs`)

```rust
/// §3's two entry classes. There is no third: control steps are Thought (P1-D3).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class { Evidence, Thought }

/// §3: cites is a JSON array of {ref, url}. Exactly that, no more.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Cite { #[serde(rename = "ref")] pub r#ref: Ref, #[serde(default)] pub url: Option<String> }

/// What the caller asks to append. `wake` and `at` are mandatory: wake_id on every step (§3),
/// and the clock is injected, never read inside the store (AGENTS.md).
#[derive(Clone, Debug)]
pub struct Append {
    pub traj: TrajId,
    pub wake: WakeId,
    pub kind: StepType,
    pub class: Class,
    pub body: serde_json::Value,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
    /// None ⇒ the provider mints a uuid v7. Tests supply one so goldens are stable (P1-D6).
    pub id: Option<StepId>,
}

/// A committed row. Cheap to clone; the payload of `ledger/step`.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: StepId, pub traj: TrajId, pub seq: Seq, pub at: DateTime<Utc>, pub wake: WakeId,
    pub kind: StepType, pub class: Class,
    pub body: Arc<serde_json::Value>, pub cites: Arc<Vec<Cite>>,
    /// CANONICAL for matching/routing (§3). Derived at append; never written by the caller.
    pub refs: Arc<BTreeSet<Ref>>,
    /// Copied from the step type's definition at append, so a binary that does not know the type
    /// can still decide whether to skip it (P1-D7).
    pub ignorable: bool,
}

// plugins/ledger/src/refs.rs — pure, shared by every provider so step_refs cannot diverge.
/// Union of (a) every cite's `ref` and (b) every `ref`/`refs` value found at ANY depth of `body`
/// (string, or array of strings). Deterministic, order-independent, allocation-bounded.
pub fn body_refs(body: &serde_json::Value) -> BTreeSet<Ref>;
pub fn derive_step_refs(cites: &[Cite], body: &serde_json::Value) -> BTreeSet<Ref>;
```

### 2.3 The merge-extensible step-type map (`plugins/ledger/src/types.rs`)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassRule { Evidence, Thought, Either }

#[derive(Clone)]
pub struct StepTypeDef {
    pub name: StepType,
    /// The body schema. Compiled once at registration; every append is validated against it.
    pub schema: schemars::Schema,
    /// A binary that does not know this type SKIPS such rows on read instead of refusing (§3).
    pub ignorable: bool,
    pub class_rule: ClassRule,
    /// Catalog name of the plugin that declared it; it is what error messages name.
    pub owner: &'static str,
}
impl StepTypeDef { pub fn of<T: schemars::JsonSchema>(name: &str, owner: &'static str) -> Self; }

/// Returned by registration; unregisters on drop of the owning effect, never on its own drop.
pub struct StepTypeToken { /* .. */ }
impl StepTypeToken { pub fn unregister(self); }

/// The 16 types the Definition installs into every provider at construction. Owner "ledger".
pub fn builtin_step_types() -> Vec<StepTypeDef>;
```

**The built-in step types.** Bodies are `schemars`-derived structs in
`plugins/ledger/src/vocabulary.rs`; the table is the contract.

| type | class | body |
|---|---|---|
| `wake/start` | Thought | `{ urgency: immediate\|coalesced\|scheduled\|catchup, trigger: Option<StepId>, claimed: Vec<SeqRange> }` |
| `wake/end` | Thought | `{ reason: completed\|aborted\|error\|max_tokens\|interrupted, cause: Option<String>, consumed: Vec<SeqRange> }` |
| `step/start` | Thought | `{ index: u32 }` |
| `step/end` | Thought | `{ index: u32, outcome: ok\|error, detail: Option<String> }` |
| `request/header` | Thought | `{ prompt_ver: String, sections: Vec<SectionId>, tools: Vec<String>, call: serde_json::Value, composition: String }` |
| `inbox/spliced` | Thought | `{ message: String, op: insert\|claim\|discard, target: next_wake\|next_step, wake: bool }` |
| `mail/delivered` | **Evidence** | `{ class: wake\|ordinary, from: Ref, subject: String, summary: String, refs: Vec<Ref> }` |
| `rollup/sealed` | **Evidence** | `{ rollup: RollupId, kind, tier: u8, from_seq: Seq, to_seq: Seq, prompt_ver: String }` |
| `pin/set` | Either | `{ title: String, text: String, supersedes: Vec<StepId> }` |
| `pin/retire` | Thought | `{ retires: Vec<StepId>, reason: String }` |
| `claim/proposed` | Thought | `{ claim: String, kind: String, title: String, body: String }` |
| `claim/accepted` | **Evidence** | `{ claim: String, proposal: StepId, edited: bool }` |
| `claim/rejected` | Thought | `{ claim: String, proposal: StepId, reason: String }` |
| `action/intent` | Thought | `{ action: ActionId, idem_key: IdemKey, kind: String, target: String, payload_digest: String }` |
| `action/done` | **Evidence** | `{ action: ActionId, status: done\|failed, artifact: Option<String> }` |
| `fork/end-seed` | Thought | `{ parent: TrajId, at_seq: Seq }` |

`SeqRange { from: Seq, to: Seq }` (inclusive) is the only compound scalar; §5's consumed-set union
is a set of ranges, unioned order-independently by `SeqRange::union(..)`.

### 2.4 Edges, rollups, agents, actions (`plugins/ledger/src/rows.rs`)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum EdgeKind { Ancestor, Merge }
#[derive(Clone, PartialEq, Debug)]
pub struct Edge { pub child: TrajId, pub parent: TrajId, pub at_seq: Seq, pub kind: EdgeKind, pub at: DateTime<Utc> }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")] pub enum RollupKind { Tier, Digest, Reconciliation }

#[derive(Clone, Debug)]
pub struct NewRollup {
    pub id: Option<RollupId>, pub traj: TrajId, pub kind: RollupKind, pub tier: u8,
    pub from_seq: Seq, pub to_seq: Seq, pub src_trajs: Vec<TrajId>,
    pub body: serde_json::Value, pub notable_refs: BTreeSet<Ref>,
    pub prompt_ver: String, pub sealed_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Rollup { /* NewRollup's fields, resolved */ pub superseded_by: Option<RollupId> }

/// §3: agents is MUTABLE CONFIG, explicitly exempt from append-only.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub name: AgentName, pub traj: TrajId, pub routing_refs: BTreeSet<Ref>,
    pub wake_classes: BTreeSet<String>, pub model_override: Option<String>,
    pub tick_floor: Option<Duration>, pub digest_rollup: Option<RollupId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum ActionStatus { Intent, Done, Failed }
#[derive(Clone, Debug)]
pub struct NewAction { pub id: Option<ActionId>, pub wake: WakeId, pub idem_key: IdemKey,
                       pub kind: String, pub payload: serde_json::Value, pub at: DateTime<Utc> }
#[derive(Clone, Debug, PartialEq)] pub struct ActionRow { /* .. */ pub status: ActionStatus }
```

Phase 1 owns the actions **table** and the two step types only; the `ctx.actions` seam, the
idem_key formula and crash reconciliation are Phase 2 (§17). `action_intent` / `action_done` here
are storage, not policy (P1-D11).

### 2.5 The `ledger` trait (`plugins/ledger/src/lib.rs`)

```rust
pub struct Ledger;                       // the ServiceKey
impl ServiceKey for Ledger { type Value = LedgerHandle; const NAME: &'static str = "ledger"; }
#[derive(Clone)] pub struct LedgerHandle(pub Arc<dyn LedgerStore>);

#[async_trait::async_trait]
pub trait LedgerStore: Send + Sync + 'static {
    fn provider(&self) -> &'static str;
    fn format_version(&self) -> u32;

    // ---- step types (merge-extensible map, §3) --------------------------------
    fn register_step_type(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError>;
    fn step_types(&self) -> Vec<StepTypeDef>;
    /// Rows skipped on read because their type was unknown AND ignorable. Monotone; the
    /// invariant module and the TUI read it.
    fn skipped_ignorable(&self) -> u64;

    // ---- append: ONE writer, seq allocated inside the commit -------------------
    async fn append(&self, req: Append) -> Result<Step, LedgerError>;
    /// One transaction, one contiguous seq run, one `ledger/step` per step, in order.
    async fn append_batch(&self, reqs: Vec<Append>) -> Result<Vec<Step>, LedgerError>;

    // ---- read ------------------------------------------------------------------
    async fn step(&self, id: &StepId) -> Result<Option<Step>, LedgerError>;
    async fn steps(&self, q: &StepQuery) -> Result<Vec<Step>, LedgerError>;
    async fn tail(&self, traj: &TrajId, n: usize) -> Result<Vec<Step>, LedgerError>;
    async fn head_seq(&self, traj: &TrajId) -> Result<Option<Seq>, LedgerError>;
    async fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError>;
    /// Live pins for a set of trajectories: every `pin/set` minus every id named by a later
    /// `pin/set.supersedes` or `pin/retire.retires`. Age is never a criterion (§3).
    async fn live_pins(&self, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError>;
    /// DELIVERED mail not named by any `wake/end.consumed` set. Union, order-independent (§5).
    async fn unconsumed_mail(&self, traj: &TrajId) -> Result<Vec<Step>, LedgerError>;

    // ---- edges, forks, membership ---------------------------------------------
    async fn add_edge(&self, e: Edge) -> Result<(), LedgerError>;
    async fn edges(&self, traj: &TrajId) -> Result<Vec<Edge>, LedgerError>;
    async fn ancestry(&self, traj: &TrajId) -> Result<Vec<TrajId>, LedgerError>;
    /// Validates the prefix, writes the edge and the end-seed marker in ONE transaction, or
    /// writes nothing at all.
    async fn fork(&self, req: Fork) -> Result<ForkOutcome, LedgerError>;
    /// own_chain ∪ ancestry ∪ ref_matches, computed AT NEED. Writes nothing, ever (§3).
    async fn connected(&self, agent: &AgentName) -> Result<Connected, LedgerError>;

    // ---- rollups ---------------------------------------------------------------
    async fn seal_rollup(&self, r: NewRollup) -> Result<Rollup, LedgerError>;
    /// The ONE permitted write to a sealed row (§3). Twice on the same row is an error.
    async fn supersede_rollup(&self, old: &RollupId, new: &RollupId) -> Result<(), LedgerError>;
    async fn rollups(&self, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError>;

    // ---- agents (MUTABLE config, exempt from append-only) ----------------------
    async fn put_agent(&self, a: AgentRow) -> Result<(), LedgerError>;
    async fn agent(&self, name: &AgentName) -> Result<Option<AgentRow>, LedgerError>;
    async fn agents(&self) -> Result<Vec<AgentRow>, LedgerError>;
    async fn delete_agent(&self, name: &AgentName) -> Result<(), LedgerError>;

    // ---- actions journal (storage only in Phase 1) -----------------------------
    async fn action_intent(&self, a: NewAction) -> Result<ActionRow, LedgerError>;
    async fn action_done(&self, id: &ActionId, status: ActionStatus, result: serde_json::Value)
        -> Result<(), LedgerError>;
    async fn actions(&self, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError>;

    // ---- integrity: the invariant module's window into the store ---------------
    /// Stable content hash per row of steps / edges / rollups. For rollups the hash EXCLUDES
    /// superseded_by, which is reported separately so a legal set-once write is not a violation.
    async fn row_hashes(&self, scope: HashScope) -> Result<Vec<RowHash>, LedgerError>;
    /// A whole trajectory as plain data, for the file view. Pure input to a pure renderer.
    async fn trajectory_view(&self, traj: &TrajId) -> Result<TrajectoryView, LedgerError>;
}

impl LedgerHandle {
    /// Registration is an EFFECT (§0.2): the disposer unregisters, and unloading the declaring
    /// plugin leaves the map as if it had never mounted.
    pub async fn declare_step_types(&self, ctx: &Context, defs: Vec<StepTypeDef>)
        -> Result<EffectHandle, PluginError>;
}
```

Supporting types:

```rust
#[derive(Clone, Debug, Default)]
pub struct StepQuery {
    pub trajs: Vec<TrajId>,        // empty ⇒ all
    pub kinds: Vec<StepType>,      // empty ⇒ all
    pub class: Option<Class>,
    pub wake: Option<WakeId>,
    pub after: Option<Seq>, pub before: Option<Seq>,
    pub refs: Vec<Ref>,            // any-match against step_refs
    pub order: Order,              // SeqAsc (default) | SeqDesc
    pub limit: Option<usize>,
}
#[derive(Clone, Debug)] pub struct SearchQuery { pub text: String, pub trajs: Vec<TrajId>, pub limit: usize }
#[derive(Clone, Debug)] pub struct SearchHit { pub step: Step, pub snippet: String }
#[derive(Clone, Debug)] pub struct Pin { pub step: StepId, pub traj: TrajId, pub seq: Seq,
                                         pub class: Class, pub title: String, pub text: String }
#[derive(Clone, Debug)] pub struct Fork { pub parent: TrajId, pub child: TrajId,
                                          pub at_seq: Seq, pub at: DateTime<Utc> }
#[derive(Clone, Debug)] pub struct ForkOutcome { pub edge: Edge, pub end_seed: Step }
#[derive(Clone, Debug)] pub struct Connected { pub own: TrajId, pub ancestry: Vec<TrajId>,
                                               pub ref_matches: Vec<TrajId>, pub refs: BTreeSet<Ref> }
impl Connected { pub fn trajectories(&self) -> BTreeSet<TrajId>; }
#[derive(Clone, Debug)] pub struct RowHash { pub table: &'static str, pub id: String,
                                             pub hash: String, pub superseded_by: Option<String> }
#[derive(Clone, Debug)] pub struct TrajectoryView { pub traj: TrajId, pub steps: Vec<Step>,
                                                    pub edges: Vec<Edge>, pub rollups: Vec<Rollup>,
                                                    pub agent: Option<AgentRow> }
```

Errors — every variant names the row and the rule it broke:

```rust
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("step type `{kind}` is not registered (append refused)")]
    UnknownStepTypeOnAppend { kind: StepType },
    #[error("step `{step}` in trajectory `{traj}` has type `{kind}`, unknown to this binary and not ignorable")]
    UnknownStepTypeOnRead { step: StepId, traj: TrajId, kind: StepType },
    #[error("step type `{kind}` is already registered by plugin `{owner}`")]
    DuplicateStepType { kind: StepType, owner: &'static str },
    #[error("an evidence step of type `{kind}` was appended with no cites; evidence requires citations")]
    EvidenceWithoutCites { kind: StepType },
    #[error("step type `{kind}` may only be appended as {expected}, not {got}")]
    ClassRuleViolated { kind: StepType, expected: &'static str, got: &'static str },
    #[error("body of `{kind}` does not match its schema: {detail}")]
    BodySchema { kind: StepType, detail: String },
    #[error("fork of `{parent}` at seq {at_seq} lies inside wake `{wake}`, opened at seq {opened_at} and never closed")]
    ForkInsideOpenWake { parent: TrajId, at_seq: Seq, wake: WakeId, opened_at: Seq },
    #[error("rollup `{0}` is already superseded by `{1}`; superseded_by is set once")]
    AlreadySuperseded(RollupId, RollupId),
    #[error("ledger at `{path}` has format version {found}, this binary speaks {expected}")]
    FormatVersion { path: String, found: u32, expected: u32 },
    #[error("no such trajectory `{0}`")] NoSuchTrajectory(TrajId),
    #[error(transparent)] Store(#[from] anyhow::Error),
}
```

### 2.6 The one ledger event (`plugins/ledger/src/events.rs`)

```rust
/// DURABLE (§0.2): the fact is already committed when this fires. Emitted POST-COMMIT, one per
/// step, in seq order. Emit mode, so an observer can neither fail nor delay the append.
pub struct LedgerStep;
impl EmitEvent for LedgerStep { const NAME: &'static str = "ledger/step"; type Payload = Arc<Step>; }
```

That is the whole Phase 1 ledger event catalog. `wake/*`, `step/*`, `request/header`,
`inbox/spliced`, `mail/delivered`, `rollup/sealed`, `pin/*`, `claim/*`, `action/*` are **step
types, not events** (§3): each one's append broadcasts `ledger/step` and nothing else. A consumer
that wants "on wake start" filters the payload by `kind`.

### 2.7 The `projection` trait (`plugins/projection/src/lib.rs`)

```rust
pub struct Projection;
impl ServiceKey for Projection { type Value = ProjectionHandle; const NAME: &'static str = "projection"; }
#[derive(Clone)] pub struct ProjectionHandle(pub Arc<dyn Projector>);

bough_util::brand_id!(pub struct SectionId;);

/// The six fixed bands, in the order §5 fixes them. Contributed sections name a band and a side.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Slot { Identity, Pins, Digest, Tiers, Tail, Mail }
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)] pub enum Place { Before, After }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub struct Position { pub slot: Slot, pub place: Place }
impl Position {
    /// (slot, place, id) — ties break by SectionId, NEVER by registration order, because fiber
    /// activation order is not deterministic (P1-D8).
    pub fn sort_key<'a>(&self, id: &'a SectionId) -> (Slot, Place, &'a str);
}

/// Which rung of the degradation ladder drops this section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum DropPriority { Fine, Coarse, Never }

#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum SectionScope { Global, Agent }

pub struct SectionSpec {
    pub id: SectionId,
    pub position: Position,
    /// Agent scope SHADOWS a global section with the same id, for that agent alone (§5).
    pub scope: SectionScope,
    pub agent: Option<AgentName>,           // Some iff scope == Agent
    pub priority: DropPriority,
    pub render: Arc<dyn SectionRender>,
}

#[async_trait::async_trait]
pub trait SectionRender: Send + Sync + 'static {
    /// `Ok(None)` ⇒ the section contributes nothing this time and does not appear at all.
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError>;
}
#[derive(Clone, Debug)]
pub struct SectionRequest { pub agent: AgentName, pub wake: Option<WakeId>, pub at: DateTime<Utc>,
                            pub ledger: LedgerHandle, pub connected: Arc<Connected> }
#[derive(Clone, Debug)]
pub struct SectionBody { pub title: String, pub body: String, pub cites: SectionCites }
/// Model-visible ⟺ ledgered (§0.2): every section says which ledger rows it renders from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionCites { pub steps: Vec<StepId>, pub rollups: Vec<RollupId> }

#[async_trait::async_trait]
pub trait Projector: Send + Sync + 'static {
    fn provider(&self) -> &'static str;
    fn section(&self, spec: SectionSpec) -> Result<SectionToken, ProjectionError>;
    async fn assemble(&self, req: &AssembleRequest) -> Result<Assembled, ProjectionError>;
    /// A pure function of the ledger; writes nothing.
    async fn file_view(&self, req: &FileViewRequest) -> Result<String, ProjectionError>;
    /// `file_view` plus one write. Returns the path written.
    async fn write_file_view(&self, req: &FileViewRequest, dir: Option<&Path>)
        -> Result<PathBuf, ProjectionError>;
}

impl ProjectionHandle {
    /// §5's `ctx.projection.section()`: an effect, so unloading the contributor removes it.
    pub async fn section(&self, ctx: &Context, spec: SectionSpec)
        -> Result<EffectHandle, PluginError>;
}

#[derive(Clone, Debug)]
pub struct AssembleRequest { pub agent: AgentName, pub wake: Option<WakeId>,
                             pub at: DateTime<Utc>, pub budget: Option<usize> }
#[derive(Clone, Debug)] pub struct FileViewRequest { pub traj: TrajId, pub at: DateTime<Utc> }

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedSection { pub id: SectionId, pub position: Position, pub title: String,
                             pub body: String, pub cites: SectionCites, pub tokens: usize,
                             pub degraded: Option<Degradation> }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Degradation { TiersDropped, TailShrunk, PinsCollapsed, MailCollapsed, DigestTruncated }
/// In-context flags: degradation of pins / digest / mail is NEVER silent (§5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Flag { PinsDegraded, MailDegraded, DigestDegraded, OverBudget }

#[derive(Clone, Debug, PartialEq)]
pub struct Assembled { pub agent: AgentName, pub sections: Vec<RenderedSection>,
                       pub flags: BTreeSet<Flag>, pub tokens: usize, pub budget: usize,
                       pub cites: SectionCites }
impl Assembled {
    /// THE golden surface. Byte-identical across providers is what V4 asserts.
    pub fn to_text(&self) -> String;
}

/// The waterfall §5 puts around the assembler: sections may be added, budget policy may degrade.
pub struct ProjectionAssemble;
impl WaterfallEvent for ProjectionAssemble {
    const NAME: &'static str = "projection/assemble";
    type Value = Draft;
}
#[derive(Clone, Debug)]
pub struct Draft { pub request: Arc<AssembleRequest>, pub sections: Vec<RenderedSection>,
                   pub budget: usize, pub flags: BTreeSet<Flag> }
```

Pure algorithms the Definition owns so a second provider cannot drift (`plugins/projection/src/`):

```rust
// tokens.rs — tiktoken-rs o200k_base in a OnceLock, plus §5's headroom factor.
pub fn count(text: &str) -> usize;
pub fn effective_budget(budget_tokens: usize, headroom: f32) -> usize;   // floor(b * h)

// order.rs
pub fn order(sections: &mut Vec<RenderedSection>);   // by Position::sort_key

// file_view.rs — the render is a PURE FUNCTION of the ledger (V8).
pub fn render_file_view(view: &TrajectoryView, at: DateTime<Utc>) -> String;
```

### 2.8 `ledger-sqlite`: schema, triggers, indexes

```sql
PRAGMA journal_mode = WAL;         -- skipped for ":memory:"
PRAGMA foreign_keys = ON;
PRAGMA user_version = 1;           -- LEDGER_FORMAT_VERSION; a mismatch on open fails loud

CREATE TABLE steps (
  id TEXT PRIMARY KEY, traj_id TEXT NOT NULL, seq INTEGER NOT NULL, at TEXT NOT NULL,
  wake_id TEXT NOT NULL, type TEXT NOT NULL, class TEXT NOT NULL CHECK (class IN ('evidence','thought')),
  body TEXT NOT NULL, cites TEXT NOT NULL, ignorable INTEGER NOT NULL DEFAULT 0,
  UNIQUE (traj_id, seq));
CREATE INDEX idx_steps_traj_seq  ON steps(traj_id, seq);
CREATE INDEX idx_steps_type      ON steps(type, traj_id, seq);
CREATE INDEX idx_steps_wake      ON steps(wake_id, seq);

CREATE TABLE step_refs (step_id TEXT NOT NULL REFERENCES steps(id), ref TEXT NOT NULL,
                        PRIMARY KEY (step_id, ref));
CREATE INDEX idx_step_refs_ref ON step_refs(ref);

CREATE TABLE edges (child_traj TEXT NOT NULL, parent_traj TEXT NOT NULL, at_seq INTEGER NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('ancestor','merge')), at TEXT NOT NULL,
                    PRIMARY KEY (child_traj, parent_traj, kind));

CREATE TABLE rollups (id TEXT PRIMARY KEY, traj_id TEXT NOT NULL, kind TEXT NOT NULL, tier INTEGER NOT NULL,
                      from_seq INTEGER NOT NULL, to_seq INTEGER NOT NULL, src_trajs TEXT NOT NULL,
                      body TEXT NOT NULL, notable_refs TEXT NOT NULL, prompt_ver TEXT NOT NULL,
                      sealed_at TEXT NOT NULL, superseded_by TEXT);
CREATE INDEX idx_rollups_traj_tier ON rollups(traj_id, tier, from_seq);

CREATE TABLE actions (id TEXT PRIMARY KEY, wake_id TEXT NOT NULL, idem_key TEXT NOT NULL UNIQUE,
                      kind TEXT NOT NULL, payload TEXT NOT NULL, status TEXT NOT NULL,
                      result TEXT, at TEXT NOT NULL, done_at TEXT);

-- MUTABLE CONFIG, explicitly exempt from append-only (§3). No triggers here, on purpose.
CREATE TABLE agents (name TEXT PRIMARY KEY, traj_id TEXT NOT NULL, routing_refs TEXT NOT NULL,
                     wake_classes TEXT NOT NULL, model_override TEXT, tick_floor INTEGER,
                     digest_rollup_id TEXT);

CREATE VIRTUAL TABLE steps_fts USING fts5(body, cites, content='steps', content_rowid='rowid');
CREATE TRIGGER steps_fts_ins AFTER INSERT ON steps BEGIN
  INSERT INTO steps_fts(rowid, body, cites) VALUES (new.rowid, new.body, new.cites); END;

-- Append-only, enforced BELOW the Rust API so a raw connection cannot get around it (V1).
CREATE TRIGGER steps_no_update BEFORE UPDATE ON steps
  BEGIN SELECT RAISE(ABORT, 'ledger: steps is append-only'); END;
CREATE TRIGGER steps_no_delete BEFORE DELETE ON steps
  BEGIN SELECT RAISE(ABORT, 'ledger: steps is append-only'); END;
CREATE TRIGGER edges_no_update ...  CREATE TRIGGER edges_no_delete ...
CREATE TRIGGER step_refs_no_update ... CREATE TRIGGER step_refs_no_delete ...
CREATE TRIGGER rollups_no_delete BEFORE DELETE ON rollups
  BEGIN SELECT RAISE(ABORT, 'ledger: rollups are sealed'); END;
-- The ONE permitted write to a sealed row: NULL -> non-NULL superseded_by, nothing else moving.
CREATE TRIGGER rollups_seal_once BEFORE UPDATE ON rollups WHEN
     OLD.superseded_by IS NOT NULL OR NEW.superseded_by IS NULL
  OR NEW.id <> OLD.id OR NEW.traj_id <> OLD.traj_id OR NEW.kind <> OLD.kind
  OR NEW.tier <> OLD.tier OR NEW.from_seq <> OLD.from_seq OR NEW.to_seq <> OLD.to_seq
  OR NEW.src_trajs <> OLD.src_trajs OR NEW.body <> OLD.body
  OR NEW.notable_refs <> OLD.notable_refs OR NEW.prompt_ver <> OLD.prompt_ver
  OR NEW.sealed_at <> OLD.sealed_at
  BEGIN SELECT RAISE(ABORT, 'ledger: superseded_by is the one set-once write to a sealed rollup'); END;
```

Config (validated purely, §0.5):

```rust
#[derive(Deserialize, Serialize, JsonSchema, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct SqliteConfig {
    pub path: PathBuf,                                   // ":memory:" allowed
    #[serde(default = "default_busy_timeout")] pub busy_timeout_ms: u64,   // 5000
}
```

`ledger-memory` config is an empty `#[serde(deny_unknown_fields)] struct MemoryConfig {}` so the
swap patch can write `config: {}`.

**Single writer.** One `Arc<Mutex<rusqlite::Connection>>`; every store call runs inside
`tokio::task::spawn_blocking`. Seq is allocated by `SELECT COALESCE(MAX(seq),0)+1 FROM steps WHERE
traj_id = ?` **inside the same transaction** as the insert — an index seek on
`idx_steps_traj_seq`, so no counter table and no second append-only exemption (P1-D9).
`ledger/step` is emitted **after** the transaction commits, from the provider's captured `Context`.

### 2.9 `projection-assembler`: assembly and degradation

Config:

```rust
#[derive(Deserialize, Serialize, JsonSchema, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct AssemblerConfig {
    pub budget_tokens: usize,      // 160_000
    pub headroom: f32,             // 0.6 — §5's factor, recalibrated in this phase's own trajectories
    pub tail_steps: usize,         // 60
    pub tail_floor_steps: usize,   // 10   — the floor §5 names
    pub mail_newest_n: usize,      // 5    — the "newest N" of a collapsed mail header
    pub max_tiers: u8,             // 3
    pub file_view_dir: PathBuf,    // !!expr bough_path("views")
}
```
`validate()` (pure): `0.0 < headroom <= 1.0`, `tail_floor_steps <= tail_steps`, `budget_tokens > 0`,
`mail_newest_n > 0`. Anything else is a bundle typo and fails loud at compose.

**Assembly, normative and deterministic.** No LLM, no clock read, no filesystem in the request
path; `at` comes from the request.

1. `connected(agent)` — own chain ∪ ancestry ∪ ref matches, computed at need.
2. Render the six built-in bands in `Slot` order:
   - **Identity** — the `agents` row (name, trajectory, routing refs, wake classes, model override)
     plus the digest pointer. The about-line's state half arrives in Phase 2 as a *contributed*
     section at `Position { Identity, After }` (P1-D12).
   - **Pins** — `live_pins(connected)`, verbatim, oldest first, each with its step id. Never
     filtered by age, never demoted.
   - **Digest** — the agent's `digest_rollup`, if any. With zero rollups the band renders nothing
     and no header (Phase 4 produces them).
   - **Tiers** — rollups of kind `tier`, **coarse to fine** (highest tier first), tier ≤
     `max_tiers`, kept when `notable_refs ∩ agent.refs ≠ ∅` **or** `notable_refs` is empty (P1-D13).
   - **Tail** — the newest `tail_steps` steps of the agent's own chain, verbatim, oldest first,
     **de-interleaved by `wake_id`** (§3: "the projector de-interleaves concurrent wakes by
     wake_id"). Selection of the window is by seq; presentation groups the selected steps by wake,
     wakes ordered by their first selected seq, seq order preserved inside a wake. Two wakes that
     ran concurrently therefore read as two blocks, never as an interleaving, and the grouping is a
     pure function of the rows (no clock, no arrival order).
   - **Mail** — `unconsumed_mail`, newest first, grouped by class.
3. Render every registered `SectionSpec` whose scope admits this agent; an `Agent`-scoped spec
   shadows a `Global` spec with the same `SectionId` for that agent alone.
4. `order()` — by `(Slot, Place, SectionId)`.
5. Dispatch `projection/assemble` (waterfall) over the `Draft`.
6. Degrade until `tokens <= effective_budget(budget_tokens, headroom)`, **in this fixed order**,
   stopping as soon as it fits:

   | rung | action | flag |
   |---|---|---|
   | 1 | drop `tier` sections finest-first (tier 1, then 2, …) and every contributed section with `DropPriority::Fine` | — |
   | 2 | shrink the tail toward `tail_floor_steps`, oldest first | — |
   | 3 | drop remaining coarse tiers and `DropPriority::Coarse` sections | — |
   | 4 | collapse pins to **titles + count** | `PinsDegraded` |
   | 5 | collapse mail to **per-class counts + newest N** | `MailDegraded` |
   | 6 | truncate the digest body to its first paragraph | `DigestDegraded` |
   | — | still over | `OverBudget` (nothing is silently dropped) |

   `DropPriority::Never` sections and the identity band are never dropped: an answer wake must
   always be buildable (§5).
7. Finalize: `Assembled { sections, flags, tokens, budget, cites }` where `cites` is the union of
   every surviving section's cites — which is exactly what the model-visible ⟺ ledgered invariant
   reads.

`to_text()` renders `## <title>\n\n<body>\n` per section, `flags` as a leading
`> DEGRADED: pins, mail` line when non-empty, and nothing else. No timestamps, no ids of the
running process: the text is a function of (ledger contents, request, config) alone.

### 2.10 Invariants

`plugins/ledger/src/invariant.rs` — specs returned by **both** providers'
`Plugin::invariants()`, so they run against whichever provider is mounted:

| name | cadence | statement |
|---|---|---|
| `append_only_rows_never_change` | OnQuiesce | Across one session, no `steps`/`edges`/`rollups` row hash changes and no row id disappears. |
| `seal_once` | OnQuiesce | A rollup's `superseded_by` transitions at most once, NULL → value, never back. |
| `seq_strictly_grows_per_trajectory` | OnQuiesce | Over the observed `ledger/step` stream: within a trajectory, each step's seq is exactly its predecessor's + 1. |
| `wake_step_enclosure` | OnQuiesce | Every `step/start`..`step/end` pair lies inside a `wake/start`..`wake/end` pair of the same wake; every step carries a wake id. |

`plugins/projection/src/invariant.rs` — returned by the assembler. §3 lists
model-visible ⟺ ledgered among the LEDGER invariants; it is implemented here because the ledger
Definition cannot see a projection section without depending on `projection`, which would invert
the seam (P1-D22). The rule is §3's, unchanged; only its home moves:

| name | cadence | statement |
|---|---|---|
| `model_visible_is_ledgered` | OnQuiesce | Every `SectionCites` entry of every projection assembled this session names a step or rollup id that exists in the ledger. |

Each check is a pure function over an observation record (`evaluate(&[Obs]) -> Result<(), String>`)
plus a store read, exactly as `hello`'s is; the record is cleared per fiber life by an inverse the
plugin's `apply` registers, because a RELOAD keeps the `FiberUid`.

### 2.11 Bundle rows

`bundles/bough-base.yml` gains two rows (and keeps Phase 0's fixture rows untouched):

```yaml
- id: ledger
  plugin: ledger-sqlite
  config:
    path: !!expr 'bough_path("ledger.db")'
    busy_timeout_ms: 5000
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 60
    tail_floor_steps: 10
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path("views")'
```

`ledger-memory` and `projection-probe` are in the binary's catalog but in **no** bundle: the swap
patch names `ledger-memory`, and the probe is mounted by the tests' own `$BOUGH_HOME` bundle.

---

## Work packages

Six packages, disjoint file sets. Order: **WP-1 → {WP-2, WP-3, WP-4} → WP-5 → WP-6.**

**Shared-file rule.** The root `Cargo.toml` belongs to **WP-1 alone** (it pins `tempfile` and adds
the six path deps). `bundles/bough-base.yml`, `crates/bough/Cargo.toml` and `BUILD.md` belong to
**WP-6 alone**. No other package edits a file outside its own crate.

**Test-name rule.** Every conformance case is one named `#[tokio::test]` in each provider's test
file, expanded by the `ledger_conformance!` macro (P1-D10), so both providers carry identical
coverage under real test names and neither can quietly skip a case.

### WP-1: `plugins/ledger` — the Service Definition

Files: `Cargo.toml` (root), `plugins/ledger/Cargo.toml`,
`plugins/ledger/src/{lib,id,step,refs,types,vocabulary,rows,query,events,error,invariant,conformance}.rs`.

Brief: the vocabulary and the rules, and not one line of storage. Write the branded ids, `Class`,
`Cite`, `Append`, `Step`, the edge/rollup/agent/action rows, `StepQuery`, `LedgerError`, and the
`LedgerStore` trait exactly as §2. Write `refs::derive_step_refs` as a pure walk over cites plus
`ref`/`refs` keys at any depth of the body — this function, not the providers, is why `step_refs`
cannot diverge between them. Write the step-type map: `StepTypeDef`, a `jsonschema`-compiled
validator per type, the duplicate-name error, and `builtin_step_types()` with all sixteen bodies as
`schemars`-derived structs. Write `LedgerHandle::declare_step_types` as the effect wrapper.
Write `invariant.rs` with the four specs as `evaluate(&[Obs])` pure functions plus their listener
record. Write `conformance.rs`: the provider-conformance suite as ~35 free `async fn`s over a
`Fixture { ledger: LedgerHandle, ctx: Context, tap: EventTap }`, plus the `ledger_conformance!`
macro that expands them into named tests in a provider's `tests/` file. This crate has no `Plugin`
impl and appears in no bundle.

Tests: `refs::tests::{cites_become_refs, body_ref_key_at_any_depth, body_refs_array_form, extraction_is_order_independent, a_non_string_ref_value_is_ignored}`;
`types::tests::{duplicate_type_is_an_error, unregister_removes_the_type, builtin_types_have_distinct_names, class_rule_is_enforced_per_type, body_failing_its_schema_is_refused}`;
`step::tests::{evidence_without_cites_is_refused, thought_may_carry_cites, seq_range_union_is_order_independent}`;
`format::tests::{envelope_fingerprint_matches_the_declared_format_version, registering_a_step_type_does_not_bump_the_format_version}`;
`invariant::tests::{seq_regression_is_a_violation, a_seq_gap_is_a_violation, wake_step_enclosure_holds, a_step_pair_outside_a_wake_is_a_violation, a_changed_row_hash_is_a_violation, setting_superseded_by_once_is_not_a_row_hash_change, a_second_supersession_is_a_seal_once_violation, a_clean_stream_reports_nothing}`.

### WP-2: `plugins/ledger-sqlite` — the production Provider

Files: `plugins/ledger-sqlite/Cargo.toml`,
`plugins/ledger-sqlite/src/{lib,schema,store,append,read,search,fork,connected,invariant}.rs`,
`plugins/ledger-sqlite/tests/{conformance,schema_triggers,events}.rs`.

Brief: the §3 schema of §2.8, its triggers, its indexes, and the trait over them. The triggers are
the point: `UPDATE`/`DELETE` on `steps`, `step_refs` and `edges` abort at the sqlite level, `rollups`
accepts exactly one `NULL → value` write to `superseded_by` and nothing else, and `agents` carries
no triggers at all because §3 exempts it. Open checks `PRAGMA user_version` and fails loud on a
mismatch. Every call goes through `spawn_blocking` over one `Arc<Mutex<Connection>>`: that mutex is
the "single writer", and seq is allocated by `MAX(seq)+1` **inside** the insert transaction, so two
concurrent appends cannot collide or gap. `append` validates class rules, citations and the body
schema *before* opening the transaction, writes `steps` + `step_refs` (derived, never
caller-supplied) in one commit, and emits `ledger/step` **after** the commit returns. Reads consult
the step-type map: an unknown type refuses with `UnknownStepTypeOnRead` unless the row's stored
`ignorable` flag is set, in which case the row is skipped and counted. FTS5 is external-content
over `steps` with an insert-only trigger. `fork` validates the prefix by scanning the parent's
`wake/*` markers up to `at_seq` and refuses inside an open wake, naming it — one transaction that
writes the edge and the `fork/end-seed` marker at the child's seq 1, or writes nothing.
`connected` is three indexed queries and no writes.

Tests: `tests/schema_triggers.rs::{update_on_steps_is_refused_by_the_trigger, delete_on_steps_is_refused_by_the_trigger, update_on_edges_is_refused, delete_on_edges_is_refused, delete_on_step_refs_is_refused, delete_on_rollups_is_refused, superseded_by_can_be_set_once, a_second_supersession_is_refused_by_the_trigger, an_update_touching_another_rollup_column_is_refused, the_agents_table_accepts_update_and_delete, opening_a_db_with_a_foreign_format_version_fails_loud}`;
`tests/conformance.rs` — the full `ledger_conformance!` expansion;
`tests/events.rs::{ledger_step_arrives_after_the_row_is_readable, a_panicking_listener_does_not_fail_the_append, a_blocking_listener_does_not_delay_the_append, batch_appends_emit_one_event_per_step_in_seq_order}`;
`store::tests::{seq_is_allocated_inside_the_transaction, thirty_two_concurrent_appends_produce_seqs_one_to_thirty_two, tail_uses_the_seq_index}`;
`search::tests::{fts_matches_body_text, fts_matches_cite_text, hits_are_ordered_deterministically}`.

### WP-3: `plugins/ledger-memory` — the test Provider

Files: `plugins/ledger-memory/Cargo.toml`,
`plugins/ledger-memory/src/{lib,store,search,invariant}.rs`,
`plugins/ledger-memory/tests/{conformance,events}.rs`.

Brief: the same trait over `parking_lot::RwLock<Inner>` with `BTreeMap` indexes, existing to prove
the seam and to make the projection goldens fast (§3). It must be a *behavioural* twin, not an
approximation: same seq allocation under concurrency (one write lock), same derived `step_refs`
(the Definition's function, not a re-implementation), same class and schema refusals, same
unknown-type read rule, same fork validation, same `connected`, same deterministic search ordering
(`seq DESC, traj ASC` after a case-insensitive token match over body+cites — the FTS5 result set
for the queries the conformance suite uses, which is the only agreement Phase 1 needs). Append-only
is structural here: there is no mutation method to call, `supersede_rollup` refuses a second write,
and `agents` is the one mutable map. Everything is dropped when the fiber unloads; no persistence,
no file, no config.

Tests: `tests/conformance.rs` — the same `ledger_conformance!` expansion, so any divergence from
`ledger-sqlite` shows up as the same named test failing on one provider;
`tests/events.rs` — the four event cases of WP-2;
`store::tests::{concurrent_appends_produce_a_contiguous_seq_run, supersede_twice_is_refused, no_api_mutates_a_committed_step, an_unloaded_fiber_leaves_no_state}`.

### WP-4: `plugins/projection` — the Service Definition

Files: `plugins/projection/Cargo.toml`,
`plugins/projection/src/{lib,section,order,tokens,file_view,error,invariant}.rs`.

Brief: the `projection` key, the `Projector` trait, the section vocabulary, and the three pure
algorithms every provider must share. `Slot`/`Place`/`Position::sort_key` fixes §5's section order
and breaks ties by `SectionId` — never by registration order, because fiber activation order is not
deterministic and a golden that depends on it is a flake waiting to happen. `tokens::count` wraps
`tiktoken-rs` `o200k_base` in a `OnceLock` and `effective_budget` applies §5's headroom factor.
`file_view::render_file_view(&TrajectoryView, at) -> String` is the whole file-view renderer: it
takes plain data and returns a string, so "the render is a pure function of the ledger" is testable
with no store, no provider and no I/O. `ProjectionHandle::section` is the effect wrapper for
§5's `ctx.projection.section()`. `invariant.rs` holds `model_visible_is_ledgered`. No `Plugin`
impl, no row.

Tests: `order::tests::{fixed_slot_order_is_identity_pins_digest_tiers_tail_mail, before_precedes_the_band_and_after_follows_it, ties_break_by_section_id_not_registration_order, ordering_is_stable_under_shuffled_input}`;
`tokens::tests::{count_is_o200k, headroom_factor_is_applied, effective_budget_floors}`;
`file_view::tests::{render_is_a_pure_function_of_the_view, render_is_stable_across_calls, an_empty_trajectory_renders_a_header_only, rollups_and_edges_appear_in_the_render}`;
`invariant::tests::{a_section_citing_a_missing_step_is_a_violation, a_section_citing_a_missing_rollup_is_a_violation, a_fully_cited_projection_reports_nothing}`.

### WP-5: `plugins/projection-assembler` — the deterministic Provider

Files: `plugins/projection-assembler/Cargo.toml`,
`plugins/projection-assembler/src/{lib,registry,bands,degrade,assemble,invariant}.rs`,
`plugins/projection-assembler/tests/{golden,file_view}.rs`,
`plugins/projection-assembler/tests/golden/*.txt`.

Brief: the assembler of §2.9, and the phase's determinism gate. The registry holds `SectionSpec`s
with agent-scope shadowing global by `SectionId`. `bands.rs` renders the six built-ins from the
ledger, groups the tail by `wake_id` so concurrent wakes de-interleave (§3), and **must work with
zero rollups** — Phase 4 produces tiers and digests, so a band with no
input renders nothing at all rather than an empty header. `degrade.rs` is the fixed ladder as a
data-driven list, so the order is readable in one place and cannot drift into an `if` chain; pins,
mail and digest degrade last and each raises its in-context flag, because §5 forbids degrading them
silently. `assemble.rs` runs the seven steps in order and dispatches the `projection/assemble`
waterfall between rendering and degradation, so a listener may add a section and still be budgeted.
Nothing in the request path reads a clock, the filesystem, or a model. The goldens are plain `.txt`
files compared with `assert_eq!`, rewritten by `UPDATE_GOLDEN=1`; each case is run against BOTH
providers and then against each other, byte for byte.

Tests: `tests/golden.rs::{fixed_section_order_on_sqlite, fixed_section_order_on_memory, degradation_order_on_sqlite, degradation_order_on_memory, pins_collapse_flags_degraded_on_sqlite, pins_collapse_flags_degraded_on_memory, mail_headers_collapse_on_sqlite, mail_headers_collapse_on_memory, agent_section_shadows_global_on_sqlite, agent_section_shadows_global_on_memory, zero_rollups_assembles_on_sqlite, zero_rollups_assembles_on_memory, every_golden_is_byte_identical_between_providers}`;
`registry::tests::{agent_scope_shadows_global_for_that_agent_only, a_disposed_section_stops_rendering, two_sections_in_one_band_order_by_id}`;
`bands::tests::{pins_ride_every_projection_verbatim, a_pin_older_than_the_tail_still_renders, a_superseding_pin_retires_its_predecessor, a_retired_pin_leaves_the_projection, re_accepting_a_requirement_supersedes_its_old_pin, tiers_are_coarse_to_fine, a_tier_whose_notable_refs_miss_the_agent_is_filtered_out, unconsumed_mail_only, tail_de_interleaves_concurrent_wakes_by_wake_id, a_single_wake_tail_is_plain_seq_order, wake_blocks_order_by_their_first_selected_seq}`;
`degrade::tests::{fine_tiers_go_first, then_the_tail_shrinks_to_its_floor, pins_are_never_dropped_before_rung_four, a_collapsed_pin_set_raises_the_degraded_flag, a_collapsed_mail_header_keeps_per_class_counts_and_newest_n, over_budget_after_every_rung_raises_over_budget_and_drops_nothing_silently, a_never_priority_section_survives_every_rung}`;
`assemble::tests::{the_waterfall_runs_between_render_and_degrade, a_listener_added_section_is_budgeted, assembly_reads_no_clock}`;
`tests/file_view.rs::{file_view_writes_the_trajectory_to_a_file, file_view_is_byte_identical_on_both_providers}`.

### WP-6: integration — fixture, bundle rows, swap, bench

Files: `plugins/projection-probe/Cargo.toml`, `plugins/projection-probe/src/{lib,invariant}.rs`,
`bundles/bough-base.yml`, `crates/bough/Cargo.toml`,
`crates/bough/tests/{ledger_swap,projection_swap,ledger_invariants,projection_bench,token_calibration}.rs`,
`BUILD.md`.

Brief: wire the phase into the real composition and prove it survives a patch. `projection-probe`
is a test instrument in the Phase 0 `hello` tradition: it injects `ledger` and `projection`,
declares two step types (`probe/note`, and `probe/scratch` with `ignorable: true`), registers one
global section and one agent-scoped section with the *same* `SectionId` (the shadowing fixture),
appends a small scripted trajectory on `apply`, and records everything it did on a shared trace the
tests assert on. Add the two product rows to `bundles/bough-base.yml` and the four dependency lines
to `crates/bough/Cargo.toml`. The swap tests boot a real `$BOUGH_HOME` through
`bough::compose::compose_plan` and recompose through `bough::watch::recompose_once`, exactly as
Phase 0's `swap.rs` does. The bench builds its fixture through the real append path and measures
`assemble()` as the best of three runs. `token_calibration.rs` discharges the one calibration
§5 asks of this phase ("recalibrate against own trajectories in Phase 1"): it assembles a
projection from this build's own recorded trajectory, counts it with `tokens::count`
(o200k_base), asks Anthropic's `count_tokens` endpoint for the true count through `bough-llm`,
prints `o200k=N anthropic=M ratio=R`, and asserts `R <= 1 / headroom` — i.e. that the shipped 0.6
factor still keeps a request that fits the estimate inside the real window. It is `BOUGH_LIVE=1`
only (an offline gate cannot call the API) and the measured R is recorded in `BUILD.md`'s Phase 1
row, which is what makes the 0.6 in `bough-base.yml` a measured number rather than an inherited
one (P1-D20). Close by updating `BUILD.md`'s Phase 1 row with the named
tests and every deferral this phase took.

Tests: `tests/ledger_swap.rs::{the_base_tree_boots_with_ledger_sqlite, a_patch_swaps_the_row_to_ledger_memory_without_a_recompile, the_assembler_reloads_against_the_new_provider, the_golden_suite_passes_against_the_swapped_provider, the_retired_provider_leaves_no_binding_and_no_listener}`;
`tests/projection_swap.rs::{disabling_the_assembler_leaves_consumers_pending, disabling_the_assembler_fails_nothing, re_enabling_the_assembler_restores_every_consumer, the_probes_sections_are_gone_while_the_assembler_is_disabled}`;
`tests/ledger_invariants.rs::{a_scripted_session_reports_no_ledger_violation, a_planted_seq_gap_is_reported, a_planted_unenclosed_step_pair_is_reported, a_projection_citing_a_missing_step_is_reported}`;
`tests/projection_bench.rs::{assembly_over_10k_steps_is_under_the_bound, assembly_over_100k_steps_is_under_50ms}`;
`tests/token_calibration.rs::{o200k_estimate_stays_within_the_headroom_factor, the_measured_ratio_is_printed_and_recorded}` (both `BOUGH_LIVE=1`, skipped otherwise).

---

## 4. Verification map

A bullet is not done until the named test has run green. A bullet whose test is `#[ignore]`d or
skipped is not done.

| # | claim | test(s) |
|---|---|---|
| **V1** | append-only is enforced by schema tests (UPDATE/DELETE on steps/edges/rollups fails at the sqlite level; `superseded_by` may be set exactly once; the agents table is mutable) and by the ledger invariant (no row hash changes across a session) | `bough-plugin-ledger-sqlite` `tests/schema_triggers.rs::{update_on_steps_is_refused_by_the_trigger, delete_on_steps_is_refused_by_the_trigger, update_on_edges_is_refused, delete_on_edges_is_refused, delete_on_step_refs_is_refused, delete_on_rollups_is_refused, superseded_by_can_be_set_once, a_second_supersession_is_refused_by_the_trigger, an_update_touching_another_rollup_column_is_refused, the_agents_table_accepts_update_and_delete}` · `ledger_memory::store::tests::{no_api_mutates_a_committed_step, supersede_twice_is_refused}` · `conformance::{a_committed_step_is_never_mutated, superseding_twice_is_refused, an_agent_row_can_be_updated_and_deleted}` (both providers) · `bough_plugin_ledger::invariant::tests::{a_changed_row_hash_is_a_violation, setting_superseded_by_once_is_not_a_row_hash_change, a_second_supersession_is_a_seal_once_violation, a_clean_stream_reports_nothing}` · `bough` `tests/ledger_invariants.rs::a_scripted_session_reports_no_ledger_violation` |
| **V2** | evidence steps require cites and a thought never promotes to evidence; step_refs are derived at append from cites and body refs; a step type unknown to the binary is refused on read unless `ignorable: true`; only envelope changes bump the format version | `conformance::{evidence_without_cites_is_refused, a_thought_never_promotes_to_evidence, class_rule_refuses_a_thought_for_an_evidence_only_type, step_refs_come_from_cites, step_refs_come_from_body_refs, step_refs_are_the_union_and_the_caller_cannot_set_them, an_unregistered_type_is_refused_on_append, an_unknown_type_is_refused_on_read, an_unknown_ignorable_type_is_skipped_and_counted}` (both providers) · `bough_plugin_ledger::refs::tests::{cites_become_refs, body_ref_key_at_any_depth, body_refs_array_form, extraction_is_order_independent}` · `…::types::tests::{class_rule_is_enforced_per_type, body_failing_its_schema_is_refused, duplicate_type_is_an_error}` · `…::format::tests::{envelope_fingerprint_matches_the_declared_format_version, registering_a_step_type_does_not_bump_the_format_version}` · `bough` `tests/ledger_swap.rs` (the probe's two types register through the real catalog) |
| **V3** | pins ride every projection verbatim regardless of age and are never demoted; a superseding pin retires its predecessor; re-accepting a requirement supersedes its old pin | `bough_plugin_projection_assembler::bands::tests::{pins_ride_every_projection_verbatim, a_pin_older_than_the_tail_still_renders, a_superseding_pin_retires_its_predecessor, a_retired_pin_leaves_the_projection, re_accepting_a_requirement_supersedes_its_old_pin}` · `…::degrade::tests::pins_are_never_dropped_before_rung_four` · `conformance::{live_pins_excludes_superseded_pins, live_pins_ignores_age, a_supersession_writes_nothing_onto_the_old_pin}` (both providers) · golden case `pins_superseded` in `tests/golden.rs::{fixed_section_order_on_sqlite, fixed_section_order_on_memory}` |
| **V4** | projection golden tests (fixed section order, degradation order, DEGRADED pin collapse, mail-header collapse, per-agent section shadowing a global one) run against BOTH ledger providers and produce byte-identical output | `bough-plugin-projection-assembler` `tests/golden.rs::{fixed_section_order_on_sqlite, fixed_section_order_on_memory, degradation_order_on_sqlite, degradation_order_on_memory, pins_collapse_flags_degraded_on_sqlite, pins_collapse_flags_degraded_on_memory, mail_headers_collapse_on_sqlite, mail_headers_collapse_on_memory, agent_section_shadows_global_on_sqlite, agent_section_shadows_global_on_memory, zero_rollups_assembles_on_sqlite, zero_rollups_assembles_on_memory, every_golden_is_byte_identical_between_providers}` · `…::registry::tests::agent_scope_shadows_global_for_that_agent_only` · `bough_plugin_projection::order::tests::ties_break_by_section_id_not_registration_order` |
| **V5** | a synthetic 100k-step trajectory keeps projection assembly under 50ms on ledger-sqlite (env-gated, prints the number, asserts the bound), plus a cheaper always-on 10k check | `bough` `tests/projection_bench.rs::assembly_over_100k_steps_is_under_50ms` (runs only with `BOUGH_BENCH=1`; prints `assemble(100k) = N.NNms`) · `bough` `tests/projection_bench.rs::assembly_over_10k_steps_is_under_the_bound` (always on, best of three, same 50ms bound) |
| **V6** | a fork requires its prefix to end outside an open wake and rejects one that does not (no silent clipping); the child's first live step is end-seed; connected(agent) is computed at need and a late-linked ref includes its history retroactively with nothing written onto entries | `conformance::{fork_at_a_closed_prefix_succeeds, fork_inside_an_open_wake_is_refused_naming_the_wake, a_refused_fork_writes_nothing, a_fork_never_clips_the_prefix, the_childs_first_step_is_the_end_seed_marker, the_end_seed_carries_the_parent_and_at_seq, connected_is_own_chain_plus_ancestry_plus_ref_matches, connected_reads_the_agents_row_at_call_time, a_late_linked_ref_includes_history_retroactively, linking_a_ref_changes_no_step_row_hash, connected_writes_nothing}` (both providers) · `bough_plugin_ledger_sqlite::fork::tests::{open_wake_scan_stops_at_at_seq, a_wake_closed_before_at_seq_is_not_open}` |
| **V7** | `ledger/step` is emitted post-commit; a listener that panics or blocks never fails or delays the append; seq strictly grows per trajectory; wake/step enclosure holds | `bough-plugin-ledger-sqlite` `tests/events.rs::{ledger_step_arrives_after_the_row_is_readable, a_panicking_listener_does_not_fail_the_append, a_blocking_listener_does_not_delay_the_append, batch_appends_emit_one_event_per_step_in_seq_order}` · the same four in `bough-plugin-ledger-memory` `tests/events.rs` · `conformance::{seq_starts_at_one_per_trajectory, seq_has_no_gaps, concurrent_appends_produce_a_contiguous_seq_run}` · `bough_plugin_ledger_sqlite::store::tests::thirty_two_concurrent_appends_produce_seqs_one_to_thirty_two` · `bough_plugin_ledger::invariant::tests::{seq_regression_is_a_violation, a_seq_gap_is_a_violation, wake_step_enclosure_holds, a_step_pair_outside_a_wake_is_a_violation}` · `bough` `tests/ledger_invariants.rs::{a_planted_seq_gap_is_reported, a_planted_unenclosed_step_pair_is_reported}` |
| **V8** | FTS search over steps across trajectories returns hits with their cites; file-view projection renders a trajectory to a file and the render is a pure function of the ledger | `conformance::{search_finds_a_step_in_another_trajectory, a_hit_carries_its_cites, search_respects_the_trajectory_filter, search_ordering_is_deterministic}` (both providers) · `bough_plugin_ledger_sqlite::search::tests::{fts_matches_body_text, fts_matches_cite_text, hits_are_ordered_deterministically}` · `bough_plugin_projection::file_view::tests::{render_is_a_pure_function_of_the_view, render_is_stable_across_calls, rollups_and_edges_appear_in_the_render}` · `bough-plugin-projection-assembler` `tests/file_view.rs::{file_view_writes_the_trajectory_to_a_file, file_view_is_byte_identical_on_both_providers}` |
| **SWAP** | the ledger provider row switches from `ledger-sqlite` to `ledger-memory` by patch with no compile and the projection golden suite still passes; disabling the `projection-assembler` row by patch leaves every consumer of `ctx.projection` PENDING with nothing FAILED, and re-enabling restores them | `bough` `tests/ledger_swap.rs::{the_base_tree_boots_with_ledger_sqlite, a_patch_swaps_the_row_to_ledger_memory_without_a_recompile, the_assembler_reloads_against_the_new_provider, the_golden_suite_passes_against_the_swapped_provider, the_retired_provider_leaves_no_binding_and_no_listener}` · `bough` `tests/projection_swap.rs::{disabling_the_assembler_leaves_consumers_pending, disabling_the_assembler_fails_nothing, re_enabling_the_assembler_restores_every_consumer, the_probes_sections_are_gone_while_the_assembler_is_disabled}` |

Beyond the nine bullets, §5 puts one calibration obligation on this phase — "recalibrate
[the 0.6 headroom factor] against own trajectories in Phase 1" — discharged by `bough`
`tests/token_calibration.rs::{o200k_estimate_stays_within_the_headroom_factor,
the_measured_ratio_is_printed_and_recorded}`, `BOUGH_LIVE=1` only, with the measured ratio written
into `BUILD.md`'s Phase 1 row. It gates no other bullet: if the measurement moves the factor, the
change is one number in `bundles/bough-base.yml`, and the goldens are regenerated with it.

**How the SWAP test runs, concretely.** Boot `--profile dev` with `BOUGH_HOME` pointed at a
`TempDir` through `bough::compose::compose_plan`, over a `profiles/` and `bundles/` laid out inside
that home (so the normative §0.5 layer stack is what the gate exercises). `quiesce()`; assert row
`ledger` is ACTIVE with `provider() == "ledger-sqlite"` and `projection` is ACTIVE against it.
Write `{ entries: { ledger: { plugin: ledger-memory, config: {} } } }` into
`$BOUGH_HOME/bough.patch.yml` and recompose through `bough::watch::recompose_once`. Assert: the
`ledger` row's `FiberUid` **changed** (a `plugin` change rebuilds, §0.3 line 107, unlike Phase 0's
in-place reload), the `projection` row's `FiberUid` is unchanged and its trace shows a re-`apply`
against the new provider, the store holds exactly one `ledger` binding whose `ProviderUid.fiber` is
the memory fiber, no listener is owned by the retired fiber, and the fingerprint moved. Then run
the assembler's golden cases through the live `ctx.projection` and assert the same bytes. Then
patch `{ entries: { projection: { disabled: true } } }`: `projection-probe` is PENDING with
`unmet == ["projection"]`, no row is FAILED, and `ledger` is untouched. Remove the `disabled` line:
the probe is ACTIVE again and its sections are back in the assembled text. No recompile, no
restart, one test process.

---

## 5. What Phase 1 does NOT build

Stated so a reviewer does not read an omission as a gap. No agent, no wake loop, no model call, no
tool — the wake/step/request step types exist as **vocabulary and schema only**, written by tests in
Phase 1 and by `agent-loop` in Phase 2. No rollup or digest PRODUCER (Phase 4); the assembler
consumes tiers and digests and must work with zero of them, and `seal_rollup` exists so tests can
plant one. No `ctx.actions` seam, no idem_key formula, no crash reconciliation (Phase 2) — only the
`actions` table and the two step types. No leader, so the pin-overflow SUPERSESSION PROPOSAL §5 asks for "when the leader exists" is
not made here: in Phase 1 the whole behaviour of pin overflow is the collapse plus the in-context
`DEGRADED` flag, and the proposal arrives with the leader in Phase 5. No mail ROUTER, no wake
scheduler, no consumption policy
(Phase 3/5): `unconsumed_mail` is a query over `wake/end` sets, not a delivery mechanism. No TUI
and no FTS pane (Phase 3; the index exists from here). No graph ops — `fork` is the ledger-level
primitive, and split/merge/bud are `graph-ops` in Phase 5. No kernel changes at all: `Cadence::
Interval` / `OnEvent` stay undispatched (P1-D14) and the fiber poll loop stays as Phase 0 left it.

---

## 6. Decisions taken where REQUIREMENTS is silent

- **P1-D1 — a Service Definition crate registers no catalog row.** `plugins/ledger` and
  `plugins/projection` are libraries under `plugins/` with `bough-plugin-*` names, no `Plugin` impl
  and no bundle row; their `invariant` specs are returned by the providers. *Alternative:* a
  Definition row that provides a registry key, rejected because it needs the providers' handle to
  check anything and an optional key absent at activation stays absent for that fiber's life.
- **P1-D2 — the step-type map lives on the ledger HANDLE**, registered through
  `LedgerHandle::declare_step_types` as an effect, mirroring §5's `ctx.projection.section()`.
  *Alternative:* a separate `step-types` service key, rejected as a second seam nobody asked for
  (§15 item 6).
- **P1-D3 — `class` is a stored column, and control step types are `Thought`.** §3's schema line
  omits `class` but is explicitly "roughly"; storing it makes "evidence requires cites" a rule the
  writer is held to and "a thought never promotes" a structural fact rather than a convention.
  Control steps are Thought because nothing renders them as a truth claim; the four that record a
  fact with something to cite (`mail/delivered`, `rollup/sealed`, `claim/accepted`, `action/done`)
  are Evidence and must carry cites.
- **P1-D4 — `pin/retire` exists alongside `pin/set { supersedes }`.** §3 names supersession as the
  relief valve, which covers replacement; withdrawing an accepted requirement with no replacement
  has no other spelling, and without it the pin set can only grow.
- **P1-D5 — intra-ledger citations are refs in a scheme**, `step:<id>` / `rollup:<id>`, so `Cite`
  stays exactly §3's `{ref, url}` and step_refs stays one flat matching index.
- **P1-D6 — `Append::id` is optional and caller-supplied in tests.** Goldens must be byte-stable,
  and a mint-only id would force ids out of the rendered text or into a normalisation pass.
- **P1-D7 — `ignorable` is an envelope column on `steps`,** copied from the type definition at
  append. The reading binary is the one that does not know the type, so the flag has to travel with
  the row rather than live only in the writer's registry.
- **P1-D8 — section ties break by `SectionId`, never by registration order.** Fiber activation
  order is not deterministic, so registration order is not a stable key and a golden built on it
  would flake.
- **P1-D9 — seq is `MAX(seq)+1` inside the insert transaction,** not a counter table. It is an
  index seek, and a counter table would need a second documented exemption from append-only.
- **P1-D10 — a `ledger_conformance!` macro expands the shared suite into named tests** in each
  provider's `tests/` file. One `run_all()` would let a provider fail one case invisibly inside a
  single red test, and would make the failure message useless.
- **P1-D11 — the `actions` table ships in Phase 1, the `actions` POLICY does not.** §3 puts the
  table in the ledger schema; §17 puts the seam in Phase 2. Phase 1 stores rows and nothing more.
- **P1-D12 — the identity band renders the agents row + digest pointer only.** The about-line's
  state half arrives in Phase 2 as a contributed section at `Position { Identity, After }`, which is
  what "plugins contribute sections" is for; inventing an `about/line` step type now would put a
  Phase 2 vocabulary in a Phase 1 crate.
- **P1-D13 — a tier rollup with EMPTY `notable_refs` is always included.** Filtering is by
  intersection with the agent's refs; an empty set has nothing to intersect and dropping it would
  hide a summary from everyone.
- **P1-D14 — every Phase 1 invariant is `Cadence::OnQuiesce`.** Phase 0 left `Interval`/`OnEvent`
  undispatched and named Phase 1 as the likely fixer; the four ledger invariants are all
  expressible as a check over a listener-recorded stream plus a store read at quiesce, so Phase 1
  takes no kernel change and the open item stands.
- **P1-D15 — `spawn_blocking` over one `Arc<Mutex<Connection>>`.** The mutex IS the single writer of
  §3, and no async task ever blocks on sqlite. If V5's bench fails, the fix is a reader pool, and
  that is the one place to change.
- **P1-D16 — `projection-probe` is a normal dependency of `crates/bough`,** as `hello` is: a
  fixture must exercise the real catalog path, and feature-gating it out of the binary would mean
  the swap test no longer tests the shipped composition. Phase 8's fixture audit removes both.
- **P1-D17 — goldens are plain `.txt` files with `UPDATE_GOLDEN=1`,** not `insta`. §13 reserves
  insta for TUI snapshots, and a golden that a reviewer can read in the diff is the point.
- **P1-D18 — the always-on 10k bench asserts the same 50ms bound, best of three runs.** A separate,
  looser bound would be a number nobody chose; best-of-three is the standard de-flake for a wall
  clock on a shared machine.
- **P1-D19 — `search` ordering is `seq DESC, traj ASC`** on both providers, not bm25 rank. Rank
  cannot be reproduced by the memory provider, and the conformance suite's whole value is that the
  two providers answer identically.
- **P1-D20 — the headroom factor stays 0.6 until a live measurement moves it, and the
  measurement is `BOUGH_LIVE=1`.** §5 asks for recalibration in Phase 1 but the offline gates
  cannot call a model API, so the calibration is a live-gated test that prints the measured
  o200k→Anthropic ratio and asserts it stays under `1 / headroom`; `make gates` never depends on
  it. *Alternative:* pick a new constant from the published third-party 1.5–1.7x figures, rejected
  because §5 asks for OUR trajectories, and a number nobody measured is the thing the requirement
  is trying to replace.
- **P1-D21 — the last three degradation rungs run pins → mail → digest, and a rung 3 drops the
  coarse tiers.** §5 fixes only "fine tiers, then the tail, then pins/digest/mail last"; it orders
  neither the three protected bands among themselves nor says what happens when the tail has hit
  its floor and the request still does not fit. Pins collapse before mail because a collapsed pin
  set keeps every title (the model still knows what it is missing) while a collapsed mail header
  loses individual subjects; the digest goes last because it is the agent's only inherited context.
  Rung 3 exists so the ladder is total: dropping the remaining coarse tiers is strictly better than
  touching a protected band, and every rung below 3 raises an in-context flag.
- **P1-D22 — model-visible ⟺ ledgered is implemented in `plugins/projection`, not
  `plugins/ledger`.** §3 lists it among the ledger invariants; the ledger Definition would have to
  depend on `projection` to see a `SectionCites`, inverting the seam (§0.2: Consumers depend on
  Definitions, never the reverse). The check itself is unchanged — every section's cited step and
  rollup ids must exist in the ledger — and it reads the ledger through the injected handle, so
  the rule holds wherever the provider is mounted.
