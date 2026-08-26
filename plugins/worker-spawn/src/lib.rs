//! Invariant (§10): a worker gets a FRESH TASK-ONLY CONTEXT. Its agent is created through the
//! agent factory on its own trajectory, seeded with exactly the write-boundary block and the
//! task, with `tools.restrict` applied in its own scope — and with NO projection of the
//! spawner's history. What comes back is the report, sealed; what lands in the spawner's chain is
//! cited evidence plus one thought per uncited claim.

pub mod boundary;
pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_workers::{
    AskMode, StartWorker, WorkerError, WorkerKind, WorkerProvider, WorkerResult, WorkerRun,
};

pub use boundary::WRITE_BOUNDARY;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "worker-spawn";

/// The row's config. The boundary block is NOT here (P2-D21).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnConfig {
    /// What `ask()` does to the worker when the spawner does not say.
    pub ask_mode: AskMode,
    /// How many steps one worker may spend before it is ended with a failure.
    pub max_steps: u32,
}

/// The provider.
pub struct SpawnProvider {
    _cfg: Arc<SpawnConfig>,
}

impl SpawnProvider {
    /// WP-6.
    pub fn new(cfg: Arc<SpawnConfig>) -> SpawnProvider {
        SpawnProvider { _cfg: cfg }
    }

    /// The seeded task: the boundary block FIRST, then the task. Pure, so the test can assert on
    /// it without a runtime — and the roundtrip test still asserts on the recorded request.
    ///
    /// WP-6.
    pub fn seed_task(_task: &str) -> String {
        todo!("WP-6: WRITE_BOUNDARY, then the task")
    }
}

#[async_trait::async_trait]
impl WorkerProvider for SpawnProvider {
    fn kinds(&self) -> Vec<WorkerKind> {
        vec![WorkerKind::Spawn]
    }

    /// WP-6: create a task-only `AgentKind::Worker` agent through the factory, run it to its
    /// report, validate against the seal, and land the result in the SPAWNER's chain.
    async fn start(
        &self,
        _req: Arc<StartWorker>,
        _run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError> {
        todo!("WP-6: the spawn roundtrip")
    }
}

/// The provider row.
pub struct SpawnPlugin;

#[async_trait::async_trait]
impl Plugin for SpawnPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SpawnConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["workers", "agents", "ledger", "tools"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: workers.provider(ctx, SpawnProvider::new(cfg))")
    }
}

bough_kernel::register_plugin!(SpawnPlugin);
