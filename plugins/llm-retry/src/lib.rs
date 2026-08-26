//! Invariant: retry lives HERE and nowhere else (P2-D5). This listener is the only thing in the
//! tree that turns a terminal model failure into another attempt, so `RequestErrorCall::attempt`
//! is a true count and the bound is a real bound.

pub mod invariant;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_llm::{FailureKind, Recovery, RequestErrorCall};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "llm-retry";

/// The row's config. Every value here varies by deployment, which is why none of it is a `const`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
    /// Total attempts including the first. `1` disables retrying without unmounting the row.
    pub max_attempts: u32,
    pub min_delay_ms: u64,
    pub max_delay_ms: u64,
    #[serde(default)]
    pub jitter: bool,
    /// Which failure kinds are eligible. A kind not listed always delegates.
    pub retry_on: Vec<FailureKind>,
}

/// The decision, as a pure function of the config and the failure: what the listener does, with
/// no clock, no I/O and no waterfall in the way.
///
/// WP-1.
pub fn decide(_cfg: &RetryConfig, _call: &RequestErrorCall) -> Option<Duration> {
    todo!("WP-1: backon's backoff, bounded by max_attempts and gated on retry_on + retryable")
}

/// Apply [`decide`] to the waterfall value. `Some` ⇒ the listener returns WITHOUT calling `next`.
///
/// WP-1.
pub fn apply_decision(_cfg: &RetryConfig, _call: &mut RequestErrorCall) -> bool {
    todo!("WP-1: set Recovery::Retry and report whether the chain was short-circuited")
}

/// The consumer row.
pub struct RetryPlugin;

#[async_trait::async_trait]
impl Plugin for RetryPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = RetryConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["llm"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.min_delay_ms > cfg.max_delay_ms {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: format!(
                    "min_delay_ms ({}) exceeds max_delay_ms ({})",
                    cfg.min_delay_ms, cfg.max_delay_ms
                ),
            });
        }
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: on_waterfall::<AgentRequestError> — Retry without next(), else delegate")
    }
}

/// Marker so `Recovery` is named in this crate's public surface, where readers look for it.
pub use bough_plugin_llm::Recovery as RetryRecovery;

const _: fn() -> Recovery = || Recovery::Terminal;

bough_kernel::register_plugin!(RetryPlugin);
