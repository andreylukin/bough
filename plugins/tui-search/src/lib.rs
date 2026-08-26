//! Invariant: this row is the SWAP subject and is deliberately self-contained. Nothing else in the
//! tree may depend on it, and disabling it by patch must be indistinguishable from never having
//! mounted it — no pane, no listener, no binding left behind (§17 Phase 3).
//!
//! It reads `ctx.ledger` and never `ctx.agents`' loop: a hit is a step id, and clicking one is a
//! `FocusRequest`, not a wake.
//!
//! Everything a test needs is a PURE function over state the pane already holds: `RenderCx`,
//! `PaneCx` and `TuiHandle` are only constructible inside `tui-shell`, so `render` and `handle`
//! are thin shells over `hit_rows`, `hit_line`, `SearchState` and `on_click` (deviation D-WP5-1,
//! reported at the seam).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{AgentId, Agents, AgentsHandle};
use bough_plugin_ledger::{
    AgentName, AgentRow, Ledger, LedgerHandle, SearchHit, SearchQuery, Seq, StepId, StepType,
    TrajId,
};
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneId, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{FocusRequest, HitId, Tui, TuiHandle};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-search";

/// The pane id this row registers under. Fixed, because `Ctrl+F` names it.
pub const PANE_ID: &str = "tui.search";

/// The `HitId` convention for a hit row (`pane.rs`: `hit:<step_id>`).
pub const HIT_PREFIX: &str = "hit:";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    pub height: u16,
    pub limit: usize,
    pub debounce_ms: u64,
}

/// One rendered hit.
#[derive(Clone, Debug, PartialEq)]
pub struct HitRow {
    /// Resolved traj → `agents` row; `None` for a rowless traj.
    pub agent: Option<AgentName>,
    pub traj: TrajId,
    pub step: StepId,
    pub seq: Seq,
    pub kind: StepType,
    pub snippet: String,
}

/// PURE: `SearchHit` + the agents rows ⇒ display rows (agent, seq, kind, snippet).
pub fn hit_rows(hits: &[SearchHit], agents: &[AgentRow]) -> Vec<HitRow> {
    hits.iter()
        .map(|h| HitRow {
            agent: agents
                .iter()
                .find(|a| a.traj == h.step.traj)
                .map(|a| a.name.clone()),
            traj: h.step.traj.clone(),
            step: h.step.id.clone(),
            seq: h.step.seq,
            kind: h.step.kind.clone(),
            snippet: one_line(&h.snippet),
        })
        .collect()
}

/// PURE: the one line a hit row draws. A rowless trajectory renders NO agent name — not an empty
/// column and not the trajectory id dressed up as one.
pub fn hit_line(row: &HitRow) -> String {
    let mut out = String::new();
    if let Some(agent) = &row.agent {
        out.push_str(agent.as_str());
        out.push(' ');
    }
    out.push_str(&format!("s{} {}  {}", row.seq.0, row.kind, row.snippet));
    out
}

/// The `HitId` for a hit row.
pub fn hit_id(step: &StepId) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{step}"))
}

/// The step a `HitId` names, when it is one of ours.
pub fn step_of_hit(hit: &HitId) -> Option<StepId> {
    hit.as_str().strip_prefix(HIT_PREFIX).map(StepId::new)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The debounce, as a pure generation counter over a clock passed in.
///
/// A burst of keystrokes bumps the generation on each one; the timer armed for keystroke *n* only
/// fires a query when its generation is still the current one, so a burst collapses into ONE
/// query and never into `n` racing ones.
#[derive(Clone, Debug)]
pub struct Debounce {
    window_ms: u64,
    generation: u64,
    last_input: Option<DateTime<Utc>>,
}

impl Debounce {
    pub fn new(window_ms: u64) -> Debounce {
        Debounce {
            window_ms,
            generation: 0,
            last_input: None,
        }
    }

    /// A keystroke. Returns the generation the caller's timer must present back.
    pub fn on_input(&mut self, now: DateTime<Utc>) -> u64 {
        self.generation += 1;
        self.last_input = Some(now);
        self.generation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Is the timer armed for `generation` the one that should run?
    pub fn due(&self, generation: u64, now: DateTime<Utc>) -> bool {
        if generation != self.generation {
            return false;
        }
        match self.last_input {
            None => false,
            Some(t) => (now - t).num_milliseconds() >= self.window_ms as i64,
        }
    }
}

/// Everything the pane holds between frames. `render` is a pure function of it.
#[derive(Debug)]
pub struct SearchState {
    /// The one-line input the pane owns (the composer belongs to the shell).
    pub input: String,
    pub rows: Vec<HitRow>,
    /// Set by a failed query; rendered inline in `theme.error`, and the list is empty beside it.
    pub error: Option<String>,
    pub debounce: Debounce,
    /// Index into `rows` under the keyboard.
    pub selected: usize,
    /// First PAINTED line of the viewport. The pane paints `1 + rows.len()` lines and its slot is
    /// a dozen cells: without this, everything past the twelfth hit was invisible AND unclickable
    /// while Up/Down moved a selection nobody could see.
    pub scroll: usize,
    /// The viewport height of the LAST frame. `handle` has no `area`, and clamping the scroll
    /// needs one.
    pub height: u16,
}

impl SearchState {
    pub fn new(cfg: &SearchConfig) -> SearchState {
        SearchState {
            input: String::new(),
            rows: Vec::new(),
            error: None,
            debounce: Debounce::new(cfg.debounce_ms),
            selected: 0,
            scroll: 0,
            height: 0,
        }
    }

    /// PURE: the first painted line of the viewport, clamped to what there is to show.
    pub fn top(&self, painted: usize, height: u16) -> usize {
        self.scroll.min(painted.saturating_sub(height as usize))
    }

    /// Scroll by `delta` painted lines, clamped.
    pub fn scroll_by(&mut self, delta: i32, painted: usize) {
        let max = painted.saturating_sub(self.height as usize) as i64;
        let to = (self.scroll as i64 + delta as i64).clamp(0, max.max(0));
        self.scroll = to as usize;
    }

    /// Keep the selected hit inside the viewport. The selected hit is painted line
    /// `selected + 1` (line 0 is the prompt).
    pub fn follow_selection(&mut self) {
        let line = self.selected + 1;
        let height = self.height.max(1) as usize;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + height {
            self.scroll = line + 1 - height;
        }
    }

    /// A typed character. Returns the generation a timer must present back to `apply`.
    pub fn push_char(&mut self, c: char, now: DateTime<Utc>) -> u64 {
        self.input.push(c);
        self.debounce.on_input(now)
    }

    pub fn backspace(&mut self, now: DateTime<Utc>) -> u64 {
        self.input.pop();
        self.debounce.on_input(now)
    }

    pub fn clear_input(&mut self, now: DateTime<Utc>) -> u64 {
        self.input.clear();
        self.rows.clear();
        self.error = None;
        self.debounce.on_input(now)
    }

    /// Land a query result. A STALE generation is dropped: a slow query for an older keystroke can
    /// never overwrite the answer to what is on screen now. Returns whether anything changed.
    pub fn apply(&mut self, generation: u64, result: Result<Vec<HitRow>, String>) -> bool {
        if generation != self.debounce.generation() {
            return false;
        }
        match result {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
            }
            Err(msg) => {
                // An FTS syntax error renders inline AND clears the list: a stale result list
                // beside a fresh error would be a lie about what matched.
                self.rows.clear();
                self.error = Some(msg);
            }
        }
        self.selected = 0;
        self.scroll = 0;
        true
    }
}

/// PURE: the lines the pane paints, each with the `HitId` its row is clickable under.
pub fn lines(state: &SearchState, prompt_focused: bool) -> Vec<(String, Option<HitId>)> {
    let cursor = if prompt_focused { "_" } else { "" };
    // The prompt NAMES itself. A bare "/" was indistinguishable from the composer's command
    // prefix on screen, so neither a reader nor V4's assertion could tell which line was the
    // search pane; the label is the pane's only chrome.
    let mut out = vec![(format!("search / {}{}", state.input, cursor), None)];
    if let Some(err) = &state.error {
        out.push((format!("! {err}"), None));
        return out;
    }
    for row in &state.rows {
        out.push((hit_line(row), Some(hit_id(&row.step))));
    }
    out
}

/// PURE: what a click on a recorded hit means. A hit is a step id, so the outcome is a
/// `FocusRequest` — never a wake.
pub fn on_click(
    hit: Option<&HitId>,
    rows: &[HitRow],
    focus_pane: Option<PaneId>,
    resolve: impl Fn(&AgentName) -> Option<AgentId>,
) -> PaneOutcome {
    let Some(hit) = hit else {
        return PaneOutcome::Ignored;
    };
    let Some(step) = step_of_hit(hit) else {
        return PaneOutcome::Ignored;
    };
    let Some(row) = rows.iter().find(|r| r.step == step) else {
        return PaneOutcome::Ignored;
    };
    PaneOutcome::Focus(FocusRequest {
        agent: row.agent.as_ref().and_then(&resolve),
        pane: focus_pane,
        step: Some(row.step.clone()),
    })
}

/// Run one query. The only I/O in the crate, and it is never called from `render`.
pub async fn run_query(
    ledger: &LedgerHandle,
    cfg: &SearchConfig,
    text: &str,
) -> Result<Vec<HitRow>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let hits = ledger
        .0
        .search(&SearchQuery {
            text: text.to_string(),
            trajs: Vec::new(),
            limit: cfg.limit,
        })
        .await
        .map_err(|e| e.to_string())?;
    let agents = ledger.0.agents().await.map_err(|e| e.to_string())?;
    Ok(hit_rows(&hits, &agents))
}

/// The pane: a one-line input it owns, debounced, over `LedgerStore::search`.
pub struct SearchPane {
    cfg: Arc<SearchConfig>,
    ledger: LedgerHandle,
    /// The `agents` registry, IF this row declared and was given one (`Inject::optional`). A hit
    /// whose agent has no live handle still focuses its step, which is why the key is optional —
    /// but it is DECLARED, and resolved through this row's own committed view (§0.3). It used to
    /// be read with `ctx.peek_live` off the shell's context, which is a capability escape.
    agents: Option<Arc<AgentsHandle>>,
    /// The ROW's context, so the debounce timer is one of its effects. `None` in a test that
    /// drives the pane without a composed tree.
    ctx: Option<Context>,
    pub state: Mutex<SearchState>,
}

impl SearchPane {
    pub fn new(cfg: Arc<SearchConfig>, ledger: LedgerHandle) -> SearchPane {
        let state = Mutex::new(SearchState::new(&cfg));
        SearchPane {
            cfg,
            ledger,
            agents: None,
            ctx: None,
            state,
        }
    }

    /// Attach the row's context, which owns the debounce timer.
    pub fn with_ctx(mut self, ctx: Context) -> SearchPane {
        self.ctx = Some(ctx);
        self
    }

    /// Attach the `agents` handle this row injected.
    pub fn with_agents(mut self, agents: Option<Arc<AgentsHandle>>) -> SearchPane {
        self.agents = agents;
        self
    }

    /// Arm the debounce timer for `generation`; the query runs only if it is still current.
    ///
    /// The timer is an EFFECT of the row (`ctx.effect_spawn`), not a bare `tokio::spawn`: this is
    /// the row the SWAP gate disables, and a query armed a moment before the disable would
    /// otherwise still run against the ledger and call `redraw()` after the row was gone.
    fn arm(self: &Arc<Self>, generation: u64, tui: TuiHandle) {
        let Some(ctx) = self.ctx.as_ref() else {
            // No row context: a test drove the pane directly. Nothing owns a timer, so there is
            // none — the query runs on the next keystroke that does have one.
            return;
        };
        let me = Arc::clone(self);
        let window = self.cfg.debounce_ms;
        ctx.effect_spawn(move |ectx| async move {
            tokio::time::sleep(std::time::Duration::from_millis(window)).await;
            if ectx.checkpoint().await.is_err() {
                // The row is unwinding: the ledger read and the redraw both belong to a pane that
                // is on its way out.
                return Ok(());
            }
            let text = {
                let st = me.state.lock();
                if !st.debounce.due(generation, Utc::now()) {
                    return Ok(());
                }
                st.input.clone()
            };
            let result = run_query(&me.ledger, &me.cfg, &text).await;
            let changed = me.state.lock().apply(generation, result);
            if changed {
                tui.redraw();
            }
            Ok(())
        });
    }
}

#[async_trait::async_trait]
impl Pane for SearchPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let state = self.state.lock();
        // The invariant's recorder: what this frame ACTUALLY put on screen.
        invariant::record(&state.rows);
        let theme = *cx.theme();
        let painted = lines(&state, cx.view.is_focused);
        let area = cx.area;
        let top = state.top(painted.len(), area.height);
        let mut out: Vec<Line> = Vec::with_capacity(painted.len().saturating_sub(top));
        for (i, (text, hit)) in painted.iter().enumerate() {
            if i < top {
                continue;
            }
            let style = if state.error.is_some() && i == 1 {
                Style::default().fg(theme.error)
            } else if i == 0 {
                Style::default().fg(theme.dim)
            } else if i.saturating_sub(1) == state.selected {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };
            out.push(Line::from(Span::styled(text.clone(), style)));
            if let (Some(hit), Some(row)) = (hit.clone(), row_rect(area, i - top)) {
                cx.hit(row, hit);
            }
        }
        let widget = Paragraph::new(out);
        drop(state);
        // `handle` has no `area`: the height its clamping needs is whatever the last frame had.
        self.state.lock().height = area.height;
        cx.frame.render_widget(widget, area);
    }

    async fn handle(&self, _ev: PaneEvent, _cx: PaneCx) -> PaneOutcome {
        // The pane is only ever held as `Arc<dyn Pane>`; the shell calls through `Arc`, and the
        // debounce timer needs an owned `Arc<SearchPane>`, so the real body lives on `Arc<Self>`
        // (see `handle_arc`). Reaching here means someone held a bare `&SearchPane`.
        PaneOutcome::Ignored
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("type", "search"),
            ("↑/↓", "select a hit"),
            ("enter/click", "focus that step"),
            ("esc", "clear"),
        ]
    }
}

/// The registered pane: an `Arc<SearchPane>` so `handle` can arm a timer that owns the pane.
struct SearchPaneArc(Arc<SearchPane>);

#[async_trait::async_trait]
impl Pane for SearchPaneArc {
    fn render(&self, cx: &mut RenderCx<'_>) {
        self.0.render(cx)
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        use crossterm::event::{KeyCode, KeyModifiers};
        match ev {
            PaneEvent::Key(key) => {
                let mut st = self.0.state.lock();
                let generation = match key.code {
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        st.push_char(c, cx.at)
                    }
                    KeyCode::Backspace => st.backspace(cx.at),
                    KeyCode::Esc => st.clear_input(cx.at),
                    KeyCode::Down => {
                        if !st.rows.is_empty() {
                            st.selected = (st.selected + 1).min(st.rows.len() - 1);
                        }
                        st.follow_selection();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Up => {
                        st.selected = st.selected.saturating_sub(1);
                        st.follow_selection();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Enter => {
                        let hit = st.rows.get(st.selected).map(|r| hit_id(&r.step));
                        let rows = st.rows.clone();
                        drop(st);
                        return click(&self.0, hit.as_ref(), &rows, &cx);
                    }
                    _ => return PaneOutcome::Ignored,
                };
                drop(st);
                self.0.arm(generation, cx.tui.clone());
                PaneOutcome::Handled
            }
            PaneEvent::Click { hit, .. } => {
                let rows = self.0.state.lock().rows.clone();
                click(&self.0, hit.as_ref(), &rows, &cx)
            }
            PaneEvent::Scroll { delta } => {
                let mut st = self.0.state.lock();
                let painted = 1 + if st.error.is_some() { 1 } else { st.rows.len() };
                st.scroll_by(delta as i32, painted);
                drop(st);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        self.0.key_hints()
    }
}

fn click(pane: &Arc<SearchPane>, hit: Option<&HitId>, rows: &[HitRow], cx: &PaneCx) -> PaneOutcome {
    let focus_pane = cx
        .tui
        .panes()
        .into_iter()
        .find(|p| p.slot == Slot::Main)
        .map(|p| p.id);
    // Name → live id, best effort, through THIS row's declared optional `agents` handle. A hit
    // whose agent has no live handle still focuses its step.
    on_click(hit, rows, focus_pane, |name| {
        pane.agents
            .as_ref()
            .and_then(|a| a.by_name(name))
            .map(|a| a.id().clone())
    })
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

/// The row.
pub struct SearchPlugin;

#[async_trait::async_trait]
impl Plugin for SearchPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SearchConfig;

    fn inject() -> Inject {
        // `agents` is OPTIONAL and DECLARED: clicking a hit resolves the agent NAME the ledger
        // recorded to a live id, and a hit whose agent has no handle still focuses its step.
        Inject::required(["tui", "ledger"]).union(&Inject::optional(["agents"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.height == 0 {
            return reject("height must be > 0; a zero-cell pane can show no hit".to_string());
        }
        // `limit: 0` is a pane that searches and always reports nothing, silently.
        if cfg.limit == 0 {
            return reject("limit must be > 0".to_string());
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());
        let tui = ctx.get::<Tui>().map_err(|e| PluginError::new(entry, e))?;

        let agents = ctx
            .try_get::<Agents>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        // The recorded frame is per-process and this row owns it: unloading forgets what it drew.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

        let pane = Arc::new(
            SearchPane::new(cfg, ledger)
                .with_agents(agents)
                .with_ctx(ctx.clone()),
        );
        // A REGISTRATION IS AN EFFECT: `register_pane` returns the disposer, and unloading this
        // row must leave no pane, no listener and no binding behind (the SWAP gate).
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Aux,
                order: 0,
                size: SlotSize::Cells(pane.cfg.height),
                title: "search".into(),
                focusable: true,
                pane: Arc::new(SearchPaneArc(pane)),
            },
        )
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SearchPlugin);
