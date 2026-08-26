//! Invariant: this is a ledger PROVIDER (§0.2). It owns storage and nothing else: the vocabulary,
//! the rules and the conformance suite belong to `bough-plugin-ledger`, which this crate depends
//! on and which never depends back. Its bundle row is `ledger-sqlite`.
//!
//! SCAFFOLD: `unused_variables` and `dead_code` are allowed while the bodies are `todo!()` and the
//! private state they thread has no reader yet. Both allows go away with the last `todo!()`.
#![allow(unused_variables, dead_code)]

pub mod append;
pub mod connected;
pub mod fork;
pub mod invariant;
pub mod read;
pub mod schema;
pub mod search;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::*;

use crate::store::SqliteStore;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "ledger-sqlite";

/// The row's config (§0.5: validated purely, no I/O in `validate`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqliteConfig {
    /// The db file. `":memory:"` is allowed and skips WAL.
    pub path: PathBuf,
    /// How long a writer waits on a locked db before giving up.
    #[serde(default = "default_busy_timeout")]
    pub busy_timeout_ms: u64,
}

fn default_busy_timeout() -> u64 {
    5000
}

/// The provider plugin.
pub struct SqliteLedgerPlugin;

#[async_trait::async_trait]
impl Plugin for SqliteLedgerPlugin {
    const NAME: &'static str = "ledger-sqlite";
    type Config = SqliteConfig;

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        todo!("WP-2: SqliteLedgerPlugin::validate")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-2: SqliteLedgerPlugin::apply — open the store, provide `ledger`, register the \
               ledger/step listener the invariants read, and defer the per-life forget"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SqliteLedgerPlugin);

#[async_trait::async_trait]
impl LedgerStore for SqliteStore {
    fn provider(&self) -> &'static str {
        SqliteLedgerPlugin::NAME
    }
    fn format_version(&self) -> u32 {
        LEDGER_FORMAT_VERSION
    }

    fn register_step_type(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError> {
        todo!("WP-2: register_step_type")
    }
    fn step_types(&self) -> Vec<StepTypeDef> {
        todo!("WP-2: step_types")
    }
    fn skipped_ignorable(&self) -> u64 {
        todo!("WP-2: skipped_ignorable")
    }

    async fn append(&self, req: Append) -> Result<Step, LedgerError> {
        crate::append::append(self, req).await
    }
    async fn append_batch(&self, reqs: Vec<Append>) -> Result<Vec<Step>, LedgerError> {
        crate::append::append_batch(self, reqs).await
    }

    async fn step(&self, id: &StepId) -> Result<Option<Step>, LedgerError> {
        todo!("WP-2: step")
    }
    async fn steps(&self, q: &StepQuery) -> Result<Vec<Step>, LedgerError> {
        crate::read::steps(self, q).await
    }
    async fn tail(&self, traj: &TrajId, n: usize) -> Result<Vec<Step>, LedgerError> {
        todo!("WP-2: tail")
    }
    async fn head_seq(&self, traj: &TrajId) -> Result<Option<Seq>, LedgerError> {
        todo!("WP-2: head_seq")
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
        crate::search::search(self, q).await
    }
    async fn live_pins(&self, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError> {
        crate::read::live_pins(self, trajs).await
    }
    async fn unconsumed_mail(&self, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
        crate::read::unconsumed_mail(self, traj).await
    }

    async fn add_edge(&self, e: Edge) -> Result<(), LedgerError> {
        todo!("WP-2: add_edge")
    }
    async fn edges(&self, traj: &TrajId) -> Result<Vec<Edge>, LedgerError> {
        todo!("WP-2: edges")
    }
    async fn ancestry(&self, traj: &TrajId) -> Result<Vec<TrajId>, LedgerError> {
        todo!("WP-2: ancestry")
    }
    async fn fork(&self, req: Fork) -> Result<ForkOutcome, LedgerError> {
        crate::fork::fork(self, req).await
    }
    async fn connected(&self, agent: &AgentName) -> Result<Connected, LedgerError> {
        crate::connected::connected(self, agent).await
    }

    async fn seal_rollup(&self, r: NewRollup) -> Result<Rollup, LedgerError> {
        todo!("WP-2: seal_rollup")
    }
    async fn supersede_rollup(&self, old: &RollupId, new: &RollupId) -> Result<(), LedgerError> {
        todo!("WP-2: supersede_rollup")
    }
    async fn rollups(&self, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError> {
        crate::read::rollups(self, q).await
    }

    async fn put_agent(&self, a: AgentRow) -> Result<(), LedgerError> {
        todo!("WP-2: put_agent")
    }
    async fn agent(&self, name: &AgentName) -> Result<Option<AgentRow>, LedgerError> {
        todo!("WP-2: agent")
    }
    async fn agents(&self) -> Result<Vec<AgentRow>, LedgerError> {
        todo!("WP-2: agents")
    }
    async fn delete_agent(&self, name: &AgentName) -> Result<(), LedgerError> {
        todo!("WP-2: delete_agent")
    }

    async fn action_intent(&self, a: NewAction) -> Result<ActionRow, LedgerError> {
        todo!("WP-2: action_intent")
    }
    async fn action_done(
        &self,
        id: &ActionId,
        status: ActionStatus,
        result: serde_json::Value,
    ) -> Result<(), LedgerError> {
        todo!("WP-2: action_done")
    }
    async fn actions(&self, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError> {
        todo!("WP-2: actions")
    }

    async fn row_hashes(&self, scope: HashScope) -> Result<Vec<RowHash>, LedgerError> {
        crate::read::row_hashes(self, scope).await
    }
    async fn trajectory_view(&self, traj: &TrajId) -> Result<TrajectoryView, LedgerError> {
        crate::read::trajectory_view(self, traj).await
    }
}
