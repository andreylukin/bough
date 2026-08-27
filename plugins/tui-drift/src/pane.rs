//! Invariant: the pane POLLS `DriftHandle::signals` on a `refresh_ms` tick in `handle`, never in
//! `render`, and writes nothing itself. The whole reset path is `drift-watch`'s existing `/reset`
//! (D-C3): the dashboard adds a way to REACH it, not a second way to do it.

use std::sync::Arc;

use bough_plugin_ledger::AgentName;
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::dash::DashRow;
use crate::DriftPaneConfig;

/// The `HitId` prefix the `[reset]` region of a row is clickable under.
pub const RESET_HIT_PREFIX: &str = "drift:reset:";

/// Everything the pane holds between frames.
#[derive(Debug, Default)]
pub struct DriftState {
    pub rows: Vec<DashRow>,
    /// Index into `rows` under the keyboard.
    pub selected: usize,
    /// The armed reset, if any: which agent, and when it was armed (D-C5).
    pub armed: Option<(AgentName, DateTime<Utc>)>,
    /// Set when the last poll failed; rendered inline in the theme's error role.
    pub error: Option<String>,
    pub height: u16,
}

/// PURE: the header line — how many agents, how many are flagged, and the armed notice.
///
/// WP-3.
pub fn header(state: &DriftState, cfg: &DriftPaneConfig, cols: u16) -> String {
    let _ = (state, cfg, cols);
    todo!("WP-3: `drift · N agents · M flagged`, plus `armed: reset <agent>?` when armed")
}

/// The pane.
pub struct DriftPane {
    pub cfg: Arc<DriftPaneConfig>,
    pub state: Mutex<DriftState>,
}

impl DriftPane {
    /// WP-3.
    pub fn new(cfg: Arc<DriftPaneConfig>) -> DriftPane {
        DriftPane {
            cfg,
            state: Mutex::new(DriftState::default()),
        }
    }
}

#[async_trait::async_trait]
impl Pane for DriftPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let _ = cx;
        todo!("WP-3: header + one clipped line per row, each with its [reset] hit region")
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        let _ = (ev, cx);
        todo!("WP-3: ↑/↓ move, `r` arms then fires, Esc disarms before it dismisses")
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "select an agent"),
            ("r r", "rebuild this agent's identity"),
            ("esc", "disarm, then dismiss"),
        ]
    }
}

/// PURE: what the armed clock says now — used by the header and by the disarm-on-expiry tick.
///
/// WP-3.
pub fn armed_expired(armed: Option<&(AgentName, DateTime<Utc>)>, now: DateTime<Utc>, arm_ms: u64) -> bool {
    let _ = (armed, now, arm_ms);
    todo!("WP-3: an arm older than arm_ms is no longer armed")
}
