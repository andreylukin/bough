//! Invariant: EXACTLY ONE catch-up wake per active agent per wake. `Agent::request_wake` already
//! returns `Nothing` when there is nothing queued, so "only over queued mail" falls out of the
//! seam; the half the seam does not give is the second `DidWake` arriving while a catch-up is still
//! in flight, and an `in_flight` set drops it here.
//!
//! A `DidWake` whose `asleep_for` is under `min_sleep_ms` produces none: a lid closed for ten
//! seconds is not a night away.

pub mod invariant;

use std::collections::HashSet;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{AgentId, AgentsHandle};
use bough_plugin_power::PowerEvent;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "catch-up-on-wake";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatchUpOnWakeConfig {
    pub min_sleep_ms: u64,
    /// Which agent kinds get a catch-up wake. `["resident"]`.
    pub kinds: Vec<String>,
}

/// The consumer's state: who is mid-catch-up.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct CatchUpOnWake {
    cfg: Arc<CatchUpOnWakeConfig>,
    agents: AgentsHandle,
    in_flight: parking_lot::Mutex<HashSet<AgentId>>,
}

impl CatchUpOnWake {
    /// One `DidWake`: request a wake per eligible agent, skipping those already in flight.
    /// Returns whom it woke, so the test asserts on the set rather than on a count. WP-8.
    pub async fn on_wake(&self, ev: &PowerEvent) -> Vec<AgentId> {
        let _ = ev;
        todo!("WP-8")
    }
}

/// The row.
pub struct CatchUpOnWakePlugin;

#[async_trait::async_trait]
impl Plugin for CatchUpOnWakePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CatchUpOnWakeConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["power", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-8: non-empty `kinds`")
    }

    /// `on_parallel::<PowerChanged>` as an effect. WP-8.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-8")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(CatchUpOnWakePlugin);
