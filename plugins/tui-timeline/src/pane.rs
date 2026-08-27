//! Invariant: `render` is a pure function of the rows and the filter the pane already holds. The
//! ledger read happens in `handle`, debounced; a frame never queries.
//!
//! The pane owns the filter EDITOR, the row focus and the click map — and nothing that decides
//! what a timeline IS: that is [`crate::order::timeline`], which the pane calls and never
//! second-guesses. Painted line *i* is visible row *i - 1* (line 0 is the header/editor), which is
//! what makes the click map a lookup rather than a search.
//!
//! `Esc` is a TWO-STEP (phase ux1's Esc rule): a non-empty editor clears, and only an empty editor
//! lets `Esc` through to the shell, which dismisses the pane. One `Esc` that both threw away a
//! typed filter and closed the pane is the interaction that rule exists to prevent.

use std::sync::Arc;

use bough_plugin_agents::{AgentId, AgentsHandle};
use bough_plugin_ledger::{AgentName, LedgerHandle, StepId};
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneId, PaneOutcome, RenderCx, Slot};
use bough_plugin_tui_shell::{FocusRequest, HitId, TuiHandle};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use ratatui::widgets::Paragraph;

use crate::filter::{parse_filter, render_filter, Filter};
use crate::order::timeline;
use crate::render::{hit_of, line, step_of_hit};
use crate::{load_rows, Loaded, Row, TimelineConfig};

/// The label of the pane's one editable field.
pub const FIELD_LABEL: &str = "filter";

/// What `Esc` meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// The editor had text (or an error); it is gone and the pane stays.
    Cleared,
    /// The editor was already empty: the shell dismisses the pane.
    Dismiss,
}

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
    /// The step ids the query behind `rows` returned — the set the invariant checks a frame
    /// against.
    pub queried: Vec<StepId>,
    /// Index into `rows` under the keyboard.
    pub selected: usize,
    pub scroll: usize,
    /// The viewport height of the LAST frame.
    pub height: u16,
    /// Whether the read window was FULL: older steps exist that this timeline never read, and the
    /// header SAYS so rather than letting an unread step look like one that never happened (§16).
    pub windowed: bool,
    /// [`TimelineConfig::limit`] and [`TimelineConfig::window`], copied so the pure functions over
    /// this state need no config.
    pub limit: usize,
    pub window: usize,
    pub time_format: String,
    /// A read is armed or running. One at a time: a pane that armed a 400-step-per-agent read on
    /// every tick starved the wake it was watching (found by `scripts/tui/01-boot-and-turn.sh`).
    pub loading: bool,
    /// When the last read landed. A tick sooner than `debounce_ms` after it is not due.
    pub loaded_at: Option<DateTime<Utc>>,
}

impl TimelineState {
    pub fn new(cfg: &TimelineConfig) -> TimelineState {
        TimelineState {
            limit: cfg.limit,
            window: cfg.window,
            time_format: cfg.time_format.clone(),
            ..TimelineState::default()
        }
    }

    /// Whether a tick should read again: none in flight, and `debounce_ms` since the last.
    pub fn due(&self, now: DateTime<Utc>, debounce_ms: u64) -> bool {
        if self.loading {
            return false;
        }
        match self.loaded_at {
            None => true,
            Some(then) => (now - then).num_milliseconds() >= debounce_ms as i64,
        }
    }

    /// PURE: what is on screen — the timeline of the loaded rows under the live filter.
    pub fn visible(&self) -> Vec<Row> {
        timeline(&self.rows, &self.filter, self.limit)
    }

    /// PURE: the plain-text lines the pane paints, each with the `HitId` its row is clickable
    /// under. Line 0 is the header and is not clickable.
    pub fn lines(&self, cols: u16) -> Vec<(String, Option<HitId>)> {
        let visible = self.visible();
        let mut out = vec![(header_of(self, visible.len(), cols), None)];
        if let Some(err) = &self.error {
            // The error renders and the PREVIOUS rows stay: the header names the filter they are
            // under, so nothing on screen is unlabelled.
            out.push((format!("! {err}"), None));
        }
        for row in &visible {
            out.push((line(row, cols, &self.time_format), Some(hit_of(row))));
        }
        out
    }

    /// A typed character.
    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// `Enter`: parse the editor. A parse error renders in the header and the previous filter
    /// stays live; a good parse replaces it and resets the selection.
    pub fn submit(&mut self, now: DateTime<Utc>) -> Result<(), String> {
        match parse_filter(&self.input, now) {
            Ok(f) => {
                self.filter = f;
                self.error = None;
                self.selected = 0;
                self.scroll = 0;
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                self.error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// `Esc`, step one or step two. The editor and the filter go together: an empty editor beside
    /// a live filter would be a pane whose field disagreed with its own rows.
    pub fn escape(&mut self) -> Escape {
        if self.input.is_empty() && self.error.is_none() && self.filter.is_empty() {
            return Escape::Dismiss;
        }
        self.input.clear();
        self.filter = Filter::default();
        self.error = None;
        self.selected = 0;
        self.scroll = 0;
        Escape::Cleared
    }

    /// Put a filter in the editor AND make it live — what `/timeline agent:sol` does.
    pub fn set_filter(&mut self, f: Filter, now: DateTime<Utc>) {
        self.input = render_filter(&f, now);
        self.filter = f;
        self.error = None;
        self.selected = 0;
        self.scroll = 0;
    }

    /// Land a query result.
    pub fn apply(&mut self, loaded: Loaded) {
        self.queried = loaded.rows.iter().map(|r| r.step.id.clone()).collect();
        self.rows = loaded.rows;
        self.windowed = loaded.windowed;
        let n = self.visible().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    /// Move the row focus, clamped: a timeline does not wrap, because its ends are real.
    pub fn move_selection(&mut self, delta: i32) {
        let n = self.visible().len();
        if n == 0 {
            self.selected = 0;
            return;
        }
        let to = (self.selected as i64 + delta as i64).clamp(0, n as i64 - 1);
        self.selected = to as usize;
        self.follow_selection();
    }

    /// Keep the selected row inside the viewport. Painted line = `selected + 1 + error rows`.
    pub fn follow_selection(&mut self) {
        let line = self.selected + 1 + usize::from(self.error.is_some());
        let height = self.height.max(1) as usize;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + height {
            self.scroll = line + 1 - height;
        }
    }

    /// PURE: the first painted line of the viewport, clamped to what there is to show.
    pub fn top(&self, painted: usize, height: u16) -> usize {
        self.scroll.min(painted.saturating_sub(height as usize))
    }

    pub fn scroll_by(&mut self, delta: i32, painted: usize) {
        let max = painted.saturating_sub(self.height as usize) as i64;
        let to = (self.scroll as i64 + delta as i64).clamp(0, max.max(0));
        self.scroll = to as usize;
    }

    /// The hit under the keyboard, for `Enter` on a selected row.
    pub fn selected_hit(&self) -> Option<HitId> {
        self.visible().get(self.selected).map(hit_of)
    }
}

/// PURE: the header line — the editor, the live filter, the row count, and the window caveat.
pub fn header(state: &TimelineState, cols: u16) -> String {
    header_of(state, state.visible().len(), cols)
}

fn header_of(state: &TimelineState, rows: usize, cols: u16) -> String {
    let mut text = format!(
        "{FIELD_LABEL} [{}\u{258f}]  {} \u{b7} {rows} rows",
        state.input,
        state.filter.describe()
    );
    if state.windowed {
        // §16: an unread step must not read the same as one that never happened.
        text.push_str(&format!(" \u{b7} newest {} steps/agent", state.window));
    }
    let cols = cols as usize;
    if text.chars().count() > cols {
        text = text.chars().take(cols).collect();
    }
    text
}

/// PURE: what a click on a row means. A row is a step, so the outcome is a `FocusRequest` — never
/// a wake.
pub fn on_click(
    hit: Option<&HitId>,
    rows: &[Row],
    focus_pane: Option<PaneId>,
    resolve: impl Fn(&AgentName) -> Option<AgentId>,
) -> PaneOutcome {
    let Some(hit) = hit else {
        return PaneOutcome::Ignored;
    };
    let Some(step) = step_of_hit(hit) else {
        return PaneOutcome::Ignored;
    };
    let Some(row) = rows.iter().find(|r| r.step.id == step) else {
        return PaneOutcome::Ignored;
    };
    PaneOutcome::Focus(FocusRequest {
        agent: resolve(&row.agent),
        pane: focus_pane,
        step: Some(row.step.id.clone()),
    })
}

/// The pane.
pub struct TimelinePane {
    pub cfg: Arc<TimelineConfig>,
    pub state: Mutex<TimelineState>,
    ledger: Option<LedgerHandle>,
    /// The `agents` registry, IF this row declared and was given one. A row whose agent has no
    /// live handle still focuses its step, which is why the key is optional — but it is DECLARED,
    /// and resolved through this row's own committed view (§0.3).
    agents: Option<Arc<AgentsHandle>>,
    /// The ROW's context, so the reload is one of its effects. `None` in a test that drives the
    /// pane without a composed tree.
    ctx: Option<bough_kernel::Context>,
}

impl TimelinePane {
    pub fn new(cfg: Arc<TimelineConfig>) -> TimelinePane {
        let state = Mutex::new(TimelineState::new(&cfg));
        TimelinePane {
            cfg,
            state,
            ledger: None,
            agents: None,
            ctx: None,
        }
    }

    pub fn with_ledger(mut self, ledger: LedgerHandle) -> TimelinePane {
        self.ledger = Some(ledger);
        self
    }

    pub fn with_agents(mut self, agents: Option<Arc<AgentsHandle>>) -> TimelinePane {
        self.agents = agents;
        self
    }

    pub fn with_ctx(mut self, ctx: bough_kernel::Context) -> TimelinePane {
        self.ctx = Some(ctx);
        self
    }

    /// Name → live id, through THIS row's declared optional `agents` handle.
    fn resolve(&self, name: &AgentName) -> Option<AgentId> {
        self.agents
            .as_ref()
            .and_then(|a| a.by_name(name))
            .map(|a| a.id().clone())
    }

    /// Reload the rows. The ONE read in the crate, and it is never called from `render`.
    ///
    /// The read is an EFFECT of the row (`ctx.effect_spawn`), not a bare `tokio::spawn`: a reload
    /// armed a moment before this row is disabled would otherwise still query the ledger and call
    /// `redraw()` after the pane was gone.
    pub fn reload(self: &Arc<Self>, tui: TuiHandle) {
        let (Some(ctx), Some(ledger)) = (self.ctx.as_ref(), self.ledger.clone()) else {
            // No row context or no ledger: a test drove the pane directly and supplies its own
            // rows. There is nothing to arm.
            return;
        };
        {
            let mut st = self.state.lock();
            if st.loading {
                return;
            }
            st.loading = true;
        }
        let me = Arc::clone(self);
        let cfg = Arc::clone(&self.cfg);
        let ctx = ctx.clone();
        ctx.clone().effect_spawn(move |ectx| async move {
            tokio::time::sleep(std::time::Duration::from_millis(cfg.debounce_ms)).await;
            if ectx.checkpoint().await.is_err() {
                me.state.lock().loading = false;
                return Ok(());
            }
            let filter = me.state.lock().filter.clone();
            let read = load_rows(&ledger, &cfg, &filter).await;
            {
                let mut st = me.state.lock();
                st.loading = false;
                st.loaded_at = Some(chrono::Utc::now());
                match read {
                    Ok(loaded) => st.apply(loaded),
                    // A failed read is REPORTED on the header, never a silently empty timeline.
                    Err(e) => st.error = Some(e.to_string()),
                }
            }
            tui.redraw();
            Ok(())
        });
    }
}

#[async_trait::async_trait]
impl Pane for TimelinePane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let mut state = self.state.lock();
        let area = cx.area;
        state.height = area.height;
        let painted = state.lines(area.width);
        // The invariant's recorder: what this frame ACTUALLY put on screen, and the set the query
        // behind it returned.
        crate::invariant::record(&state.visible(), &state.queried);
        let theme = *cx.theme();
        let selected_line = state.selected + 1 + usize::from(state.error.is_some());
        let top = state.top(painted.len(), area.height);
        let mut out: Vec<ratatui::text::Line> =
            Vec::with_capacity(painted.len() - top.min(painted.len()));
        for (i, (text, hit)) in painted.into_iter().enumerate() {
            if i < top {
                continue;
            }
            let style = if i == 0 {
                ratatui::style::Style::default().fg(theme.dim)
            } else if state.error.is_some() && i == 1 {
                ratatui::style::Style::default().fg(theme.error)
            } else if i == selected_line && cx.view.is_focused {
                ratatui::style::Style::default().fg(theme.accent)
            } else {
                ratatui::style::Style::default().fg(theme.fg)
            };
            out.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                text, style,
            )));
            if let (Some(hit), Some(rect)) = (hit, row_rect(area, i - top)) {
                cx.hit(rect, hit);
            }
        }
        let row_focus = Some(state.selected);
        drop(state);
        cx.report_rows(row_focus, true);
        cx.frame.render_widget(Paragraph::new(out), area);
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        // The pane is only ever held as `Arc<dyn Pane>`; the reload needs an owned `Arc<Self>`, so
        // the real body lives on `TimelinePaneArc`. Reaching here means someone held a bare
        // `&TimelinePane` (the `tui-search` precedent).
        PaneOutcome::Ignored
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

/// The registered pane: an `Arc<TimelinePane>` so `handle` can arm a reload that owns the pane.
pub struct TimelinePaneArc(pub Arc<TimelinePane>);

#[async_trait::async_trait]
impl Pane for TimelinePaneArc {
    fn render(&self, cx: &mut RenderCx<'_>) {
        self.0.render(cx)
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        use crossterm::event::{KeyCode, KeyModifiers};
        match ev {
            PaneEvent::Key(key) => {
                let mut st = self.0.state.lock();
                match key.code {
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        st.push_char(c);
                    }
                    KeyCode::Backspace => st.backspace(),
                    KeyCode::Esc => {
                        let outcome = st.escape();
                        drop(st);
                        return match outcome {
                            // Step one: the editor cleared and the pane stays. The rows are now
                            // under the empty filter, so the screen has to repaint.
                            Escape::Cleared => {
                                cx.tui.redraw();
                                self.0.reload(cx.tui.clone());
                                PaneOutcome::Handled
                            }
                            // Step two: not ours — the shell dismisses the pane (ux1's Esc rule).
                            Escape::Dismiss => PaneOutcome::Ignored,
                        };
                    }
                    KeyCode::Up => {
                        st.move_selection(-1);
                        drop(st);
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Down => {
                        st.move_selection(1);
                        drop(st);
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::PageUp => {
                        let page = st.height.max(1) as i32;
                        st.move_selection(-page);
                        drop(st);
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::PageDown => {
                        let page = st.height.max(1) as i32;
                        st.move_selection(page);
                        drop(st);
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Enter => {
                        // `Enter` on a typed filter APPLIES it; `Enter` on an untouched editor
                        // focuses the selected row. The editor is where the keystrokes went, so
                        // it is what `Enter` is about while it differs from the live filter.
                        let typed = parse_filter(&st.input, cx.at).ok() != Some(st.filter.clone());
                        if typed {
                            let _ = st.submit(cx.at);
                            drop(st);
                            cx.tui.redraw();
                            self.0.reload(cx.tui.clone());
                            return PaneOutcome::Handled;
                        }
                        let hit = st.selected_hit();
                        let rows = st.visible();
                        drop(st);
                        return click(&self.0, hit.as_ref(), &rows, &cx);
                    }
                    _ => return PaneOutcome::Ignored,
                }
                drop(st);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::Click { hit, .. } => {
                let rows = self.0.state.lock().visible();
                click(&self.0, hit.as_ref(), &rows, &cx)
            }
            PaneEvent::Scroll { delta } => {
                let mut st = self.0.state.lock();
                let painted = st.lines(0).len();
                st.scroll_by(delta as i32, painted);
                drop(st);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            // Gaining focus is the moment a fresh read can be relied on: the keyboard can leave
            // this pane by paths that never reach it.
            PaneEvent::FocusChanged(true) => {
                self.0.reload(cx.tui.clone());
                PaneOutcome::Handled
            }
            PaneEvent::Tick => {
                if self.0.state.lock().due(cx.at, self.0.cfg.debounce_ms) {
                    self.0.reload(cx.tui.clone());
                }
                PaneOutcome::Ignored
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        self.0.key_hints()
    }
}

fn click(pane: &Arc<TimelinePane>, hit: Option<&HitId>, rows: &[Row], cx: &PaneCx) -> PaneOutcome {
    let focus_pane = cx
        .tui
        .panes()
        .into_iter()
        .find(|p| p.slot == Slot::Main)
        .map(|p| p.id);
    on_click(hit, rows, focus_pane, |name| pane.resolve(name))
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
    use crate::testing::{config, row};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn state() -> TimelineState {
        let mut st = TimelineState::new(&config());
        st.rows = vec![
            row("sol", "t1", 1, "wake/start", "12:00:00"),
            row("terra", "t2", 1, "tool/call", "12:00:01"),
        ];
        st.queried = st.rows.iter().map(|r| r.step.id.clone()).collect();
        st
    }

    #[test]
    fn esc_clears_the_editor_first_and_dismisses_second() {
        let mut st = state();
        st.push_char('a');
        assert_eq!(st.escape(), Escape::Cleared);
        assert!(st.input.is_empty());
        assert_eq!(st.escape(), Escape::Dismiss);
    }

    #[test]
    fn a_parse_error_keeps_the_previous_filter_live() {
        let mut st = state();
        st.input = "agent:sol".to_string();
        st.submit(now()).expect("well-formed");
        st.input = "wombat:7".to_string();
        st.submit(now()).expect_err("not a filter");
        assert_eq!(st.filter, parse_filter("agent:sol", now()).unwrap());
        assert_eq!(st.visible().len(), 1);
        let lines = st.lines(120);
        assert!(lines[0].0.contains("agent:sol"), "{:?}", lines[0].0);
        assert!(lines[1].0.contains("wombat:7"), "{:?}", lines[1].0);
    }

    #[test]
    fn a_click_on_a_row_focuses_that_step() {
        let st = state();
        let visible = st.visible();
        let hit = hit_of(&visible[1]);
        match on_click(Some(&hit), &visible, None, |_| None) {
            PaneOutcome::Focus(req) => assert_eq!(req.step, Some(visible[1].step.id.clone())),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            on_click(Some(&HitId::new("hit:x")), &visible, None, |_| None),
            PaneOutcome::Ignored
        );
        assert_eq!(
            on_click(None, &visible, None, |_| None),
            PaneOutcome::Ignored
        );
    }

    #[test]
    fn every_visible_row_is_clickable_and_the_header_is_not() {
        let st = state();
        let lines = st.lines(120);
        assert_eq!(lines[0].1, None, "the header is not a row");
        assert_eq!(lines.len(), 1 + st.visible().len());
        for (i, r) in st.visible().iter().enumerate() {
            assert_eq!(lines[i + 1].1.as_ref(), Some(&hit_of(r)));
        }
    }

    #[test]
    fn the_header_says_when_the_window_was_full() {
        let mut st = state();
        assert!(!header(&st, 120).contains("steps/agent"));
        st.windowed = true;
        let head = header(&st, 120);
        assert!(
            head.contains(&format!("newest {} steps/agent", st.window)),
            "{head}"
        );
    }

    #[test]
    fn the_row_focus_clamps_at_both_ends() {
        let mut st = state();
        st.height = 10;
        st.move_selection(-1);
        assert_eq!(st.selected, 0);
        st.move_selection(50);
        assert_eq!(st.selected, st.visible().len() - 1);
    }
}
