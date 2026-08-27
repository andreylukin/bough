//! Invariant (§11 "Digging", §5): what this pane renders is what the loop would send. It calls the
//! SAME `ctx.projection` the wake flow calls, with the same request defaults, and paints
//! `Assembled::to_text()` verbatim — the bytes are the loop's by construction, not by imitation.
//!
//! The pane is a CONSUMER (§0.2): it registers no service key, owns no write path, and adds no
//! second way to assemble a projection. Its only I/O is one `assemble` read per refresh.
//!
//! `PreviewAt::Seq` is byte-exact and is what V1 asserts; `PreviewAt::Head` states its delta on the
//! header rather than claiming an exactness today's seam cannot give it (decision D-C1).
//!
//! SCAFFOLD: `allow(unused_variables)` covers the `todo!()` bodies and comes out with them.
#![allow(unused_variables)]

pub mod command;
pub mod delta;
pub mod error;
pub mod invariant;
pub mod pane;
pub mod snapshot;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};

pub use crate::delta::{added_lines, only_preface, WAKE_PREFACE_KINDS};
pub use crate::error::PreviewError;
pub use crate::pane::{PreviewPane, PreviewState};
pub use crate::snapshot::{digest, snapshot, system_prefix, PreviewAt, Snapshot};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-preview";

/// The pane id this row registers under. Fixed, because `/preview` names it.
pub const PANE_ID: &str = "tui.preview";

/// The row's config. Every deployment-varying number is here; nothing is hardcoded in the pane.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewConfig {
    /// Rows the pane asks for when the Aux band has room.
    pub height: u16,
    /// Terminal ROWS below which this pane costs zero (`SlotSize::Responsive`'s `collapse`).
    pub collapse_rows: u16,
    pub min_rows: u16,
    pub max_rows: u16,
    /// Debounce on `ledger/step` before re-assembling. Assembly is deterministic but not free.
    pub refresh_ms: u64,
    /// Hard cap on rendered characters, so a 160k-token projection cannot stall a frame.
    pub max_chars: usize,
}

/// The row.
pub struct PreviewPlugin;

#[async_trait::async_trait]
impl Plugin for PreviewPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PreviewConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "projection", "ledger"])
            .union(&Inject::optional(["agents", "commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-1: reject height 0, max_chars 0, min_rows > max_rows")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let _ = (ctx, cfg);
        todo!("WP-1: register the pane (Slot::Aux, order 10, Responsive) and /preview")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PreviewPlugin);
