//! Invariant: `render` is a PURE function of the `Snapshot` the pane already holds. Every read of
//! the projection seam happens in `refresh`, on a tick or on a debounced `ledger/step` — never in
//! a frame (§11's render rule).
//!
//! `Esc` is the ux1 rule: the pane holds no editor, so `Esc` has nothing of its own to clear and
//! goes straight through to the shell, which dismisses the overlay.

use std::sync::Arc;

use bough_plugin_ledger::{AgentName, LedgerHandle};
use bough_plugin_projection::ProjectionHandle;
use bough_plugin_tui_shell::pane::{Pane, PaneCx, PaneEvent, PaneOutcome, RenderCx};
use bough_plugin_tui_shell::TuiHandle;
use parking_lot::Mutex;
use ratatui::widgets::Paragraph;

use crate::delta::WAKE_PREFACE_KINDS;
use crate::snapshot::{snapshot, PreviewAt, Snapshot};
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
    /// A refresh is armed or running. One at a time: `assemble` is deterministic but NOT free,
    /// and a pane that armed one per tick starved the wake it was previewing.
    pub refreshing: bool,
    /// When the last refresh landed. A tick sooner than `refresh_ms` after it is not due.
    pub refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl PreviewState {
    /// A fresh state in [`PreviewAt::Head`].
    pub fn new() -> PreviewState {
        PreviewState {
            mode: Some(PreviewAt::Head),
            ..Default::default()
        }
    }

    /// The mode the next refresh assembles in. `Head` when nothing chose one.
    pub fn mode(&self) -> PreviewAt {
        self.mode.clone().unwrap_or(PreviewAt::Head)
    }

    /// `t`: Head ⇄ the `as_of` the last snapshot was taken at. Toggling with no snapshot in hand
    /// leaves Head, because there is no anchor to hold yet.
    pub fn toggle(&mut self) {
        self.mode = Some(match self.mode() {
            PreviewAt::Head => match &self.snapshot {
                Some(s) => PreviewAt::Seq(s.as_of),
                None => PreviewAt::Head,
            },
            PreviewAt::Seq(_) => PreviewAt::Head,
        });
    }

    /// Land a refresh.
    pub fn apply(&mut self, snap: Snapshot) {
        self.mode = Some(snap.at.clone());
        self.refreshed_at = Some(snap.taken_at);
        self.snapshot = Some(snap);
        self.error = None;
        self.scroll = 0;
    }

    /// PURE: the first painted line of the viewport, clamped to what there is to show.
    pub fn top(&self, painted: usize, height: u16) -> usize {
        self.scroll.min(painted.saturating_sub(height as usize))
    }

    pub fn scroll_by(&mut self, delta: i32, painted: usize) {
        let max = painted.saturating_sub(self.height.max(1) as usize) as i64;
        let to = (self.scroll as i64 + i64::from(delta)).clamp(0, max.max(0));
        self.scroll = to as usize;
    }

    /// Whether a tick should take a new snapshot: none in flight, and `refresh_ms` since the last.
    pub fn due(&self, now: chrono::DateTime<chrono::Utc>, refresh_ms: u64) -> bool {
        if self.refreshing {
            return false;
        }
        match self.refreshed_at {
            None => true,
            Some(then) => (now - then).num_milliseconds() >= refresh_ms as i64,
        }
    }

    /// The bytes `y` copies: the WHOLE prefix, not the visible window.
    pub fn copy_text(&self) -> Option<String> {
        self.snapshot.as_ref().map(|s| s.text.clone())
    }
}

/// The caveat the header states for a mode: how many preface rows a real wake would add on top of
/// what is on screen. `Seq` is anchored and adds none — that is the mode V1 asserts (D-C1).
pub fn preface_rows(at: &PreviewAt) -> usize {
    match at {
        PreviewAt::Head => WAKE_PREFACE_KINDS.len(),
        PreviewAt::Seq(_) => 0,
    }
}

/// PURE: the header line.
/// `preview · <agent> · <mode> as_of <seq> · <tokens>/<budget> tok · <digest[..8]> · +N preface rows at wake`
pub fn header(state: &PreviewState, preface_rows: usize, cols: u16) -> String {
    let mut text = match &state.snapshot {
        None => "preview \u{b7} nothing taken yet".to_string(),
        Some(s) => {
            // The MODE is the state's, not the snapshot's: `t` changes what the next refresh will
            // assemble, and a header still claiming the old mode would be a lie for one frame.
            let mut t = format!(
                "preview \u{b7} {} \u{b7} {} as_of {} \u{b7} {}/{} tok \u{b7} {}",
                s.agent,
                state.mode().word(),
                s.as_of.0,
                s.tokens,
                s.budget,
                &s.digest[..8.min(s.digest.len())],
            );
            if preface_rows > 0 {
                // §16: Head is NOT byte-exact against the next wake, and says so here rather than
                // letting the reader assume an exactness this seam cannot give.
                t.push_str(&format!(" \u{b7} +{preface_rows} preface rows at wake"));
            }
            t
        }
    };
    let cols = cols as usize;
    if text.chars().count() > cols {
        text = text.chars().take(cols).collect();
    }
    text
}

/// PURE: the plain-text lines the pane paints, clipped to [`PreviewConfig::max_chars`]. A clipped
/// preview SAYS it was clipped: a truncated surface that looks whole is the lie §16 forbids.
pub fn lines(state: &PreviewState, cfg: &PreviewConfig, cols: u16) -> Vec<String> {
    let mut out = vec![header(state, preface_rows(&state.mode()), cols)];
    if let Some(err) = &state.error {
        out.push(clip(&format!("preview failed: {err}"), cols));
    }
    let Some(snap) = &state.snapshot else {
        return out;
    };
    let mut spent = 0usize;
    let mut clipped = false;
    for line in snap.text.lines() {
        let n = line.chars().count();
        if spent + n > cfg.max_chars {
            clipped = true;
            break;
        }
        spent += n;
        out.push(clip(line, cols));
    }
    if clipped {
        out.push(clip(
            &format!(
                "\u{2026} clipped at {} characters; `y` copies the whole prefix",
                cfg.max_chars
            ),
            cols,
        ));
    }
    out
}

fn clip(text: &str, cols: u16) -> String {
    if text.chars().count() <= cols as usize {
        return text.to_string();
    }
    text.chars().take(cols as usize).collect()
}

/// What a key meant. PURE, so the pane's key handling is testable without a `PaneCx`, which is
/// only constructible inside `tui-shell` (the `tui-search` precedent, D-WP5-1).
#[derive(Clone, Debug, PartialEq)]
pub enum KeyAction {
    /// The state changed; repaint.
    Redraw,
    /// Repaint AND take a fresh snapshot (the mode changed).
    Refresh,
    /// Put this text on the clipboard.
    Copy(String),
    /// Not ours: the shell dismisses the pane.
    Dismiss,
    Ignored,
}

/// PURE: what one key does to the state.
pub fn on_key(
    key: crossterm::event::KeyEvent,
    state: &mut PreviewState,
    painted: usize,
) -> KeyAction {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => KeyAction::Dismiss,
        KeyCode::Up => {
            state.scroll_by(-1, painted);
            KeyAction::Redraw
        }
        KeyCode::Down => {
            state.scroll_by(1, painted);
            KeyAction::Redraw
        }
        KeyCode::PageUp => {
            state.scroll_by(-(state.height.max(1) as i32), painted);
            KeyAction::Redraw
        }
        KeyCode::PageDown => {
            state.scroll_by(state.height.max(1) as i32, painted);
            KeyAction::Redraw
        }
        KeyCode::Char('t') => {
            state.toggle();
            KeyAction::Refresh
        }
        KeyCode::Char('y') => match state.copy_text() {
            Some(text) => KeyAction::Copy(text),
            None => KeyAction::Ignored,
        },
        _ => KeyAction::Ignored,
    }
}

/// The pane.
pub struct PreviewPane {
    pub cfg: Arc<PreviewConfig>,
    pub state: Mutex<PreviewState>,
    projection: Option<ProjectionHandle>,
    ledger: Option<LedgerHandle>,
    /// The ROW's context, so the refresh is one of its effects. `None` in a test that drives the
    /// pane without a composed tree.
    ctx: Option<bough_kernel::Context>,
}

impl PreviewPane {
    pub fn new(cfg: Arc<PreviewConfig>) -> PreviewPane {
        PreviewPane {
            cfg,
            state: Mutex::new(PreviewState::new()),
            projection: None,
            ledger: None,
            ctx: None,
        }
    }

    pub fn with_seams(mut self, projection: ProjectionHandle, ledger: LedgerHandle) -> PreviewPane {
        self.projection = Some(projection);
        self.ledger = Some(ledger);
        self
    }

    pub fn with_ctx(mut self, ctx: bough_kernel::Context) -> PreviewPane {
        self.ctx = Some(ctx);
        self
    }

    /// Take a fresh snapshot for `agent`. The ONE read in the crate, and it is never called from
    /// `render`. It is an EFFECT of the row, so a refresh armed a moment before this row is
    /// disabled cannot still call the seam and `redraw()` after the pane is gone.
    pub fn refresh(self: &Arc<Self>, tui: TuiHandle, agent: AgentName) {
        let (Some(ctx), Some(projection), Some(ledger)) = (
            self.ctx.as_ref(),
            self.projection.clone(),
            self.ledger.clone(),
        ) else {
            return;
        };
        {
            // One refresh at a time. Without this the pane armed an `assemble` on every tick.
            let mut st = self.state.lock();
            if st.refreshing {
                return;
            }
            st.refreshing = true;
        }
        let me = Arc::clone(self);
        let cfg = Arc::clone(&self.cfg);
        let ctx = ctx.clone();
        ctx.clone().effect_spawn(move |ectx| async move {
            tokio::time::sleep(std::time::Duration::from_millis(cfg.refresh_ms)).await;
            if ectx.checkpoint().await.is_err() {
                me.state.lock().refreshing = false;
                return Ok(());
            }
            let at = me.state.lock().mode();
            let taken = snapshot(&projection, &ledger, &agent, at, chrono::Utc::now()).await;
            {
                let mut st = me.state.lock();
                st.refreshing = false;
                match taken {
                    Ok(snap) => st.apply(snap),
                    // A failed read is REPORTED on the pane, never a silently blank preview (§16).
                    Err(e) => {
                        st.error = Some(e.to_string());
                        st.refreshed_at = Some(chrono::Utc::now());
                    }
                }
            }
            tui.redraw();
            Ok(())
        });
    }
}

#[async_trait::async_trait]
impl Pane for PreviewPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let mut state = self.state.lock();
        let area = cx.area;
        state.height = area.height;
        let painted = lines(&state, &self.cfg, area.width);
        if let Some(snap) = &state.snapshot {
            // The invariant's recorder: what this frame ACTUALLY put on screen.
            crate::invariant::record(snap.as_of, &snap.digest);
        }
        let theme = *cx.theme();
        let top = state.top(painted.len(), area.height);
        let mut out: Vec<ratatui::text::Line> = Vec::new();
        for (i, text) in painted.into_iter().enumerate() {
            if i < top {
                continue;
            }
            let style = if i == 0 {
                ratatui::style::Style::default().fg(theme.dim)
            } else if state.error.is_some() && i == 1 {
                ratatui::style::Style::default().fg(theme.error)
            } else {
                ratatui::style::Style::default().fg(theme.fg)
            };
            out.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                text, style,
            )));
        }
        drop(state);
        cx.frame.render_widget(Paragraph::new(out), area);
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        // The pane is only ever held as `Arc<dyn Pane>`; the refresh needs an owned `Arc<Self>`,
        // so the real body lives on `PreviewPaneArc` (the `tui-search` precedent).
        PaneOutcome::Ignored
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

/// The registered pane: an `Arc<PreviewPane>` so `handle` can arm a refresh that owns the pane.
pub struct PreviewPaneArc(pub Arc<PreviewPane>);

#[async_trait::async_trait]
impl Pane for PreviewPaneArc {
    fn render(&self, cx: &mut RenderCx<'_>) {
        self.0.render(cx)
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Key(key) => {
                let painted = {
                    let st = self.0.state.lock();
                    let cols = cx.tui.size().width;
                    lines(&st, &self.0.cfg, cols).len()
                };
                let action = {
                    let mut st = self.0.state.lock();
                    on_key(key, &mut st, painted)
                };
                match action {
                    KeyAction::Redraw => {
                        cx.tui.redraw();
                        PaneOutcome::Handled
                    }
                    KeyAction::Refresh => {
                        if let Some(agent) = cx.agent.as_ref().map(|a| a.name().clone()) {
                            self.0.refresh(cx.tui.clone(), agent);
                        }
                        cx.tui.redraw();
                        PaneOutcome::Handled
                    }
                    KeyAction::Copy(text) => {
                        cx.tui.copy(&text).await;
                        PaneOutcome::Handled
                    }
                    KeyAction::Dismiss | KeyAction::Ignored => PaneOutcome::Ignored,
                }
            }
            PaneEvent::Scroll { delta } => {
                let painted = {
                    let st = self.0.state.lock();
                    lines(&st, &self.0.cfg, cx.tui.size().width).len()
                };
                self.0.state.lock().scroll_by(i32::from(delta), painted);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::Focus(_) | PaneEvent::FocusChanged(true) => {
                // A new focused agent is a new preview: the pane's whole claim is that it shows
                // what THIS agent would be given. The debounce still holds — one at a time.
                self.0.state.lock().refreshed_at = None;
                if let Some(agent) = cx.agent.as_ref().map(|a| a.name().clone()) {
                    self.0.refresh(cx.tui.clone(), agent);
                }
                PaneOutcome::Ignored
            }
            PaneEvent::Tick => {
                if self.0.state.lock().due(cx.at, self.0.cfg.refresh_ms) {
                    if let Some(agent) = cx.agent.as_ref().map(|a| a.name().clone()) {
                        self.0.refresh(cx.tui.clone(), agent);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::Seq;
    use crossterm::event::{KeyCode, KeyEvent};

    fn cfg() -> PreviewConfig {
        PreviewConfig {
            height: 10,
            collapse_rows: 20,
            min_rows: 3,
            max_rows: 30,
            refresh_ms: 0,
            max_chars: 10_000,
        }
    }

    fn snap(text: &str) -> Snapshot {
        Snapshot {
            agent: AgentName::new("sol"),
            at: PreviewAt::Seq(Seq(7)),
            as_of: Seq(7),
            text: text.to_string(),
            tokens: 12,
            budget: 100,
            flags: Default::default(),
            sections: Vec::new(),
            digest: crate::snapshot::digest(text),
            taken_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn the_header_states_the_head_delta_and_the_anchored_mode_states_none() {
        let mut st = PreviewState::new();
        st.apply(snap("a\nb"));
        let anchored = header(&st, preface_rows(&st.mode()), 200);
        assert!(anchored.contains("anchored"), "{anchored}");
        assert!(!anchored.contains("preface"), "{anchored}");
        st.mode = Some(PreviewAt::Head);
        let head = header(&st, preface_rows(&st.mode()), 200);
        assert!(head.contains("head"), "{head}");
        assert!(head.contains("+3 preface rows"), "{head}");
    }

    #[test]
    fn a_clipped_preview_says_it_was_clipped() {
        let mut c = cfg();
        c.max_chars = 3;
        let mut st = PreviewState::new();
        st.apply(snap("abc\ndef"));
        let painted = lines(&st, &c, 200);
        assert!(
            painted.iter().any(|l| l.contains("clipped")),
            "{painted:#?}"
        );
        assert!(!painted.iter().any(|l| l == "def"), "{painted:#?}");
    }

    #[test]
    fn render_is_a_pure_function_of_the_held_snapshot() {
        let mut st = PreviewState::new();
        st.apply(snap("a\nb\nc"));
        assert_eq!(lines(&st, &cfg(), 80), lines(&st, &cfg(), 80));
    }

    #[test]
    fn esc_is_not_the_panes_and_the_shell_dismisses() {
        let mut st = PreviewState::new();
        let action = on_key(KeyEvent::from(KeyCode::Esc), &mut st, 3);
        assert_eq!(action, KeyAction::Dismiss);
    }

    #[test]
    fn t_toggles_head_and_anchored_and_asks_for_a_fresh_snapshot() {
        let mut st = PreviewState::new();
        st.apply(snap("a"));
        st.mode = Some(PreviewAt::Head);
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('t')), &mut st, 2),
            KeyAction::Refresh
        );
        assert_eq!(st.mode(), PreviewAt::Seq(Seq(7)));
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('t')), &mut st, 2),
            KeyAction::Refresh
        );
        assert_eq!(st.mode(), PreviewAt::Head);
    }

    #[test]
    fn y_copies_the_whole_prefix_not_the_visible_window() {
        let mut st = PreviewState::new();
        st.height = 1;
        st.apply(snap("a\nb\nc"));
        let action = on_key(KeyEvent::from(KeyCode::Char('y')), &mut st, 4);
        assert_eq!(action, KeyAction::Copy("a\nb\nc".to_string()));
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let mut st = PreviewState::new();
        st.height = 2;
        st.apply(snap("a\nb\nc\nd"));
        on_key(KeyEvent::from(KeyCode::Up), &mut st, 5);
        assert_eq!(st.scroll, 0);
        for _ in 0..20 {
            on_key(KeyEvent::from(KeyCode::Down), &mut st, 5);
        }
        assert_eq!(st.scroll, 3);
    }
}
