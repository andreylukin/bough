//! Invariant: this crate is the ledger SERVICE DEFINITION (§0.2, P1-D1). It owns the `ledger`
//! service key, the §3 vocabulary, the one durable event, the pure algorithms both providers must
//! agree on and the provider-conformance suite — and not one line of storage. It has no `Plugin`
//! impl, no `register_plugin!` and no row in any bundle; its invariant specs are returned by the
//! PROVIDERS' `Plugin::invariants()`.
//!
//! Consumers depend on this crate, never on a provider crate.
//!
//! SCAFFOLD: `unused_variables` and `dead_code` are allowed while the bodies are `todo!()` and the
//! private state they thread has no reader yet. Both allows go away with the last `todo!()`.
#![allow(unused_variables, dead_code)]

pub mod conformance;
pub mod error;
pub mod events;
pub mod id;
pub mod invariant;
pub mod query;
pub mod refs;
pub mod rows;
pub mod step;
pub mod types;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, PluginError, ServiceKey};

pub use error::LedgerError;
pub use events::LedgerStep;
pub use id::{
    envelope_fingerprint, ActionId, AgentName, IdemKey, Ref, RollupId, Seq, StepId, StepType,
    TrajId, WakeId, LEDGER_FORMAT_VERSION,
};
pub use query::{
    ActionQuery, Connected, Fork, ForkOutcome, HashScope, Order, Pin, RollupQuery, RowHash,
    SearchHit, SearchQuery, StepQuery, TrajectoryView,
};
pub use rows::{
    ActionRow, ActionStatus, AgentRow, Edge, EdgeKind, NewAction, NewRollup, Rollup, RollupKind,
};
pub use step::{Append, Cite, Class, SeqRange, Step};
pub use types::{builtin_step_types, ClassRule, StepTypeDef, StepTypeMap, StepTypeToken};

/// The `ledger` service key.
pub struct Ledger;

impl ServiceKey for Ledger {
    type Value = LedgerHandle;
    const NAME: &'static str = "ledger";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct LedgerHandle(pub Arc<dyn LedgerStore>);

/// What a ledger provider does.
///
/// One writer: `seq` is allocated atomically inside the same commit as the insert, so two
/// concurrent appends can neither collide nor gap (P1-D9).
#[async_trait::async_trait]
pub trait LedgerStore: Send + Sync + 'static {
    /// Catalog name of the plugin behind this binding; the swap test reads it.
    fn provider(&self) -> &'static str;
    /// The envelope version this store speaks.
    fn format_version(&self) -> u32;

    // ---- step types (merge-extensible map, §3) --------------------------------

    /// Add one step type. `Err(DuplicateStepType)` if the name is taken.
    fn register_step_type(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError>;
    /// Every registered type, sorted by name.
    fn step_types(&self) -> Vec<StepTypeDef>;
    /// Rows skipped on read because their type was unknown AND ignorable. Monotone.
    fn skipped_ignorable(&self) -> u64;

    // ---- append: ONE writer, seq allocated inside the commit -------------------

    /// Validate, commit, then emit `ledger/step`.
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
    /// writes nothing at all. A prefix ending inside an open wake is REFUSED, never clipped (§3).
    async fn fork(&self, req: Fork) -> Result<ForkOutcome, LedgerError>;
    /// `own_chain ∪ ancestry ∪ ref_matches`, computed AT NEED. Writes nothing, ever (§3).
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

    // ---- actions journal (storage only in Phase 1, P1-D11) ---------------------

    async fn action_intent(&self, a: NewAction) -> Result<ActionRow, LedgerError>;
    async fn action_done(
        &self,
        id: &ActionId,
        status: ActionStatus,
        result: serde_json::Value,
    ) -> Result<(), LedgerError>;
    async fn actions(&self, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError>;

    // ---- integrity: the invariant module's window into the store ---------------

    /// Stable content hash per row. For rollups the hash EXCLUDES `superseded_by`, which is
    /// reported separately so a legal set-once write is not a violation.
    async fn row_hashes(&self, scope: HashScope) -> Result<Vec<RowHash>, LedgerError>;
    /// A whole trajectory as plain data, for the file view. Pure input to a pure renderer.
    async fn trajectory_view(&self, traj: &TrajId) -> Result<TrajectoryView, LedgerError>;
}

impl std::fmt::Debug for LedgerHandle {
    /// A handle prints as its provider's catalog name: enough to tell the two providers apart in
    /// a test failure, and no more.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LedgerHandle({})", self.0.provider())
    }
}

impl LedgerHandle {
    /// §5's `ctx.projection.section()` shape, for step types (P1-D2): registration is an EFFECT,
    /// so the disposer unregisters and unloading the declaring plugin leaves the map as if it had
    /// never mounted.
    pub async fn declare_step_types(
        &self,
        ctx: &Context,
        defs: Vec<StepTypeDef>,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-1: LedgerHandle::declare_step_types")
    }
}
