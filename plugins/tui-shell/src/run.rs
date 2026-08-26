//! Invariant: ONE task owns the screen. Every draw, every hit map and every `last_frame` publish
//! happens in this loop, so no two writers can interleave escape sequences. A panic inside a
//! pane's render unwinds this task; the panic hook has already restored the terminal, and the
//! loop asks the kernel to exit with code 101 so the launcher tears the tree down (V8).

use bough_kernel::Context;

use crate::pane::{PaneEvent, PaneId, PaneOutcome};
use crate::{TuiConfig, TuiHandle};

/// The event loop, spawned as the row's effect. Returns when the effect is halted.
pub async fn run(_ctx: Context, _tui: TuiHandle, _cfg: std::sync::Arc<TuiConfig>) {
    todo!("WP-2")
}

/// Draw one frame: layout slots → each pane's `render` into a fresh `HitMap` → overlay the
/// selection highlight → publish `last_frame`.
pub fn draw(_tui: &TuiHandle) {
    todo!("WP-2")
}

/// Route one already-typed pane event and act on its outcome (focus, command, compose).
pub async fn route(_tui: &TuiHandle, _target: PaneId, _ev: PaneEvent) -> PaneOutcome {
    todo!("WP-2")
}
