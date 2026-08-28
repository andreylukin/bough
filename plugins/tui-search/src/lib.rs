//! Invariant: this row is the SWAP subject and is deliberately self-contained. Nothing else in the
//! tree may depend on it, and disabling it by patch must be indistinguishable from never having
//! mounted it — no pane, no listener, no binding left behind (§17 Phase 3).
//!
//! It reads `ctx.ledger` and never `ctx.agents`' loop: a hit is a step id, and clicking one is a
//! `FocusRequest`, not a wake. What it INDEXES is the rendered conversation (`index`), never the
//! ledger's FTS over raw JSON (phase ux1 M11).
//!
//! Everything a test needs is a PURE function over state the pane already holds: `RenderCx`,
//! `PaneCx` and `TuiHandle` are only constructible inside `tui-shell`, so `render` and `handle`
//! are thin shells over `index::{entries, search, lines}`, `SearchState` and `on_click`
//! (deviation D-WP5-1, reported at the seam).

pub mod index;
pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{AgentId, Agents, AgentsHandle};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, Order, StepId, StepQuery};
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneId, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{FocusRequest, HitId, Tui, TuiHandle};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use ratatui::widgets::Paragraph;

pub use crate::index::{counter, Entry, Hit};

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
    /// The most hits a query reports.
    pub limit: usize,
    pub debounce_ms: u64,
    /// Characters either side of a match in a snippet.
    pub snippet_radius: usize,
    /// How many steps of each trajectory the index covers. The index is over RENDERED rows held
    /// in memory, so the window is what bounds the work.
    pub window: usize,
}

/// PURE: the one line a hit draws, without styling. The pane paints the styled version through
/// [`index::lines`]; this is what a test and a log read.
pub fn hit_line(hit: &Hit) -> String {
    format!("{}  {}", hit.speaker, hit.snippet)
}

/// The `HitId` for a hit row.
pub fn hit_id(step: &StepId) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{step}"))
}

/// The step a `HitId` names, when it is one of ours.
pub fn step_of_hit(hit: &HitId) -> Option<StepId> {
    hit.as_str().strip_prefix(HIT_PREFIX).map(StepId::new)
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
    pub rows: Vec<Hit>,
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
    /// How many steps per agent the last query actually looked at ([`SearchConfig::window`]).
    pub window: usize,
    /// Whether that window was FULL for at least one agent — i.e. there are older steps this
    /// query did not read. The counter SAYS so: "no matches" for a word said 500 steps ago and
    /// "no matches" for a word never said must not read the same (M11).
    pub windowed: bool,
}

impl SearchState {
    pub fn new(cfg: &SearchConfig) -> SearchState {
        SearchState {
            input: String::new(),
            rows: Vec::new(),
            error: None,
            debounce: Debounce::new(cfg.debounce_ms),
            window: cfg.window,
            windowed: false,
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

    /// `Esc`: query, hits and rows all go in ONE call (minor 30 — the field "never clears").
    pub fn clear(&mut self, now: DateTime<Utc>) -> u64 {
        self.input.clear();
        self.rows.clear();
        self.error = None;
        self.selected = 0;
        self.scroll = 0;
        self.debounce.on_input(now)
    }

    /// `n` / `N`: step to the next or previous match, wrapping.
    pub fn step_match(&mut self, forward: bool) {
        self.selected = index::step_selection(self.selected, self.rows.len(), forward);
        self.follow_selection();
    }

    /// What the counter reads: `""` while the query is empty (D-WP6-2).
    pub fn counter(&self) -> String {
        if self.input.trim().is_empty() {
            return String::new();
        }
        let base = index::counter(self.selected, self.rows.len());
        if self.windowed {
            format!("{base} \u{b7} newest {} steps", self.window)
        } else {
            base
        }
    }

    /// Land a query result. A STALE generation is dropped: a slow query for an older keystroke can
    /// never overwrite the answer to what is on screen now. Returns whether anything changed.
    pub fn apply<F: Into<Found>>(&mut self, generation: u64, result: Result<F, String>) -> bool {
        if generation != self.debounce.generation() {
            return false;
        }
        match result {
            Ok(found) => {
                let found: Found = found.into();
                self.rows = found.hits;
                self.windowed = found.windowed;
                self.error = None;
            }
            Err(msg) => {
                // An FTS syntax error renders inline AND clears the list: a stale result list
                // beside a fresh error would be a lie about what matched.
                self.rows.clear();
                self.windowed = false;
                self.error = Some(msg);
            }
        }
        self.selected = 0;
        self.scroll = 0;
        true
    }
}

/// PURE: the plain-text lines the pane paints, each with the `HitId` its row is clickable under.
/// The STYLED version is [`index::lines`]; this one is what a text assertion reads.
pub fn lines(state: &SearchState, _prompt_focused: bool) -> Vec<(String, Option<HitId>)> {
    let counter = state.counter();
    let head = if counter.is_empty() {
        format!("{} [{}\u{258f}]", index::FIELD_LABEL, state.input)
    } else {
        format!(
            "{} [{}\u{258f}]  {counter}",
            index::FIELD_LABEL,
            state.input
        )
    };
    let mut out = vec![(head, None)];
    if let Some(err) = &state.error {
        out.push((format!("! {err}"), None));
        return out;
    }
    for hit in &state.rows {
        out.push((hit_line(hit), Some(hit_id(&hit.step))));
    }
    out
}

/// PURE: what a click on a recorded hit means. A hit is a step id, so the outcome is a
/// `FocusRequest` — never a wake.
pub fn on_click(
    hit: Option<&HitId>,
    rows: &[Hit],
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
        agent: resolve(&row.agent),
        pane: focus_pane,
        step: Some(row.step.clone()),
    })
}

/// Run one query. The only I/O in the crate, and it is never called from `render`.
///
/// The I/O is a READ of steps; the search itself is [`index::search`] over rows the focus pane's
/// own projection built. Nothing here touches `LedgerStore::search`: FTS over ledger JSON is what
/// put `request/header  {"as_of":53,…}` on screen (M11).
pub async fn run_query(
    ledger: &LedgerHandle,
    cfg: &SearchConfig,
    text: &str,
) -> Result<Found, String> {
    if text.trim().is_empty() {
        return Ok(Found::default());
    }
    let agents = ledger.0.agents().await.map_err(|e| e.to_string())?;
    let mut windowed = false;
    let mut hits = Vec::new();
    for agent in &agents {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![agent.traj.clone()],
                order: Order::SeqDesc,
                limit: Some(cfg.window),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        // A FULL window means there are older steps this query never read. Silence about that
        // is what makes "no matches" ambiguous (M11's other half).
        windowed |= steps.len() >= cfg.window;
        // Newest-first is how the window is taken; the projection wants seq order.
        let mut steps = steps;
        steps.reverse();
        let rows = bough_plugin_tui_focus::rows::rows_from_steps(&steps);
        let entries = index::entries(&agent.name, &rows);
        hits.extend(index::search(&entries, text, cfg.snippet_radius));
        if hits.len() >= cfg.limit {
            hits.truncate(cfg.limit);
            break;
        }
    }
    Ok(Found { hits, windowed })
}

/// What one query found, and whether it reached its horizon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// `true` when at least one agent's step window was full: older steps exist and were NOT
    /// searched. The pane says so on the counter rather than letting an unread step look like a
    /// word that was never written.
    pub windowed: bool,
}

impl From<Vec<Hit>> for Found {
    fn from(hits: Vec<Hit>) -> Found {
        Found {
            hits,
            windowed: false,
        }
    }
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
        let area = cx.area;
        let painted = index::lines(
            &state.rows,
            state.selected,
            &state.input,
            area.width,
            &theme,
        );
        let top = state.top(painted.len(), area.height);
        let mut out: Vec<ratatui::text::Line> =
            Vec::with_capacity(painted.len().saturating_sub(top));
        for (i, (line, hit)) in painted.into_iter().enumerate() {
            if i < top {
                continue;
            }
            let hit = hit.clone();
            out.push(line);
            if let (Some(hit), Some(row)) = (hit, row_rect(area, i - top)) {
                cx.hit(row, hit);
            }
        }
        if let Some(err) = &state.error {
            out.insert(
                1.min(out.len()),
                ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!("! {err}"),
                    ratatui::style::Style::default().fg(theme.error),
                )),
            );
        }
        let widget = Paragraph::new(out);
        // Rows only while there is something to show (visual audit F1): an empty search took a
        // third of the frame on every launch. With no query and no hits the pane asks for zero
        // rows; the shell hands it its full height again the moment Ctrl+F moves the keyboard
        // here, so it never has to be visible to be opened.
        let wanted = if state.input.is_empty() && state.rows.is_empty() && state.error.is_none() {
            0
        } else {
            self.cfg.height
        };
        drop(state);
        cx.report_aux_rows(wanted);
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
            ("^n/^N", "next / previous match"),
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
                    // Esc on an EMPTY search is not ours: the shell takes the keyboard back to
                    // the composer, and the pane gives up its rows on the next frame (F1).
                    KeyCode::Esc if st.input.is_empty() && st.rows.is_empty() => {
                        return PaneOutcome::Ignored
                    }
                    KeyCode::Esc => st.clear(cx.at),
                    // Backwards FIRST: a terminal may report `Ctrl+Shift+n` as `n` with both
                    // modifiers or as `N` with control, and both mean "the previous match".
                    KeyCode::Char('n' | 'N')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.modifiers.contains(KeyModifiers::SHIFT) =>
                    {
                        st.step_match(false);
                        drop(st);
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        st.step_match(true);
                        drop(st);
                        // The selection moved, so the SCREEN has to: nothing else repaints for a
                        // key a pane handled.
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Char('N') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        st.step_match(false);
                        drop(st);
                        // The selection moved, so the SCREEN has to: nothing else repaints for a
                        // key a pane handled.
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Down => {
                        st.step_match(true);
                        drop(st);
                        // The selection moved, so the SCREEN has to: nothing else repaints for a
                        // key a pane handled.
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Up => {
                        st.step_match(false);
                        drop(st);
                        // The selection moved, so the SCREEN has to: nothing else repaints for a
                        // key a pane handled.
                        cx.tui.redraw();
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
            // Minor 30, "the field never clears": `Esc` is the SHELL's "give the composer the
            // keyboard back" binding and never reaches a pane (`keymap.rs`), so the pane clears on
            // LOSING focus. Query, hits and selection go together, which is what reopening `^f`
            // on a fresh field means.
            // …and it opens FRESH. `Ctrl+F` is "start a search", not "resume the last one": the
            // keyboard can leave this pane by paths that never reach it (the composer takes it
            // back), so gaining focus is the moment that can be relied on.
            PaneEvent::FocusChanged(true) => {
                let mut st = self.0.state.lock();
                if st.input.is_empty() && st.rows.is_empty() {
                    return PaneOutcome::Ignored;
                }
                st.clear(cx.at);
                drop(st);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::FocusChanged(false) => {
                let mut st = self.0.state.lock();
                if st.input.is_empty() && st.rows.is_empty() {
                    return PaneOutcome::Ignored;
                }
                st.clear(cx.at);
                drop(st);
                cx.tui.redraw();
                PaneOutcome::Handled
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

fn click(pane: &Arc<SearchPane>, hit: Option<&HitId>, rows: &[Hit], cx: &PaneCx) -> PaneOutcome {
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
        if cfg.window == 0 {
            return reject("window must be > 0; a zero-step index can match nothing".to_string());
        }
        if cfg.snippet_radius == 0 {
            return reject(
                "snippet_radius must be > 0; a zero-width window shows the match with no context"
                    .to_string(),
            );
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
