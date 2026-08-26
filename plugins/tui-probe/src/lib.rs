//! Invariant: this is a TEST INSTRUMENT, not a product row. It is in the catalog and in NO bundle;
//! the tests' and `scripts/tui/`'s own `--patch` mounts it. It exists so V8 can prove the two
//! things a well-behaved TUI must do and no product row can be asked to demonstrate: PANIC inside
//! a pane's render, and NEVER ACTIVATE.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};

/// The catalog name of the pane row.
pub const PLUGIN_NAME: &str = "tui-probe";
/// The catalog name of the row that can never activate.
pub const NEVER_PLUGIN_NAME: &str = "tui-never";

/// The probe pane's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    /// The deterministic string this pane renders, so a script has something to assert on.
    pub text: String,
    /// The key that makes `render` panic, e.g. `"p"`. Empty ⇒ never panics.
    pub panic_key: String,
}

/// A deterministic fixture pane that panics on demand.
pub struct ProbePane {
    _private: (),
}

#[async_trait::async_trait]
impl Pane for ProbePane {
    fn render(&self, _cx: &mut RenderCx<'_>) {
        todo!("WP-7")
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        todo!("WP-7: the configured key arms the panic for the next render")
    }
}

/// The pane row.
pub struct TuiProbePlugin;

#[async_trait::async_trait]
impl Plugin for TuiProbePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ProbeConfig;

    fn inject() -> Inject {
        Inject::required(["tui"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// The row that can NEVER activate: it declares an injection nobody provides, so §0.2's "an
/// enabled row that never activates is a boot failure" has a deliberate vehicle (V8).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NeverConfig {}

/// The never-activating row.
pub struct TuiNeverPlugin;

#[async_trait::async_trait]
impl Plugin for TuiNeverPlugin {
    const NAME: &'static str = NEVER_PLUGIN_NAME;
    type Config = NeverConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "a_key_nobody_provides"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        unreachable!("`tui.never` exists to stay PENDING; reaching `apply` is the bug it hunts")
    }
}

bough_kernel::register_plugin!(TuiProbePlugin);
bough_kernel::register_plugin!(TuiNeverPlugin);
