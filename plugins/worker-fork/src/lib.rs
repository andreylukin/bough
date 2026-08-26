//! Invariant (§2): there is ONE loop. This crate drives no loop of its own — a private one-shot
//! loop inside a worker Provider would put loop code in a second crate. It forks the ledger,
//! assembles the PARENT at the fork seq, pins that prefix on the child, and hands the child to the
//! ordinary agent machinery; the parent's message history reaches it through `transcript::rebuild`
//! over the forked chain, which is the other half of "keeps the parent's history" (§10).

pub mod invariant;
pub mod point;
pub mod prefix;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_workers::{
    StartWorker, WorkerError, WorkerKind, WorkerProvider, WorkerResult, WorkerRun,
};

pub use point::fork_point;
pub use vocabulary::{ForkPrefix, OWNER};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "worker-fork";

/// The provider for [`WorkerKind::Fork`].
pub struct ForkProvider {
    #[allow(dead_code)]
    ledger: bough_plugin_ledger::LedgerHandle,
    #[allow(dead_code)]
    agents: bough_plugin_agents::AgentsHandle,
    #[allow(dead_code)]
    projection: bough_plugin_projection::ProjectionHandle,
    #[allow(dead_code)]
    cfg: Arc<ForkConfig>,
}

#[async_trait::async_trait]
impl WorkerProvider for ForkProvider {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Fork]
    }

    /// In order: [`fork_point`] → `ledger.fork(parent → worker-fork-<id>)` → assemble the PARENT
    /// at that seq → `agents.create(CreateAgent { kind: Fork, traj: <the forked child>, setup })`
    /// where `setup` pins the prefix, appends `fork/prefix`, and registers the report tool and the
    /// step budget exactly as `worker-spawn` does. The seed message is the task.
    async fn start(
        &self,
        _req: Arc<StartWorker>,
        _run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        todo!("WP-6: fork, assemble the parent, pin, create the one-shot child")
    }
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForkConfig {
    /// The step budget one fork child gets. It counts against the SAME spawn bounds as a
    /// `worker-spawn` child: a fork is not a way around the bound.
    pub max_steps: u32,
}

/// The `worker.fork` row.
pub struct WorkerForkPlugin;

#[async_trait::async_trait]
impl Plugin for WorkerForkPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ForkConfig;

    fn inject() -> Inject {
        Inject::required(["workers", "agents", "ledger", "projection", "tools"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: declare fork/prefix, register the ForkProvider on `workers`")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::pinned_prefix_reconstructs()]
    }
}

bough_kernel::register_plugin!(WorkerForkPlugin);
