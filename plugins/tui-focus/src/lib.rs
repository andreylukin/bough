//! Invariant: NO STEP IS RENDERED TWICE. The live tail (what has streamed but not yet flushed to
//! `thought/text`) and the durable rows never overlap: the trailing step renders `live` whenever
//! `live.len() >= durable.len()` and the durable text otherwise (P3-D12), which makes the handover
//! flicker-free without any coordination between the `llm/stream` tee and the `ledger/step`
//! listener — two listeners that race by construction.
//!
//! This pane IS §11's `trajectory` pane (P3-D4): it owns the live tail AND the scrollback.

pub mod invariant;
pub mod rows;
pub mod scroll;
pub mod stream;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};

pub use rows::{rows_from_steps, Row};
pub use scroll::Scroll;
pub use stream::{trailing_text, LiveText};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-focus";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FocusConfig {
    /// Rows held in memory; older ones are paged from the ledger on demand.
    pub max_rows: usize,
    /// Fold marker past this many lines of one tool body.
    pub max_tool_lines: usize,
    pub page_lines: u16,
    pub expand_new_tools: bool,
    pub show_reasoning: bool,
}

/// The trajectory pane.
pub struct FocusPane {
    _private: (),
}

#[async_trait::async_trait]
impl Pane for FocusPane {
    fn render(&self, _cx: &mut RenderCx<'_>) {
        todo!("WP-4")
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        todo!("WP-4: scroll keys, wheel, and a click on a tool header toggles expansion")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        todo!("WP-4")
    }
}

/// The row.
pub struct FocusPlugin;

#[async_trait::async_trait]
impl Plugin for FocusPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = FocusConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "agents", "ledger", "llm"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4: register the pane, the ledger/step listener and the llm/stream tee")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(FocusPlugin);
