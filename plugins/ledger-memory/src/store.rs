//! Invariant: append-only is STRUCTURAL here — there is no mutation method to call. One write
//! lock allocates seq exactly as sqlite's transaction does, `step_refs` come from the
//! Definition's `derive_step_refs` (never a re-implementation), and `agents` is the one mutable
//! map. Everything is dropped when the fiber unloads: no persistence, no file, no config.

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_ledger::{
    ActionId, ActionRow, AgentName, AgentRow, Edge, LedgerError, Rollup, RollupId, Seq, Step,
    StepId, StepTypeMap, TrajId,
};
use parking_lot::RwLock;

/// Everything the memory provider holds, behind one lock — the lock IS the single writer.
#[derive(Default)]
pub struct Inner {
    /// Steps by trajectory, in seq order.
    pub steps: BTreeMap<TrajId, Vec<Step>>,
    /// Every step by id, for the point lookup.
    pub by_id: BTreeMap<StepId, (TrajId, Seq)>,
    pub edges: Vec<Edge>,
    pub rollups: BTreeMap<RollupId, Rollup>,
    pub actions: BTreeMap<ActionId, ActionRow>,
    /// The one mutable map (§3 exempts `agents` from append-only).
    pub agents: BTreeMap<AgentName, AgentRow>,
}

/// The store behind the `ledger` binding.
pub struct MemoryStore {
    pub(crate) inner: RwLock<Inner>,
    pub(crate) types: Arc<StepTypeMap>,
    /// The provider's captured context: `ledger/step` is emitted from it, post-commit.
    pub(crate) ctx: Context,
    pub(crate) skipped: Arc<std::sync::atomic::AtomicU64>,
}

impl MemoryStore {
    /// An empty store with the sixteen builtin step types installed.
    pub fn new(ctx: Context) -> Arc<MemoryStore> {
        todo!("WP-3: MemoryStore::new")
    }

    /// Validate, take the write lock, allocate `MAX(seq)+1`, insert. The sqlite transaction's twin.
    pub(crate) fn commit(
        &self,
        reqs: Vec<bough_plugin_ledger::Append>,
    ) -> Result<Vec<Step>, LedgerError> {
        todo!("WP-3: MemoryStore::commit")
    }
}
