//! Invariant: the rail reads `about/line` by step-type NAME out of the ledger; it does NOT depend
//! on `bough-plugin-about-line` (P3-D11). A pane depending on a Consumer crate would invert the
//! seam rule, and the merge-extensible step-type map (§3) exists precisely so a renderer can read
//! a type it does not own. With `about-line` disabled the strip renders the glyph and no
//! about-lines.
//!
//! The intent half is ALWAYS rendered under its label, never as truth (§2).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::Status;
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-strip";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StripConfig {
    pub width: u16,
    pub show_about: bool,
    pub about_lines: u16,
    /// Refresh cadence for counters the ledger owns (unconsumed mail).
    pub refresh_ms: u64,
}

/// PURE, unit-tested: status + pending wake ⇒ glyph and style role.
pub fn glyph(_status: Status, _wake_pending: bool, _disposed: bool) -> (char, &'static str) {
    todo!("WP-4")
}

/// Re-exported from the render library, which owns it because both panes read it (§1).
pub use bough_plugin_tui_render::{about_from_step, AboutView};

/// The rail itself.
pub struct StripPane {
    _private: (),
}

#[async_trait::async_trait]
impl Pane for StripPane {
    fn render(&self, _cx: &mut RenderCx<'_>) {
        todo!("WP-4")
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        todo!("WP-4: a click on a rail row returns Focus(agent)")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        todo!("WP-4")
    }
}

/// The row.
pub struct StripPlugin;

#[async_trait::async_trait]
impl Plugin for StripPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = StripConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "agents", "ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4: register the pane, and the four listeners that keep it current")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(StripPlugin);
