//! Invariant: this is the ONLY crate in the phase with concrete loop code, and it holds §5's wake
//! flow exactly as drawn. Everything a deployment might want to change about a wake is a plugin
//! on one of the waterfalls, never a branch in here — and there is deliberately NO wake budget
//! field: §5 says bounding a runaway wake is a plugin cancelling from `agent/wake-stopping`, and
//! a `max_steps` here would be exactly the hardcoded tunable §0.2 forbids.

pub mod driver;
pub mod invariant;
pub mod mail;
pub mod preempt;
pub mod repair;
pub mod request;
pub mod scope;
pub mod transcript;
pub mod wake;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};

pub use driver::{LoopDriver, LoopFactory};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "agent-loop";

/// The row's config. Every field varies by deployment; none of them is a protocol constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopConfig {
    /// How long ordinary mail coalesces before a drain wake runs.
    pub drain_debounce_ms: u64,
    /// The one grace step a preempted wake gets to jot.
    pub grace_deadline_ms: u64,
    pub default_max_tokens: i64,
    /// Stamped into every `request/header`.
    pub prompt_ver: String,
    /// How often streamed text is flushed into a `thought/text` step.
    pub text_flush_ms: u64,
    /// Run crash repair at `apply`.
    pub repair_on_boot: bool,
    /// How long `stop()` waits for a wake to drain before it gives up on being graceful.
    pub status_drain_ms: u64,
}

/// The Provider row: it takes the `agents` factory slot.
pub struct AgentLoopPlugin;

#[async_trait::async_trait]
impl Plugin for AgentLoopPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LoopConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["agents", "ledger", "projection", "llm", "tools"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4: crash repair (if configured), then agents.set_factory(LoopFactory::new(cfg))")
    }

    fn invariants() -> Vec<InvariantSpec> {
        invariant::specs()
    }
}

bough_kernel::register_plugin!(AgentLoopPlugin);
