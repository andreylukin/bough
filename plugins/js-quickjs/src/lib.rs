//! Invariant: this row is a `js` PROVIDER and nothing else. One `Runtime` per program, dropped
//! when the program ends; no module loader, no `std`/`os` bindings, no timers — the only
//! capabilities a program has are the `HostFn`s the seam handed it.

pub mod engine;
pub mod invariant;
pub mod preflight;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};

pub use engine::QuickJsEngine;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "js-quickjs";

/// The row's config. Both fields are real tunables, not protocol constants.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuickJsConfig {
    /// How often the interrupt handler samples the wall clock, in interrupt ticks. Too small
    /// burns time in the handler; too large loosens the wall-clock cap.
    pub interrupt_check_ops: u64,
    /// Programs that may run at once across the tree. A barrier, not a queue depth.
    pub max_concurrent_programs: usize,
}

/// The Provider row.
pub struct QuickJsPlugin;

#[async_trait::async_trait]
impl Plugin for QuickJsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = QuickJsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["js"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.interrupt_check_ops == 0 || cfg.max_concurrent_programs == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "interrupt_check_ops and max_concurrent_programs must be at least 1"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let js = ctx
            .get::<bough_plugin_js::Js>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // Installing the engine is an effect: unloading this row frees the seam's one slot, and
        // a SECOND engine row is a boot failure rather than a silent replacement.
        js.set_engine(&ctx, Arc::new(QuickJsEngine::new(cfg)))
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::no_runtime_outlives_its_program()]
    }
}

bough_kernel::register_plugin!(QuickJsPlugin);
