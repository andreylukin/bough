//! Invariant: this crate suppresses WAKES, never DELIVERIES. §5 says ordinary mail QUEUES for a
//! dormant agent, so `mail/delivered` still lands, the message still splices, and the consumed set
//! simply does not grow — the standing invariant then drains the backlog in ONE wake at
//! reactivation. Suppressing delivery instead would lose the backlog, and dormancy would stop
//! being reversible.
//!
//! The suppression point is `agent/wake-request` (P5-D1), dispatched by every loop Provider
//! immediately before it opens a wake: `agent/pre-step` is too late, because it rejects a claim
//! inside an already-durable wake.

pub mod admit;
pub mod command;
pub mod error;
pub mod fold;
pub mod invariant;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::{AgentName, Cite, StepId, WakeId};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

pub use admit::{admits, Decision, ReactivateCause};
pub use error::DormancyError;
pub use fold::dormant_from;
pub use vocabulary::{AgentDormancy, OWNER, STEP_TYPE};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "dormancy";

/// The `dormancy` service key.
pub struct Dormancy;

impl ServiceKey for Dormancy {
    type Value = DormancyHandle;
    const NAME: &'static str = "dormancy";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct DormancyHandle(pub Arc<DormancyInner>);

/// The seam's live state: the cache of the fold, and the seams it folds over.
pub struct DormancyInner {
    #[allow(dead_code)]
    ledger: bough_plugin_ledger::LedgerHandle,
    #[allow(dead_code)]
    agents: bough_plugin_agents::AgentsHandle,
    /// The CACHE. Never a database read on the admission path — admission runs before every wake.
    #[allow(dead_code)]
    dormant: parking_lot::Mutex<std::collections::BTreeSet<String>>,
}

/// A request to put a lane to sleep.
#[derive(Clone, Debug)]
pub struct SleepRequest {
    pub agent: AgentName,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

/// A request to wake a lane up.
#[derive(Clone, Debug)]
pub struct WakeUpRequest {
    pub agent: AgentName,
    pub cause: ReactivateCause,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

/// What one sleep or reactivation did.
#[derive(Clone, Debug, PartialEq)]
pub struct DormancyChange {
    pub agent: AgentName,
    pub dormant: bool,
    pub step: StepId,
    /// `Some` when reactivation armed the drain the standing invariant demands.
    pub drain: Option<WakeId>,
}

impl DormancyHandle {
    /// An empty cache over the two bound seams.
    pub fn new(
        _ledger: bough_plugin_ledger::LedgerHandle,
        _agents: bough_plugin_agents::AgentsHandle,
    ) -> DormancyHandle {
        todo!("WP-2: construct the inner with an empty dormant set")
    }

    /// The cache of the fold. Never a database read on the admission path.
    pub fn is_dormant(&self, _agent: &AgentName) -> bool {
        todo!("WP-2: read the cached set")
    }

    /// Every dormant lane, name-ordered.
    pub fn dormant(&self) -> Vec<AgentName> {
        todo!("WP-2: read the cached set")
    }

    /// Put a lane to sleep. Appends `agent/dormancy { dormant: true }`, citing what justified it.
    /// Sleeping an already-dormant lane is idempotent.
    pub async fn sleep(&self, _req: SleepRequest) -> Result<DormancyChange, DormancyError> {
        todo!("WP-2: append agent/dormancy and update the cache")
    }

    /// Wake a lane up. Appends `agent/dormancy { dormant: false }` and, if unconsumed ordinary
    /// mail exists, requests ONE drain wake — §5's standing invariant is what drains the backlog.
    pub async fn wake_up(&self, _req: WakeUpRequest) -> Result<DormancyChange, DormancyError> {
        todo!("WP-2: append agent/dormancy, update the cache, arm one drain wake")
    }

    /// Rebuild one agent's state from the ledger fold. Called at activation for every row.
    pub async fn reload(&self, _agent: &AgentName) -> Result<bool, DormancyError> {
        todo!("WP-2: one SeqDesc limit-1 query per agent, then cache")
    }
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DormancyConfig {
    /// Fold every existing `agents` row at activation, so a restart does not wake a sleeping lane.
    pub reload_at_activation: bool,
}

/// The `dormancy` row.
pub struct DormancyPlugin;

#[async_trait::async_trait]
impl Plugin for DormancyPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DormancyConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents"]).union(&Inject::optional(["commands"]))
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-2: declare the step type, provide `dormancy`, listen on agent/wake-request, \
               reload every row, register the commands"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::no_wake_while_dormant()]
    }
}

bough_kernel::register_plugin!(DormancyPlugin);
