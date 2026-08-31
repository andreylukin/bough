//! Invariant: this is a TEST INSTRUMENT, not a product row. It is in the catalog and in NO bundle;
//! the tests' and `scripts/tui/`'s own `--patch` mounts it. It exists so V8 can prove the two
//! things a well-behaved TUI must do and no product row can be asked to demonstrate: PANIC inside
//! a pane's render, and NEVER ACTIVATE.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneId, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::Tui;
use parking_lot::Mutex;

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
    /// A line written to STDERR when this row unwinds. Empty ⇒ nothing is written.
    ///
    /// The only unload evidence that both survives a process boundary AND lands in the same
    /// ordered stream as the launcher's unresolved-row report, which is what makes "teardown
    /// BEFORE the report" (V8) assertable as behaviour rather than as source order.
    #[serde(default)]
    pub teardown_marker: String,
}

/// A deterministic fixture pane that panics on demand.
pub struct ProbePane {
    text: String,
    panic_key: String,
    /// Set by the configured key; read (and honoured) by the NEXT render. The panic has to happen
    /// inside `render` — that is the path §11 says must restore the terminal, and the path a
    /// pane's own `handle` does not exercise.
    armed: Mutex<bool>,
}

impl ProbePane {
    /// The pane the row registers.
    pub fn new(cfg: &ProbeConfig) -> ProbePane {
        ProbePane {
            text: cfg.text.clone(),
            panic_key: cfg.panic_key.clone(),
            armed: Mutex::new(false),
        }
    }

    /// PURE: whether this key arms the panic. An empty `panic_key` never does.
    pub fn arms(&self, ch: char) -> bool {
        !self.panic_key.is_empty() && self.panic_key.starts_with(ch)
    }

    /// Whether the next render will panic.
    pub fn is_armed(&self) -> bool {
        *self.armed.lock()
    }
}

#[async_trait::async_trait]
impl Pane for ProbePane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        if *self.armed.lock() {
            panic!("tui-probe: the configured panic key was pressed");
        }
        let area = cx.area;
        cx.frame
            .render_widget(ratatui::widgets::Paragraph::new(self.text.clone()), area);
    }

    async fn handle(&self, ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        if let PaneEvent::Key(key) = ev {
            if let crossterm::event::KeyCode::Char(ch) = key.code {
                if self.arms(ch) {
                    *self.armed.lock() = true;
                    return PaneOutcome::Handled;
                }
            }
        }
        PaneOutcome::Ignored
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![("probe", "panics on the configured key")]
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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // Registration is an EFFECT: unloading the row takes the pane with it.
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new("tui.probe"),
                slot: Slot::Aux,
                order: 900,
                size: SlotSize::Cells(3),
                title: "probe".to_string(),
                focusable: true,
                pane: Arc::new(ProbePane::new(&cfg)),
            },
        )
        .await?;

        if !cfg.teardown_marker.is_empty() {
            let marker = cfg.teardown_marker.clone();
            ctx.effect(move |e| async move {
                e.defer_sync(move || eprintln!("{marker}"));
                Ok(())
            })
            .await?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(panic_key: &str) -> ProbePane {
        ProbePane::new(&ProbeConfig {
            text: "PROBE-OK".to_string(),
            panic_key: panic_key.to_string(),
            teardown_marker: String::new(),
        })
    }

    #[test]
    fn the_configured_key_arms_the_panic_and_others_do_not() {
        let p = pane("p");
        assert!(p.arms('p'));
        assert!(!p.arms('q'));
        assert!(!p.is_armed(), "nothing is armed until the key is pressed");
    }

    #[test]
    fn an_empty_panic_key_never_arms() {
        let p = pane("");
        assert!(!p.arms('p'));
        assert!(!p.arms('\0'));
    }

    /// The row that never activates asks for a key nobody provides — that is the whole fixture.
    #[test]
    fn the_never_row_requires_a_key_nobody_provides() {
        let inject = <TuiNeverPlugin as Plugin>::inject();
        let keys = format!("{inject:?}");
        assert!(keys.contains("a_key_nobody_provides"), "{keys}");
    }
}
