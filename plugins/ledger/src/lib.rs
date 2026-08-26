//! Invariant: this crate is the ledger SERVICE DEFINITION (§0.2, P1-D1). It owns the `ledger`
//! service key, the §3 vocabulary, the one durable event, the pure algorithms both providers must
//! agree on and the provider-conformance suite — and not one line of storage. It has no `Plugin`
//! impl, no `register_plugin!` and no row in any bundle; its invariant specs are returned by the
//! PROVIDERS' `Plugin::invariants()`.
//!
//! Consumers depend on this crate, never on a provider crate.

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
        let store = self.0.clone();
        let entry = ctx.entry_id().clone();
        ctx.effect(move |ectx| async move {
            for def in defs {
                match store.register_step_type(def) {
                    Ok(token) => ectx.defer_sync(token.into_inverse()),
                    // A partial declaration is not a state anyone can reason about: the effect
                    // fails, and `Context::effect` unwinds the inverses already deferred, so the
                    // map is exactly as it was before the call.
                    Err(e) => return Err(PluginError::new(entry.clone(), e)),
                }
            }
            Ok(())
        })
        .await
    }
}

/// The on-disk ENVELOPE, as a version and a fingerprint over it.
///
/// Its own module so the rule has a name: only a structural envelope change bumps
/// [`LEDGER_FORMAT_VERSION`], and registering a step type is not one (§3).
pub mod format {
    pub use crate::id::{envelope_fingerprint, ENVELOPE, LEDGER_FORMAT_VERSION};

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The fingerprint of the envelope declared in `id.rs`, recorded here. If a column is
        /// added, renamed, reordered or removed, this test fails and the fix is to bump
        /// `LEDGER_FORMAT_VERSION` and paste the new digest — which is the point: the bump cannot
        /// be forgotten.
        #[test]
        fn envelope_fingerprint_matches_the_declared_format_version() {
            assert_eq!(LEDGER_FORMAT_VERSION, 1);
            assert_eq!(
                envelope_fingerprint(),
                "824283423bd318f3864d3c9af1446268652aad0886c8e8938c92b8b7ccd89f92",
                "the envelope changed: bump LEDGER_FORMAT_VERSION and record the new fingerprint"
            );
            // Stable across calls: it is a digest of a constant, not of anything runtime.
            assert_eq!(envelope_fingerprint(), envelope_fingerprint());
        }

        /// §3: "Only structural envelope changes bump the ledger format version." A plugin
        /// declaring a step type is the ordinary case and must move neither number.
        #[test]
        fn registering_a_step_type_does_not_bump_the_format_version() {
            #[derive(schemars::JsonSchema)]
            #[allow(dead_code)]
            struct ProbeNote {
                text: String,
            }

            let before = (LEDGER_FORMAT_VERSION, envelope_fingerprint());
            let map = crate::types::StepTypeMap::with_builtins();
            let token = map
                .register(crate::types::StepTypeDef::of::<ProbeNote>(
                    "probe/note",
                    "probe",
                ))
                .expect("a fresh step type registers");
            assert_eq!((LEDGER_FORMAT_VERSION, envelope_fingerprint()), before);
            token.unregister();
            assert_eq!((LEDGER_FORMAT_VERSION, envelope_fingerprint()), before);
        }
    }
}
