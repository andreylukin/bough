//! Invariant (§8): the dashboard is a VIEW of `drift-watch`'s per-agent signals and nothing more.
//! It computes no signal of its own, writes nothing, and reaches §8's one-command reset by
//! dispatching `drift-watch`'s own `/reset` through the `commands` seam (decision D-C3), so there
//! stays exactly one implementation of a reset.
//!
//! `TooFewSamples` is a verdict of its own: this pane never turns thin evidence into a `Steady`
//! glyph (§16).
//!
//! SCAFFOLD: `allow(unused_variables)` covers the `todo!()` bodies and comes out with them.
#![allow(unused_variables)]

pub mod command;
pub mod dash;
pub mod invariant;
pub mod pane;
pub mod render;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};

pub use crate::dash::{arm, dash_row, reset_command, verdict, DashRow, ResetStep, Verdict};
pub use crate::pane::{DriftPane, DriftState};
pub use crate::render::{bar, line};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-drift";

/// The pane id this row registers under. Fixed, because `/driftboard` names it.
pub const PANE_ID: &str = "tui.drift";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftPaneConfig {
    pub height: u16,
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Most agents shown; the rest are a `… N more` line.
    pub agents_shown: usize,
    pub refresh_ms: u64,
    /// Columns the tool-share bar gets.
    pub bar_cols: u16,
    /// Milliseconds the reset stays armed after the first `r` (decision D-C5).
    pub arm_ms: u64,
}

/// The row.
pub struct DriftBoardPlugin;

#[async_trait::async_trait]
impl Plugin for DriftBoardPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DriftPaneConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "drift"]).union(&Inject::optional(["agents", "commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-3: reject height 0, agents_shown 0, bar_cols 0, arm_ms 0")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let _ = (ctx, cfg);
        todo!("WP-3: register the pane (Slot::Aux, order 30, Responsive) and /driftboard")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(DriftBoardPlugin);
