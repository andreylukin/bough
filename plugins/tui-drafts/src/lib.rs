//! Invariant: THIS PANE OFFERS NO SEND. Its key hints are `↑/↓ select`, `enter expand`, `y copy`,
//! and there is no code path from a key it handles to `ctx.actions`, to a network, or to anything
//! that could deliver a draft. A test asserts on the key hints AND on the rendered buffer, because
//! the absence is the whole point (§7, V4).
//!
//! It registers into `tui-shell`'s `Aux` slot as an EFFECT, listens on `ledger/step` for the two
//! draft kinds, and re-reads through `DraftsHandle::list`.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_drafts::{DraftRow, DraftsHandle};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-drafts";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftsPaneConfig {
    pub height_pct: u16,
    pub limit: usize,
    pub show_body_lines: usize,
}

/// The pane's own state: the rows it last read, and which is selected.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct DraftsPane {
    cfg: Arc<DraftsPaneConfig>,
    drafts: DraftsHandle,
    rows: parking_lot::Mutex<Vec<DraftRow>>,
    selected: parking_lot::Mutex<usize>,
    expanded: parking_lot::Mutex<bool>,
}

impl DraftsPane {
    /// An empty pane over one drafts handle. WP-4.
    pub fn new(cfg: Arc<DraftsPaneConfig>, drafts: DraftsHandle) -> Arc<DraftsPane> {
        let _ = (cfg, drafts);
        todo!("WP-4")
    }

    /// Re-read from `DraftsHandle::list`. Called from `handle`, never from `render`. WP-4.
    pub async fn refresh(&self) {
        todo!("WP-4")
    }
}

#[async_trait::async_trait]
impl Pane for DraftsPane {
    /// SYNCHRONOUS and non-blocking: renders from `rows`, which `handle` filled. WP-4.
    fn render(&self, cx: &mut RenderCx<'_>) {
        let _ = cx;
        todo!("WP-4")
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        let _ = (ev, cx);
        todo!("WP-4: ↑/↓ select, enter expand, y copy. NOTHING sends.")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("↑/↓", "select"), ("enter", "expand"), ("y", "copy")]
    }
}

/// The row.
pub struct DraftsPanePlugin;

#[async_trait::async_trait]
impl Plugin for DraftsPanePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DraftsPaneConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tui", "drafts", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-4: `height_pct` in 1..=100, `limit > 0`")
    }

    /// Register the pane in `Slot::Aux`, `SlotSize::Percent(height_pct)`, `focusable: true`, as an
    /// effect; then subscribe to `ledger/step`. WP-4.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(DraftsPanePlugin);
