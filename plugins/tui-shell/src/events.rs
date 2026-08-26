//! Invariant: dispatch modes are part of the public contract (§0.2). `tui/focus` is an EMIT
//! mirror of shell state and nothing durable rides it (P2-D25); `tui/key` is a WATERFALL, so a
//! listener that wants a binding without touching the shell MUST call `next()` to delegate.

use bough_kernel::{EmitEvent, WaterfallEvent};
use bough_plugin_agents::AgentId;
use bough_plugin_ledger::StepId;
use crossterm::event::KeyEvent;

use crate::pane::PaneId;

/// Where focus should go. Every field is optional: a request names only what it changes.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FocusRequest {
    pub agent: Option<AgentId>,
    pub pane: Option<PaneId>,
    /// Scroll the trajectory so this step is visible and highlighted.
    pub step: Option<StepId>,
}

/// `tui/focus` — EMIT. A live mirror of shell state; nothing durable rides it (P2-D25).
pub struct TuiFocusEvent;

impl EmitEvent for TuiFocusEvent {
    const NAME: &'static str = "tui/focus";
    type Payload = FocusRequest;
}

/// One key, on its way to a pane.
#[derive(Clone, Debug)]
pub struct KeyDispatch {
    pub key: KeyEvent,
    pub target: PaneId,
    pub composer_focused: bool,
    /// A listener that sets this to `true` consumes the key; the shell's keymap then skips it.
    pub handled: bool,
}

/// `tui/key` — WATERFALL. The extension point for a plugin that wants a keybinding without
/// touching the shell (P3-D18). Listeners MUST call `next()` to delegate.
pub struct TuiKeyEvent;

impl WaterfallEvent for TuiKeyEvent {
    const NAME: &'static str = "tui/key";
    type Value = KeyDispatch;
}
