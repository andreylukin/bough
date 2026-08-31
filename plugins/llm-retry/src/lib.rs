//! Invariant: retry lives HERE and nowhere else (P2-D5). This listener is the only thing in the
//! tree that turns a terminal model failure into another attempt, so `RequestErrorCall::attempt`
//! is a true count and the bound is a real bound.

pub mod invariant;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_llm::{AgentRequestError, FailureKind, Recovery, RequestErrorCall};

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
/// `None` ⇒ delegate. `Some(d)` ⇒ retry after `d`.
///
/// THREE gates, all of which must open: the adapter said the failure is retryable, the config
/// lists its kind, and attempts remain. `attempt` is 1-based and counts the attempt that just
/// FAILED, so `max_attempts: 1` never retries and the row can be neutered without unmounting it.
pub fn decide(cfg: &RetryConfig, call: &RequestErrorCall) -> Option<Duration> {
    if !call.failure.retryable {
        return None;
    }
    if !cfg.retry_on.contains(&call.failure.kind) {
        return None;
    }
    if call.attempt >= cfg.max_attempts {
        return None;
    }
    Some(backoff(cfg, call.attempt))
}

/// `backon`'s exponential ladder, evaluated for one attempt.
///
/// `ExponentialBuilder` is the source of the schedule (§13 names `backon` for exactly this), read
/// through its iterator rather than reimplemented, so "the delays this row produces" and "the
/// delays backon produces" cannot drift.
fn backoff(cfg: &RetryConfig, attempt: u32) -> Duration {
    use backon::BackoffBuilder;
    let mut builder = backon::ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(cfg.min_delay_ms))
        .with_max_delay(Duration::from_millis(cfg.max_delay_ms))
        .with_max_times(cfg.max_attempts.max(1) as usize);
    if cfg.jitter {
        builder = builder.with_jitter();
    }
    let mut it = builder.build();
    // The ladder is stateless per call, so the attempt index is walked to.
    let mut last = Duration::from_millis(cfg.min_delay_ms);
    for _ in 0..attempt.max(1) {
        match it.next() {
            Some(d) => last = d,
            None => break,
        }
    }
    last.min(Duration::from_millis(cfg.max_delay_ms))
}

/// Apply [`decide`] to the waterfall value. `true` ⇒ the listener returns WITHOUT calling `next`.
pub fn apply_decision(cfg: &RetryConfig, call: &mut RequestErrorCall) -> bool {
    match decide(cfg, call) {
        Some(after) => {
            // The SAME request is re-entered: the loop rebuilds it from the ledger, so a retry
            // cannot change what the model sees without a step saying so.
            call.recovery = Recovery::Retry { after };
            true
        }
        None => false,
    }
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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        ctx.on_waterfall::<AgentRequestError, _, _>(move |mut call: RequestErrorCall, next| {
            let cfg = cfg.clone();
            async move {
                if apply_decision(&cfg, &mut call) {
                    // §5: a listener that OWNS recovery returns without calling `next()`.
                    return call;
                }
                next.run(call).await
            }
        })
        .await?;
        Ok(())
    }
}

/// Marker so `Recovery` is named in this crate's public surface, where readers look for it.
pub use bough_plugin_llm::Recovery as RetryRecovery;

const _: fn() -> Recovery = || Recovery::Terminal;

bough_kernel::register_plugin!(RetryPlugin);
