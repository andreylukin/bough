//! Invariant: this is the TEST ledger Provider (§3). It is a behavioural TWIN of `ledger-sqlite`,
//! not an approximation: same seq allocation under concurrency, same derived `step_refs` (the
//! Definition's function), same class and schema refusals, same unknown-type read rule, same fork
//! validation, same `connected`, same deterministic search ordering. Its bundle row is
//! `ledger-memory`, and it is in NO bundle: the swap patch names it.
//!
//! SCAFFOLD: `unused_variables` and `dead_code` are allowed while the bodies are `todo!()` and the
//! private state they thread has no reader yet. Both allows go away with the last `todo!()`.
#![allow(unused_variables, dead_code)]

pub mod invariant;
pub mod search;
pub mod store;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::*;

use crate::store::MemoryStore;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "ledger-memory";

/// No configuration at all — an empty struct, so the swap patch can write `config: {}`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {}

/// The provider plugin.
pub struct MemoryLedgerPlugin;

#[async_trait::async_trait]
impl Plugin for MemoryLedgerPlugin {
    const NAME: &'static str = "ledger-memory";
    type Config = MemoryConfig;

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-3: MemoryLedgerPlugin::apply — build the store, provide `ledger`, register the \
               ledger/step listener the invariants read, and defer the per-life forget"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(MemoryLedgerPlugin);

#[async_trait::async_trait]
impl LedgerStore for MemoryStore {
    fn provider(&self) -> &'static str {
        MemoryLedgerPlugin::NAME
    }
    fn format_version(&self) -> u32 {
        LEDGER_FORMAT_VERSION
    }

    fn register_step_type(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError> {
        todo!("WP-3: register_step_type")
    }
    fn step_types(&self) -> Vec<StepTypeDef> {
        todo!("WP-3: step_types")
    }
    fn skipped_ignorable(&self) -> u64 {
        todo!("WP-3: skipped_ignorable")
    }

    async fn append(&self, req: Append) -> Result<Step, LedgerError> {
        todo!("WP-3: append")
    }
    async fn append_batch(&self, reqs: Vec<Append>) -> Result<Vec<Step>, LedgerError> {
        todo!("WP-3: append_batch")
    }

    async fn step(&self, id: &StepId) -> Result<Option<Step>, LedgerError> {
        todo!("WP-3: step")
    }
    async fn steps(&self, q: &StepQuery) -> Result<Vec<Step>, LedgerError> {
        todo!("WP-3: steps")
    }
    async fn tail(&self, traj: &TrajId, n: usize) -> Result<Vec<Step>, LedgerError> {
        todo!("WP-3: tail")
    }
    async fn head_seq(&self, traj: &TrajId) -> Result<Option<Seq>, LedgerError> {
        todo!("WP-3: head_seq")
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
        crate::search::search(self, q)
    }
    async fn live_pins(&self, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError> {
        todo!("WP-3: live_pins")
    }
    async fn unconsumed_mail(&self, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
        todo!("WP-3: unconsumed_mail")
    }

    async fn add_edge(&self, e: Edge) -> Result<(), LedgerError> {
        todo!("WP-3: add_edge")
    }
    async fn edges(&self, traj: &TrajId) -> Result<Vec<Edge>, LedgerError> {
        todo!("WP-3: edges")
    }
    async fn ancestry(&self, traj: &TrajId) -> Result<Vec<TrajId>, LedgerError> {
        todo!("WP-3: ancestry")
    }
    async fn fork(&self, req: Fork) -> Result<ForkOutcome, LedgerError> {
        todo!("WP-3: fork")
    }
    async fn connected(&self, agent: &AgentName) -> Result<Connected, LedgerError> {
        todo!("WP-3: connected")
    }

    async fn seal_rollup(&self, r: NewRollup) -> Result<Rollup, LedgerError> {
        todo!("WP-3: seal_rollup")
    }
    async fn supersede_rollup(&self, old: &RollupId, new: &RollupId) -> Result<(), LedgerError> {
        todo!("WP-3: supersede_rollup")
    }
    async fn rollups(&self, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError> {
        todo!("WP-3: rollups")
    }

    async fn put_agent(&self, a: AgentRow) -> Result<(), LedgerError> {
        todo!("WP-3: put_agent")
    }
    async fn agent(&self, name: &AgentName) -> Result<Option<AgentRow>, LedgerError> {
        todo!("WP-3: agent")
    }
    async fn agents(&self) -> Result<Vec<AgentRow>, LedgerError> {
        todo!("WP-3: agents")
    }
    async fn delete_agent(&self, name: &AgentName) -> Result<(), LedgerError> {
        todo!("WP-3: delete_agent")
    }

    async fn action_intent(&self, a: NewAction) -> Result<ActionRow, LedgerError> {
        todo!("WP-3: action_intent")
    }
    async fn action_done(
        &self,
        id: &ActionId,
        status: ActionStatus,
        result: serde_json::Value,
    ) -> Result<(), LedgerError> {
        todo!("WP-3: action_done")
    }
    async fn actions(&self, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError> {
        todo!("WP-3: actions")
    }

    async fn row_hashes(&self, scope: HashScope) -> Result<Vec<RowHash>, LedgerError> {
        todo!("WP-3: row_hashes")
    }
    async fn trajectory_view(&self, traj: &TrajId) -> Result<TrajectoryView, LedgerError> {
        todo!("WP-3: trajectory_view")
    }
}
