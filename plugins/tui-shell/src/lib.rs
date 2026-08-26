//! Invariant: this crate OWNS THE TERMINAL, and nothing else in the tree touches it. It is the
//! only place that enters raw mode and the alt screen, the only place that draws, and the only
//! place that restores — on a clean quit, on a boot failure, on a panic and on SIGINT (§11, V8).
//! Panes are registered as EFFECTS: a pane row unloading reflows the layout with no restart, and
//! that is the phase's SWAP gate.
//!
//! It drives `ctx.agents` and reads `ctx.ledger`; it never imports `bough-plugin-agent-loop`.

pub mod clip;
pub mod composer;
pub mod events;
pub mod invariant;
pub mod pane;
pub mod run;
pub mod select;
pub mod term;
pub mod theme;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_agents::AgentId;
use bough_plugin_ledger::StepId;
use ratatui::layout::Rect;

pub use clip::{copy, CopyOutcome};
pub use composer::{Composer, ComposerAction};
pub use events::{FocusRequest, KeyDispatch, TuiFocusEvent, TuiKeyEvent};
pub use pane::{
    HitId, Pane, PaneCx, PaneEvent, PaneId, PaneInfo, PaneOutcome, PaneSpec, RenderCx, ShellView,
    Slot, SlotSize,
};
pub use select::{text_from_buffer, Selection};
pub use term::{install_panic_hook, restore_now, TerminalGuard};
pub use theme::{Backend, Theme, ThemeName};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-shell";

/// The `tui` service key.
pub struct Tui;

impl ServiceKey for Tui {
    type Value = TuiHandle;
    const NAME: &'static str = "tui";
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct TuiHandle(pub Arc<TuiInner>);

/// The shell's live state: the pane registry, the focus, the composer, the last frame.
pub struct TuiInner {
    _private: (),
}

impl TuiHandle {
    /// Register a pane. An EFFECT (§0.2): the returned disposer removes the pane from its slot,
    /// drops its hit map and requests a redraw, so a pane row unloading reflows the layout with
    /// no restart (the SWAP gate).
    pub async fn register_pane(
        &self,
        _ctx: &Context,
        _spec: PaneSpec,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-2")
    }

    /// Every live pane, sorted by (slot, order, id). Stable across frames.
    pub fn panes(&self) -> Vec<PaneInfo> {
        todo!("WP-2")
    }

    /// The agent the `Main` slot is showing, if any.
    pub fn focused_agent(&self) -> Option<AgentId> {
        todo!("WP-2")
    }

    /// The pane holding keyboard focus.
    pub fn focused_pane(&self) -> PaneId {
        todo!("WP-2")
    }

    /// Moves focus and emits `tui/focus`. `step` is a request the focus pane consumes.
    pub async fn focus(&self, _req: FocusRequest) {
        todo!("WP-2")
    }

    /// Move keyboard focus to one pane.
    pub async fn focus_pane(&self, _pane: PaneId) {
        todo!("WP-2")
    }

    /// Coalesced: many calls in one frame budget cost one frame.
    pub fn redraw(&self) {
        todo!("WP-2")
    }

    /// One-line transient message in [`Slot::Status`].
    pub fn notify(&self, _text: impl Into<String>) {
        todo!("WP-2")
    }

    /// OSC52 to the terminal + `arboard` when configured. Never fails the caller (P3-D7).
    pub async fn copy(&self, _text: &str) -> CopyOutcome {
        todo!("WP-2")
    }

    /// The whole terminal.
    pub fn size(&self) -> Rect {
        todo!("WP-2")
    }

    /// Which backend the shell resolved `Backend::Auto` to (P3-D2).
    pub fn backend(&self) -> Backend {
        todo!("WP-2")
    }

    /// The last rendered buffer. The selection reads from it; tests assert against it.
    pub fn last_frame(&self) -> Arc<ratatui::buffer::Buffer> {
        todo!("WP-2")
    }

    /// The step the focus pane was last asked to anchor on, if any.
    pub fn anchored_step(&self) -> Option<StepId> {
        todo!("WP-2")
    }

    /// Ask the process to end. Delegates to `Kernel::request_exit` (P2-D23): the launcher still
    /// owns teardown, and teardown is what restores the terminal.
    pub fn quit(&self, _code: u8) {
        todo!("WP-2")
    }
}

/// Everything the shell can go wrong as before there is a screen to say it on.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal setup failed at `{step}`: {source}")]
    Terminal {
        step: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Failed(String),
}

/// The row's config. Every deployment-varying value is here; nothing is a `DEFAULT_` constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TuiConfig {
    /// `auto` (default): crossterm when stdout is a TTY, else the headless TestBackend, so
    /// `--check` and CI can mount the tui profile without a terminal (P3-D2).
    pub backend: Backend,
    /// Size of the headless backend. Ignored by crossterm.
    pub size: [u16; 2],
    /// Redraw coalescing budget.
    pub frame_ms: u64,
    /// Relative-time refresh; also the [`PaneEvent::Tick`] cadence.
    pub tick_ms: u64,
    pub theme: ThemeName,
    pub mouse: bool,
    pub osc52: bool,
    /// Best-effort `arboard` in addition to OSC52.
    pub clipboard: bool,
    pub composer_max_lines: u16,
}

/// The row.
pub struct TuiShellPlugin;

#[async_trait::async_trait]
impl Plugin for TuiShellPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = TuiConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger", "commands"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-2: guard, panic hook, provide `tui`, spawn the loop, register the built-ins")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TuiShellPlugin);
