//! Invariant: the pane POLLS `DriftHandle::signals` on a `refresh_ms` tick in `handle`, never in
//! `render`, and writes nothing itself. The whole reset path is `drift-watch`'s existing `/reset`
//! (D-C3): the dashboard adds a way to REACH it, not a second way to do it.
//!
//! `render` and `handle` are thin shells over the pure functions below (`lines`, `header`,
//! `on_key`, `on_click`): `RenderCx` and `PaneCx` are only constructible inside `tui-shell`, so
//! everything a test needs is a function of [`DriftState`] (the `tui-search` precedent, D-WP5-1).

use std::sync::Arc;

use bough_plugin_drift_watch::DriftHandle;
use bough_plugin_ledger::{AgentName, LedgerHandle};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};
use bough_plugin_tui_shell::HitId;
use chrono::{DateTime, Utc};
use crossterm::event::KeyCode;
use parking_lot::Mutex;
use ratatui::widgets::Paragraph;

use crate::dash::{arm, dash_row, reset_command, DashRow, ResetStep, Verdict};
use crate::render::{clip, line};
use crate::DriftPaneConfig;

/// The `HitId` prefix the `[reset]` region of a row is clickable under.
pub const RESET_HIT_PREFIX: &str = "drift:reset:";

/// The label the clickable reset region draws.
pub const RESET_LABEL: &str = "[reset]";

/// The `HitId` a row's `[reset]` region is recorded under.
pub fn reset_hit(agent: &AgentName) -> HitId {
    HitId::new(format!("{RESET_HIT_PREFIX}{agent}"))
}

/// The agent a `HitId` names, when it is one of ours.
pub fn agent_of_hit(hit: &HitId) -> Option<AgentName> {
    hit.as_str()
        .strip_prefix(RESET_HIT_PREFIX)
        .map(AgentName::new)
}

/// Everything the pane holds between frames. `render` is a pure function of it.
#[derive(Debug, Default)]
pub struct DriftState {
    pub rows: Vec<DashRow>,
    /// Index into `rows` under the keyboard.
    pub selected: usize,
    /// The armed reset, if any: which agent, and when it was armed (D-C5).
    pub armed: Option<(AgentName, DateTime<Utc>)>,
    /// Set when the last poll failed; rendered inline in the theme's error role.
    pub error: Option<String>,
    /// When the last poll ran. `None` until the first one — which is why an empty dashboard says
    /// "not polled yet" rather than "no agents".
    pub polled_at: Option<DateTime<Utc>>,
    pub height: u16,
}

impl DriftState {
    /// The row under the keyboard.
    pub fn selected_row(&self) -> Option<&DashRow> {
        self.rows.get(self.selected)
    }

    /// Move the selection by `delta`, clamped. Total over an empty list.
    pub fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.rows.len() as i32 - 1;
        self.selected = (self.selected as i32 + delta).clamp(0, max) as usize;
    }

    /// Land a poll result. A failure CLEARS nothing: the numbers on screen were true when they
    /// were read, and blanking them would replace a stale truth with a blank that reads as
    /// "steady, nothing to see".
    pub fn apply(&mut self, at: DateTime<Utc>, result: Result<Vec<DashRow>, String>) {
        self.polled_at = Some(at);
        match result {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
                self.move_selection(0);
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Whether the poll is due again.
    pub fn poll_due(&self, now: DateTime<Utc>, refresh_ms: u64) -> bool {
        match self.polled_at {
            None => true,
            Some(t) => (now - t).num_milliseconds() >= refresh_ms as i64,
        }
    }

    /// Drop an arm that has run out of time, so the header cannot advertise an arm a second `r`
    /// would no longer fire.
    pub fn expire_arm(&mut self, now: DateTime<Utc>, arm_ms: u64) -> bool {
        if armed_expired(self.armed.as_ref(), now, arm_ms) {
            self.armed = None;
            return true;
        }
        false
    }
}

/// PURE: the header line — how many agents, how many are flagged, and the armed notice.
pub fn header(state: &DriftState, cfg: &DriftPaneConfig, cols: u16) -> String {
    let n = state.rows.len();
    let flagged = state
        .rows
        .iter()
        .filter(|r| r.verdict == Verdict::Flagged)
        .count();
    let unknown = state
        .rows
        .iter()
        .filter(|r| r.verdict == Verdict::TooFewSamples)
        .count();
    let mut head = format!("drift \u{b7} {n} agents \u{b7} {flagged} flagged");
    if unknown > 0 {
        // §16 again: the count of rows there is not enough evidence for is SAID, never folded
        // into the steady majority.
        head.push_str(&format!(" \u{b7} {unknown} too few samples"));
    }
    if state.polled_at.is_none() {
        head.push_str(" \u{b7} not polled yet");
    }
    if let Some((agent, _)) = &state.armed {
        head.push_str(&format!(
            " \u{b7} armed: press r again to reset {agent} ({}ms)",
            cfg.arm_ms
        ));
    }
    clip(&head, cols)
}

/// PURE: the plain-text lines the pane paints, each with the `HitId` its `[reset]` region is
/// clickable under. What a text assertion reads, and what `render` paints.
pub fn lines(state: &DriftState, cfg: &DriftPaneConfig, cols: u16) -> Vec<(String, Option<HitId>)> {
    let mut out = vec![(header(state, cfg, cols), None)];
    if let Some(err) = &state.error {
        out.push((clip(&format!("! {err}"), cols), None));
    }
    let shown = cfg.agents_shown.min(state.rows.len());
    let room = cols.saturating_sub(RESET_LABEL.chars().count() as u16 + 1);
    for (i, row) in state.rows.iter().take(shown).enumerate() {
        let marker = if i == state.selected { ">" } else { " " };
        let text = format!("{marker}{} {RESET_LABEL}", line(row, room, cfg.bar_cols));
        out.push((clip(&text, cols), Some(reset_hit(&row.agent))));
    }
    if state.rows.len() > shown {
        out.push((
            clip(&format!("\u{2026} {} more", state.rows.len() - shown), cols),
            None,
        ));
    }
    out
}

/// PURE: an arm older than `arm_ms` is no longer armed.
pub fn armed_expired(
    armed: Option<&(AgentName, DateTime<Utc>)>,
    now: DateTime<Utc>,
    arm_ms: u64,
) -> bool {
    match armed {
        None => false,
        Some((_, at)) => (now - *at).num_milliseconds() >= arm_ms as i64,
    }
}

/// PURE: what a key means. The whole keyboard surface of the pane, as a function of the state it
/// already holds — no clock read, no I/O.
///
/// `Esc` DISARMS before it dismisses: with a reset armed, the key spends itself cancelling the arm
/// and reports `Handled`. The shell returns the keyboard to the composer either way (§ux1's Esc
/// rule), which is recorded in `docs/track-c-merge-notes.md`.
pub fn on_key(
    state: &mut DriftState,
    code: KeyCode,
    now: DateTime<Utc>,
    arm_ms: u64,
) -> PaneOutcome {
    match code {
        KeyCode::Up => {
            state.move_selection(-1);
            PaneOutcome::Handled
        }
        KeyCode::Down => {
            state.move_selection(1);
            PaneOutcome::Handled
        }
        KeyCode::Char('r') => match state.selected_row().map(|r| r.agent.clone()) {
            None => PaneOutcome::Ignored,
            Some(agent) => press_reset(state, &agent, now, arm_ms),
        },
        KeyCode::Esc => {
            if state.armed.take().is_some() {
                PaneOutcome::Handled
            } else {
                PaneOutcome::Ignored
            }
        }
        _ => PaneOutcome::Ignored,
    }
}

/// PURE: the two-step reset, shared by the key and the click so they cannot disagree.
pub fn press_reset(
    state: &mut DriftState,
    agent: &AgentName,
    now: DateTime<Utc>,
    arm_ms: u64,
) -> PaneOutcome {
    // An expired arm is not an arm: without this, `r` … a minute … `r` would fire.
    state.expire_arm(now, arm_ms);
    match arm(state.armed.clone(), agent, now, arm_ms) {
        ResetStep::Arm => {
            state.armed = Some((agent.clone(), now));
            PaneOutcome::Handled
        }
        ResetStep::Fire => {
            state.armed = None;
            PaneOutcome::Command(reset_command(agent))
        }
    }
}

/// PURE: what a click on a recorded `[reset]` region means. Two-step, exactly as the key is.
pub fn on_click(
    state: &mut DriftState,
    hit: Option<&HitId>,
    now: DateTime<Utc>,
    arm_ms: u64,
) -> PaneOutcome {
    let Some(agent) = hit.and_then(agent_of_hit) else {
        return PaneOutcome::Ignored;
    };
    let Some(i) = state.rows.iter().position(|r| r.agent == agent) else {
        return PaneOutcome::Ignored;
    };
    // Clicking a row selects it, so the header's armed notice names the row that was clicked.
    state.selected = i;
    press_reset(state, &agent, now, arm_ms)
}

/// The pane.
pub struct DriftPane {
    pub cfg: Arc<DriftPaneConfig>,
    /// The `drift` seam. Polled in `handle`, never in `render`.
    pub drift: Option<DriftHandle>,
    /// The ledger, for the agent list a poll iterates.
    pub ledger: Option<LedgerHandle>,
    pub state: Mutex<DriftState>,
}

impl DriftPane {
    pub fn new(cfg: Arc<DriftPaneConfig>) -> DriftPane {
        DriftPane {
            cfg,
            drift: None,
            ledger: None,
            state: Mutex::new(DriftState::default()),
        }
    }

    /// Attach the seams a poll needs. A pane without them renders and arms exactly as it does
    /// with them; it simply never has rows (which is what a unit test drives).
    pub fn with_seams(mut self, drift: DriftHandle, ledger: LedgerHandle) -> DriftPane {
        self.drift = Some(drift);
        self.ledger = Some(ledger);
        self
    }

    /// One poll. The ONLY I/O in the crate, and it is never called from `render`.
    pub async fn poll(&self, now: DateTime<Utc>) -> Result<Vec<DashRow>, String> {
        let (Some(drift), Some(ledger)) = (self.drift.as_ref(), self.ledger.as_ref()) else {
            return Ok(Vec::new());
        };
        let agents = ledger.0.agents().await.map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        for a in &agents {
            match drift.signals(&a.name, now).await {
                Ok(s) => rows.push(dash_row(&s)),
                // An agent with no trajectory yet is not a failed dashboard: it has nothing to
                // measure, and the rest of the tree is still worth reporting.
                Err(_) => continue,
            }
        }
        Ok(rows)
    }

    /// Poll if the refresh tick is due, and say whether anything on screen changed.
    async fn maybe_poll(&self, now: DateTime<Utc>) -> bool {
        if !self.state.lock().poll_due(now, self.cfg.refresh_ms) {
            return false;
        }
        let result = self.poll(now).await;
        self.state.lock().apply(now, result);
        true
    }
}

#[async_trait::async_trait]
impl Pane for DriftPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let state = self.state.lock();
        let area = cx.area;
        let painted = lines(&state, &self.cfg, area.width);
        let theme = *cx.theme();
        let mut out: Vec<ratatui::text::Line> = Vec::with_capacity(painted.len());
        let mut hits: Vec<(usize, HitId)> = Vec::new();
        for (i, (text, hit)) in painted.into_iter().enumerate() {
            let style = if state.error.is_some() && i == 1 {
                ratatui::style::Style::default().fg(theme.error)
            } else {
                ratatui::style::Style::default()
            };
            out.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                text, style,
            )));
            if let Some(hit) = hit {
                hits.push((i, hit));
            }
        }
        drop(state);
        for (i, hit) in hits {
            if let Some(rect) = row_rect(area, i) {
                cx.hit(rect, hit);
            }
        }
        // `handle` has no `area`; the height it clamps against is whatever the last frame had.
        self.state.lock().height = area.height;
        cx.frame.render_widget(Paragraph::new(out), area);
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Key(key) => {
                let outcome = {
                    let mut st = self.state.lock();
                    on_key(&mut st, key.code, cx.at, self.cfg.arm_ms)
                };
                if outcome != PaneOutcome::Ignored {
                    if let PaneOutcome::Handled = outcome {
                        if let Some((agent, _)) = self.state.lock().armed.clone() {
                            cx.tui.notify(format!("press r again to reset {agent}"));
                        }
                    }
                    cx.tui.redraw();
                }
                outcome
            }
            PaneEvent::Click { hit, .. } => {
                let outcome = {
                    let mut st = self.state.lock();
                    on_click(&mut st, hit.as_ref(), cx.at, self.cfg.arm_ms)
                };
                if outcome != PaneOutcome::Ignored {
                    cx.tui.redraw();
                }
                outcome
            }
            // THE POLL, on the refresh tick — never in `render` (this module's invariant).
            PaneEvent::Tick => {
                let expired = self.state.lock().expire_arm(cx.at, self.cfg.arm_ms);
                let polled = self.maybe_poll(cx.at).await;
                if expired || polled {
                    cx.tui.redraw();
                    return PaneOutcome::Handled;
                }
                PaneOutcome::Ignored
            }
            // Opening the dashboard shows what is true NOW, not what was true when it last had
            // the keyboard.
            PaneEvent::FocusChanged(true) => {
                self.state.lock().polled_at = None;
                if self.maybe_poll(cx.at).await {
                    cx.tui.redraw();
                }
                PaneOutcome::Handled
            }
            // Losing the keyboard disarms: an arm that survived the pane losing focus would fire
            // on an `r` typed somewhere else entirely.
            PaneEvent::FocusChanged(false) => {
                if self.state.lock().armed.take().is_some() {
                    cx.tui.redraw();
                    return PaneOutcome::Handled;
                }
                PaneOutcome::Ignored
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("\u{2191}/\u{2193}", "select an agent"),
            ("r r", "rebuild this agent's identity"),
            ("esc", "disarm, then dismiss"),
        ]
    }
}

/// The rect a painted line occupies, if it is on screen.
fn row_rect(area: ratatui::layout::Rect, index: usize) -> Option<ratatui::layout::Rect> {
    let i = u16::try_from(index).ok()?;
    if i >= area.height {
        return None;
    }
    Some(ratatui::layout::Rect {
        x: area.x,
        y: area.y + i,
        width: area.width,
        height: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dash::Verdict;
    use bough_plugin_drift_watch::SignalState;

    fn cfg() -> DriftPaneConfig {
        DriftPaneConfig {
            height: 10,
            collapse_rows: 20,
            min_rows: 4,
            max_rows: 20,
            agents_shown: 8,
            refresh_ms: 2_000,
            bar_cols: 8,
            arm_ms: 3_000,
        }
    }

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_700_000_000_000 + ms).expect("a fixed instant")
    }

    fn row(name: &str) -> DashRow {
        DashRow {
            agent: AgentName::new(name),
            samples: 40,
            thought_cv: 0.2,
            tool_entropy: 0.8,
            top_tools: Vec::new(),
            claim_rejection: SignalState::Inactive {
                since: "no claim in the window has been decided".into(),
            },
            flags: Vec::new(),
            verdict: Verdict::Steady,
        }
    }

    fn state() -> DriftState {
        DriftState {
            rows: vec![row("sol"), row("terra")],
            polled_at: Some(at(0)),
            ..Default::default()
        }
    }

    #[test]
    fn r_twice_returns_the_reset_command_for_the_focused_row() {
        let mut st = state();
        // Move to the SECOND row, so a command naming the first would be visibly wrong.
        assert_eq!(
            on_key(&mut st, KeyCode::Down, at(0), 3_000),
            PaneOutcome::Handled
        );
        assert_eq!(st.selected, 1);
        assert_eq!(
            on_key(&mut st, KeyCode::Char('r'), at(10), 3_000),
            PaneOutcome::Handled
        );
        assert_eq!(
            st.armed.as_ref().map(|(a, _)| a.to_string()),
            Some("terra".into())
        );
        assert_eq!(
            on_key(&mut st, KeyCode::Char('r'), at(20), 3_000),
            PaneOutcome::Command("/reset terra".to_string())
        );
        // Firing consumes the arm: a third `r` arms again rather than resetting twice.
        assert!(st.armed.is_none());
        assert_eq!(
            on_key(&mut st, KeyCode::Char('r'), at(30), 3_000),
            PaneOutcome::Handled
        );
    }

    #[test]
    fn the_reset_command_is_exactly_slash_reset_agent() {
        let mut st = state();
        let _ = on_key(&mut st, KeyCode::Char('r'), at(0), 3_000);
        let out = on_key(&mut st, KeyCode::Char('r'), at(1), 3_000);
        // The pane dispatches drift-watch's own `/reset` and adds no write path of its own (D-C3).
        assert_eq!(out, PaneOutcome::Command("/reset sol".to_string()));
        match out {
            PaneOutcome::Command(line) => {
                assert_eq!(line, reset_command(&AgentName::new("sol")));
                assert!(line.starts_with("/reset "));
            }
            other => panic!("expected a command, got {other:?}"),
        }
        // The click path spells it the same way.
        let mut st = state();
        let hit = reset_hit(&AgentName::new("terra"));
        assert_eq!(
            on_click(&mut st, Some(&hit), at(0), 3_000),
            PaneOutcome::Handled
        );
        assert_eq!(
            on_click(&mut st, Some(&hit), at(1), 3_000),
            PaneOutcome::Command("/reset terra".to_string())
        );
    }

    #[test]
    fn esc_disarms_before_it_dismisses() {
        let mut st = state();
        let _ = on_key(&mut st, KeyCode::Char('r'), at(0), 3_000);
        assert!(st.armed.is_some());
        // ARMED: the key spends itself on the arm.
        assert_eq!(
            on_key(&mut st, KeyCode::Esc, at(1), 3_000),
            PaneOutcome::Handled
        );
        assert!(st.armed.is_none());
        // …and the very next `r` ARMS rather than fires, which is the point of disarming.
        assert_eq!(
            on_key(&mut st, KeyCode::Char('r'), at(2), 3_000),
            PaneOutcome::Handled
        );
        st.armed = None;
        // UNARMED: the pane does not eat the key, so the shell dismisses.
        assert_eq!(
            on_key(&mut st, KeyCode::Esc, at(3), 3_000),
            PaneOutcome::Ignored
        );
    }

    #[test]
    fn render_is_a_pure_function_of_the_held_signals() {
        let st = state();
        let cfg = cfg();
        // Same state, same bytes, every time: no clock is read and nothing is polled to draw.
        assert_eq!(lines(&st, &cfg, 100), lines(&st, &cfg, 100));
        let painted = lines(&st, &cfg, 100);
        assert_eq!(painted.len(), 1 + st.rows.len());
        assert!(painted[0].0.starts_with("drift \u{b7} 2 agents"));
        assert!(painted[1].0.contains("sol"));
        assert_eq!(painted[1].1, Some(reset_hit(&AgentName::new("sol"))));
        // A DIFFERENT state draws differently — the function is of the signals, not of nothing.
        let mut other = state();
        other.rows[0].verdict = Verdict::Flagged;
        assert_ne!(lines(&other, &cfg, 100), painted);
        assert!(lines(&other, &cfg, 100)[0].0.contains("1 flagged"));
        // Every line honours the width.
        for (text, _) in lines(&st, &cfg, 40) {
            assert!(text.chars().count() <= 40);
        }
    }

    #[test]
    fn an_expired_arm_does_not_fire() {
        let mut st = state();
        let _ = on_key(&mut st, KeyCode::Char('r'), at(0), 3_000);
        // The second `r` lands after arm_ms: it re-ARMS rather than rebuilding an identity the
        // user stopped asking for three seconds ago.
        assert_eq!(
            on_key(&mut st, KeyCode::Char('r'), at(4_000), 3_000),
            PaneOutcome::Handled
        );
        assert!(armed_expired(
            Some(&(AgentName::new("sol"), at(0))),
            at(3_000),
            3_000
        ));
        assert!(!armed_expired(
            Some(&(AgentName::new("sol"), at(0))),
            at(2_999),
            3_000
        ));
        assert!(!armed_expired(None, at(9_999), 3_000));
    }

    #[test]
    fn a_poll_failure_keeps_the_numbers_that_were_true() {
        let mut st = state();
        st.apply(at(1), Err("ledger is gone".into()));
        assert_eq!(st.rows.len(), 2, "a failed poll does not blank the board");
        assert_eq!(st.error.as_deref(), Some("ledger is gone"));
        let painted = lines(&st, &cfg(), 100);
        assert!(painted[1].0.starts_with("! ledger is gone"));
    }

    #[test]
    fn the_agent_list_is_bounded_by_agents_shown() {
        let mut cfg = cfg();
        cfg.agents_shown = 1;
        let painted = lines(&state(), &cfg, 100);
        assert_eq!(painted.len(), 3, "header, one row, and the `… more` line");
        assert!(painted[2].0.contains("1 more"));
    }
}
