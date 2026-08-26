//! Invariant: this crate is the workers SERVICE DEFINITION (§10). It owns the `workers` key, the
//! start/result vocabulary, the live-run table, the provider registry and the THREE BOUNDS — and
//! no spawning. The bounds are checked HERE so every provider obeys the same numbers (§7), and a
//! provider that wanted its own would have to lie about the seam.
//!
//! P2-D1: it owns live state (the run table), so it IS a catalog row and provides its own key.

pub mod error;
pub mod ids;
pub mod invariant;
pub mod run;
pub mod seal;
pub mod start;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey};

pub use error::WorkerError;
pub use ids::WorkerId;
pub use run::{AskAnswer, WorkerRun};
pub use seal::{Report, ReportClaim, SealSpec};
pub use start::{AskMode, Bounds, StartWorker, WorkerKind, WorkerOutcome, WorkerResult};
pub use vocabulary::{WorkerClaim, WorkerReport, WorkerStarted};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "workers";

/// The `workers` service key.
pub struct Workers;

impl ServiceKey for Workers {
    type Value = WorkersHandle;
    const NAME: &'static str = "workers";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct WorkersHandle(pub Arc<WorkersInner>);

/// The seam's live state: the run table, the provider registry and the bounds.
pub struct WorkersInner {
    /// WP-6 fills these in.
    _runs: parking_lot::Mutex<Vec<WorkerRun>>,
    _providers: parking_lot::Mutex<Vec<Arc<dyn WorkerProvider>>>,
    _bounds: Bounds,
}

/// What a worker Provider does.
#[async_trait::async_trait]
pub trait WorkerProvider: Send + Sync + 'static {
    fn kinds(&self) -> Vec<WorkerKind>;
    async fn start(
        &self,
        req: Arc<StartWorker>,
        run: WorkerRun,
    ) -> Result<WorkerResult, WorkerError>;
}

impl WorkersHandle {
    /// An empty seam with the row's bounds. WP-6.
    pub fn new(bounds: Bounds) -> WorkersHandle {
        WorkersHandle(Arc::new(WorkersInner {
            _runs: parking_lot::Mutex::new(Vec::new()),
            _providers: parking_lot::Mutex::new(Vec::new()),
            _bounds: bounds,
        }))
    }

    /// Register a Provider. Registration is an effect (§0.2). WP-6.
    pub async fn provider(
        &self,
        _ctx: &Context,
        _p: Arc<dyn WorkerProvider>,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-6: register, with the inverse that removes it")
    }

    /// Bounds are checked HERE, in the Definition, so every provider obeys the same numbers (§7).
    /// A kind no Provider registered is [`WorkerError::NoProvider`].
    ///
    /// WP-6.
    pub async fn start(
        &self,
        _ctx: &Context,
        _req: StartWorker,
    ) -> Result<WorkerResult, WorkerError> {
        todo!("WP-6: bounds, then the provider, then the spawner's chain")
    }

    /// The live runs. WP-6.
    pub fn live(&self) -> Vec<WorkerRun> {
        todo!("WP-6")
    }
    /// The configured bounds. WP-6.
    pub fn bounds(&self) -> Bounds {
        todo!("WP-6")
    }
    /// How many runs are in flight right now. WP-6.
    pub fn in_flight(&self) -> usize {
        todo!("WP-6")
    }
}

/// The row's config: the three bounds of §7.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkersConfig {
    /// Default 8.
    pub max_in_flight: usize,
    /// Default 3.
    pub max_depth: u8,
    pub per_wake_spawn_cap: usize,
}

/// The Service Definition row.
pub struct WorkersPlugin;

#[async_trait::async_trait]
impl Plugin for WorkersPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = WorkersConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_in_flight == 0 || cfg.max_depth == 0 || cfg.per_wake_spawn_cap == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "every worker bound must be at least 1; unmount the row to disable workers"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: declare the three step types, provide::<Workers>, record the invariant stream")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::runs_stay_within_bounds()]
    }
}

bough_kernel::register_plugin!(WorkersPlugin);
