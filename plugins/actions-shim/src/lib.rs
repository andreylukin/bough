//! Invariant (§7): this row is an ordinary `actions` Provider — intent row, then the outward act
//! carrying the journal's derived marker, then `action/done`. It adds no write path of its own and
//! no second idempotency scheme; the journal's idem key is the only name an act has.
//!
//! CATALOG-ONLY (decision D-C8): in the binary, in no bundle, mounted by a test's own `--patch`.
//! It is also the second Provider the plugin audit's provider half swaps on the `actions` seam.
//!
//! SCAFFOLD: `allow(unused_variables)` covers the `todo!()` bodies and comes out with them.
#![allow(unused_variables)]

pub mod invariant;
pub mod provider;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::ActionKind;

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
        let _ = cfg;
        todo!("WP-4: reject an empty `gh`, and an empty `kinds` (a Provider claiming nothing)")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let _ = (ctx, cfg);
        todo!("WP-4: register GhShimProvider through ActionsHandle::provider (registration is an effect)")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ActionsShimPlugin);
