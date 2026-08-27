//! Invariant (§11, §17 Phase 8): the timeline is a PURE function of the ledger stream. Every row
//! it shows is a step somebody appended; the order is total and deterministic; and the filters
//! compose as a conjunction of five independent dimensions, so narrowing one can never widen the
//! result.
//!
//! The pane is a CONSUMER of `ledger` (§0.2): no service key, no write path, no wake. Clicking a
//! row is a `FocusRequest` on that step, exactly as `tui-search` focuses a hit.
//!
//! SCAFFOLD: `allow(unused_variables)` covers the `todo!()` bodies and comes out with them.
#![allow(unused_variables)]

pub mod command;
pub mod error;
pub mod filter;
pub mod invariant;
pub mod order;
pub mod pane;
pub mod render;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{AgentName, Step, TrajId};

pub use crate::error::FilterError;
pub use crate::filter::{parse_filter, render_filter, Filter};
pub use crate::order::timeline;
pub use crate::pane::{TimelinePane, TimelineState};
pub use crate::render::{hit_of, line};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-timeline";

/// The pane id this row registers under. Fixed, because `/timeline` names it.
pub const PANE_ID: &str = "tui.timeline";

/// One row of the timeline: a step, and whose it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub agent: AgentName,
    pub traj: TrajId,
    pub step: Step,
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineConfig {
    pub height: u16,
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Newest steps read PER TRAJECTORY before filtering. The read bound.
    pub window: usize,
    /// Rows rendered after filtering. The render bound.
    pub limit: usize,
    pub debounce_ms: u64,
    /// `chrono` format for the time column.
    pub time_format: String,
}

/// The row.
pub struct TimelinePlugin;

#[async_trait::async_trait]
impl Plugin for TimelinePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = TimelineConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "ledger"]).union(&Inject::optional(["agents", "commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-2: reject height 0, window 0, limit 0, an unparseable time_format")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let _ = (ctx, cfg);
        todo!("WP-2: register the pane (Slot::Aux, order 20, Responsive) and /timeline")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TimelinePlugin);
