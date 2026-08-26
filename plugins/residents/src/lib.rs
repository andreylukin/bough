//! Invariant: at most ONE catch-up wake per agent per activation, and none at all for an agent
//! with nothing queued (§5, V6). TUI launch is the lid-open proxy (§13: there is no lid
//! notification on macOS; Phase 7's `sleep-listener` row will call the same method).
//!
//! The row holds every resumed agent's `AgentDisposer` inside its own effect, so disabling
//! `residents` by patch tears the roster down and leaves the ledger untouched (P3-D17).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::AgentName;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "residents";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentsConfig {
    /// Agent names to CREATE when the ledger has no row for them. Empty ⇒ create nothing.
    pub bootstrap: Vec<String>,
    /// Trajectory id prefix for a bootstrapped agent: `lane/` + name.
    pub traj_prefix: String,
    /// Resume every `agents` row at launch and hold its disposer.
    pub resume_all: bool,
    /// Run §5's catch-up wake once the roster is up.
    pub catch_up: bool,
}

/// PURE: which agents get a catch-up wake, given the roster and each one's unconsumed mail.
/// Empty for an agent with nothing queued — that is V6's "and none when nothing is queued".
pub fn catch_up_set(_roster: &[(AgentName, usize)]) -> Vec<AgentName> {
    todo!("WP-7")
}

/// The row.
pub struct ResidentsPlugin;

#[async_trait::async_trait]
impl Plugin for ResidentsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ResidentsConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7: wait for the factory slot, resume the roster as an effect, then catch up")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ResidentsPlugin);
