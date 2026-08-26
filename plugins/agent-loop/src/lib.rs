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
pub mod testing;
pub mod transcript;
pub mod wake;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::Agents;
use bough_plugin_ledger::Ledger;
use bough_plugin_llm::Llm;
use bough_plugin_projection::Projection;
use bough_plugin_tools::Tools;

pub use driver::{LoopDriver, LoopFactory};
pub use wake::LoopDeps;

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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let err = |e: anyhow::Error| PluginError::new(entry.clone(), e);
        let ledger = ctx.get::<Ledger>().map_err(|e| err(e.into()))?;
        let projection = ctx.get::<Projection>().map_err(|e| err(e.into()))?;
        let llm = ctx.get::<Llm>().map_err(|e| err(e.into()))?;
        let tools = ctx.get::<Tools>().map_err(|e| err(e.into()))?;
        let agents = ctx.get::<Agents>().map_err(|e| err(e.into()))?;

        // Crash repair BEFORE the factory is published: an agent must never resume onto a
        // trajectory whose last wake is still open (§5).
        if cfg.repair_on_boot {
            repair::run(&ledger, chrono::Utc::now())
                .await
                .map_err(|e| err(anyhow::anyhow!(e)))?;
        }

        let deps = LoopDeps {
            ctx: ctx.clone(),
            ledger: (*ledger).clone(),
            projection: (*projection).clone(),
            llm: (*llm).clone(),
            tools: (*tools).clone(),
            // §5 makes the composition fingerprint part of `request/header`. A missing one is a
            // misconfiguration, and §0.2 says misconfiguration fails LOUD at the earliest
            // resolvable point — silently stamping "" on every header would have made the
            // fingerprint the header is required to carry quietly absent.
            composition: ctx
                .kernel()
                .and_then(|k| k.composition())
                .map(|c| c.fingerprint.as_str().to_string())
                .ok_or_else(|| {
                    err(anyhow::anyhow!(
                        "no composition fingerprint is resolvable from the kernel;                          `request/header` cannot carry the fingerprint §5 requires"
                    ))
                })?,
            cfg: cfg.clone(),
        };

        // The recorded requests are this fiber's: a reload starts clean, so the invariant never
        // reports a request a previous incarnation sent.
        let fiber = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(fiber));
            e.defer_sync(move || driver::forget(fiber));
            Ok(())
        })
        .await?;

        agents
            .set_factory(&ctx, Arc::new(LoopFactory::new(cfg, deps)))
            .await
            .map_err(|e| err(anyhow::anyhow!(e)))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        invariant::specs()
    }
}

bough_kernel::register_plugin!(AgentLoopPlugin);
