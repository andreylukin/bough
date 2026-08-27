//! Invariant (§7): this row is an ordinary `actions` Provider — intent row, then the outward act
//! carrying the journal's derived marker, then `action/done`. It adds no write path of its own and
//! no second idempotency scheme; the journal's idem key is the only name an act has.
//!
//! CATALOG-ONLY (decision D-C8): in the binary, in no bundle, mounted by a test's own `--patch`.
//! It is also the second Provider the plugin audit's provider half swaps on the `actions` seam.

pub mod invariant;
pub mod provider;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::{ActionKind, Actions};

pub use crate::provider::GhShimProvider;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions-shim";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShimConfig {
    /// The binary invoked for GitHub kinds. `gh` — and a test puts a RECORDING SHIM first on PATH,
    /// because tests never call the real one (AGENTS.md).
    pub gh: String,
    /// Which of the four kinds this Provider claims. Default: all four.
    pub kinds: Vec<ActionKind>,
    /// A sleep INSIDE `execute`, before the outward call. The window a `kill -9` lands in for the
    /// "killed between the intent row and the outward act" half of V3.
    pub delay_before_ms: u64,
    /// A sleep after the outward call and before `action/done`. The other half of V3 — the one
    /// that would re-execute if reconciliation guessed.
    pub delay_after_ms: u64,
}

/// The row.
pub struct ActionsShimPlugin;

#[async_trait::async_trait]
impl Plugin for ActionsShimPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ShimConfig;

    fn inject() -> Inject {
        Inject::required(["actions"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.gh.trim().is_empty() {
            return Err(ConfigError::Rejected {
                detail: "gh: the binary to invoke must be named; there is no default".into(),
            });
        }
        if cfg.kinds.is_empty() {
            return Err(ConfigError::Rejected {
                detail: "kinds: a Provider that claims no kind provides nothing".into(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let actions = ctx
            .get::<Actions>()
            .map_err(|e| PluginError::new(entry, e))?;
        // Registration is an effect (§0.2): unloading this row makes its kinds stop existing.
        actions
            .provider(&ctx, Arc::new(GhShimProvider::new(cfg)))
            .await?;
        // The invocation record is this fiber's, so unloading it leaves no trace of the acts it
        // performed — the process-global record is the invariant's, and it forgets with the row.
        ctx.effect(|e| async move {
            e.defer_sync(crate::invariant::forget);
            Ok(())
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ActionsShimPlugin);
