//! Invariant: `render` is a PURE function of the `Snapshot` the pane already holds. Every read of
//! the projection seam happens in `handle`, on a tick or on a debounced `ledger/step` — never in a
//! frame (§11's render rule).

use std::sync::Arc;

use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};
use parking_lot::Mutex;

use crate::snapshot::{PreviewAt, Snapshot};
use crate::PreviewConfig;

/// Everything the pane holds between frames.
#[derive(Debug, Default)]
pub struct PreviewState {
    /// The last taken snapshot. `None` until the first refresh lands.
    pub snapshot: Option<Snapshot>,
    /// Which mode `t` last chose.
    pub mode: Option<PreviewAt>,
    /// First painted line of the viewport.
    pub scroll: usize,
    /// The viewport height of the LAST frame; `handle` has no `area` and clamping needs one.
    pub height: u16,
    /// Set when the last refresh failed; rendered inline in the theme's error role.
    pub error: Option<String>,
}

impl PreviewState {
    /// A fresh state in [`PreviewAt::Head`].
    ///
    /// WP-1.
    pub fn new() -> PreviewState {
        PreviewState {
            mode: Some(PreviewAt::Head),
            ..Default::default()
        }
    }
}

/// PURE: the header line.
/// `preview · <agent> · as_of <seq> · <tokens>/<budget> tok · <digest[..8]> · +N preface rows at wake`
///
/// WP-1.
pub fn header(state: &PreviewState, preface_rows: usize, cols: u16) -> String {
    let _ = (state, preface_rows, cols);
    todo!("WP-1: the header line, clipped to cols")
}

/// PURE: the plain-text lines the pane paints, clipped to [`PreviewConfig::max_chars`]. A clipped
/// preview SAYS it was clipped: a truncated surface that looks whole is the lie §16 forbids.
///
/// WP-1.
pub fn lines(state: &PreviewState, cfg: &PreviewConfig, cols: u16) -> Vec<String> {
    let _ = (state, cfg, cols);
    todo!("WP-1: header + body, clipped")
}

/// The pane.
pub struct PreviewPane {
    pub cfg: Arc<PreviewConfig>,
    pub state: Mutex<PreviewState>,
}

impl PreviewPane {
    /// WP-1.
    pub fn new(cfg: Arc<PreviewConfig>) -> PreviewPane {
        PreviewPane {
            cfg,
            state: Mutex::new(PreviewState::new()),
        }
    }
}

#[async_trait::async_trait]
impl Pane for PreviewPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let _ = cx;
        todo!("WP-1: paint `lines(&state, &cfg, area.width)`")
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        let _ = (ev, cx);
        todo!("WP-1: scroll, `t` toggles the mode, `y` copies, Esc is Handled")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "scroll"),
            ("t", "head / anchored"),
            ("y", "copy the whole prefix"),
            ("esc", "dismiss"),
        ]
    }
}
