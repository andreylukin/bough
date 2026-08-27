//! Invariant: this row OWNS the status line and nothing else. It reads `ctx.ledger` and the
//! shell's handle, assembles a [`StatusView`], and draws one row — it never steers an agent and
//! never writes a step. Disabling the row by patch removes the line and reflows the layout, which
//! is the phase's SWAP gate (phase ux1 §2.5, §17).

pub mod invariant;
pub mod status;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};

pub use status::{elide_path, fields, status_line, Field, StatusView};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-status";
/// The pane id this row registers in [`bough_plugin_tui_shell::Slot::Status`].
pub const PANE_ID: &str = "tui.status";

/// The row's config. Every deployment-varying value is here; nothing is a `DEFAULT_` constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusConfig {
    /// Longest cwd rendered before the middle is elided.
    pub cwd_max: u16,
    /// Spinner frames, as one string. Deployment-varying (a terminal without a good font).
    pub spinner: String,
    pub spinner_ms: u64,
    /// Key hints, in order, as `"key=meaning"` pairs. The hint list is config, not a constant,
    /// because it is the one chrome a user might want shortened.
    pub hints: Vec<String>,
}

/// The status pane.
pub struct StatusPane {
    #[allow(dead_code)]
    cfg: Arc<StatusConfig>,
    #[allow(dead_code)]
    view: parking_lot::Mutex<StatusView>,
}

impl StatusPane {
    /// A pane over an empty view. Public so a test can drive it without a composed tree.
    pub fn new(cfg: Arc<StatusConfig>) -> StatusPane {
        StatusPane {
            cfg,
            view: parking_lot::Mutex::new(StatusView::default()),
        }
    }

    /// The view the line would draw right now.
    pub fn view(&self) -> StatusView {
        self.view.lock().clone()
    }
}

/// The row.
pub struct TuiStatusPlugin;

#[async_trait::async_trait]
impl Plugin for TuiStatusPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = StatusConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "ledger"]).union(&Inject::optional(["agents", "workspace"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-4: reject a zero cwd_max, an empty spinner, a zero spinner_ms, a malformed hint")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let _ = (ctx, cfg);
        todo!("WP-4: register the pane in Slot::Status and mount the listeners that fill StatusView")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TuiStatusPlugin);
