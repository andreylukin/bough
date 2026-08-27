//! Invariant: `render` is a pure function of the rows and the filter the pane already holds. The
//! ledger read happens in `handle`, debounced; a frame never queries.

use std::sync::Arc;

use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};
use parking_lot::Mutex;

use crate::filter::Filter;
use crate::{Row, TimelineConfig};

/// Everything the pane holds between frames.
#[derive(Debug, Default)]
pub struct TimelineState {
    /// The one-line filter editor the pane owns (the `tui-search` query precedent).
    pub input: String,
    /// The filter currently LIVE. A parse error leaves this untouched.
    pub filter: Filter,
    /// The last parse error, rendered in the header in the theme's error role.
    pub error: Option<String>,
    pub rows: Vec<Row>,
    /// Index into `rows` under the keyboard.
    pub selected: usize,
    pub scroll: usize,
    /// The viewport height of the LAST frame.
    pub height: u16,
    /// Whether the read window was FULL: older steps exist that this timeline never read, and the
    /// header SAYS so rather than letting an unread step look like one that never happened (§16).
    pub windowed: bool,
}

impl TimelineState {
    /// WP-2.
    pub fn new(cfg: &TimelineConfig) -> TimelineState {
        let _ = cfg;
        TimelineState::default()
    }
}

/// PURE: the header line — the live filter, the row count, and the window caveat.
///
/// WP-2.
pub fn header(state: &TimelineState, cols: u16) -> String {
    let _ = (state, cols);
    todo!("WP-2: `filter · N rows · newest W steps/agent`, or the parse error")
}

/// The pane.
pub struct TimelinePane {
    pub cfg: Arc<TimelineConfig>,
    pub state: Mutex<TimelineState>,
}

impl TimelinePane {
    /// WP-2.
    pub fn new(cfg: Arc<TimelineConfig>) -> TimelinePane {
        let state = Mutex::new(TimelineState::new(&cfg));
        TimelinePane { cfg, state }
    }
}

#[async_trait::async_trait]
impl Pane for TimelinePane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let _ = cx;
        todo!("WP-2: header + one clipped line per row, each with its HitId")
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        let _ = (ev, cx);
        todo!("WP-2: edit the filter, Enter parses, click focuses, Esc clears then dismisses")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("type", "filter"),
            ("enter", "apply the filter"),
            ("↑/↓", "select a row"),
            ("enter/click", "focus that agent and step"),
            ("esc", "clear, then dismiss"),
        ]
    }
}
