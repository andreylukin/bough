//! Invariant: NO STEP IS RENDERED TWICE. The live tail (what has streamed but not yet flushed to
//! `thought/text`) and the durable rows never overlap: the trailing step renders `live` whenever
//! `live.len() >= durable.len()` and the durable text otherwise (P3-D12), which makes the handover
//! flicker-free without any coordination between the `llm/stream` tee and the `ledger/step`
//! listener — two listeners that race by construction.
//!
//! This pane IS §11's `trajectory` pane (P3-D4): it owns the live tail AND the scrollback.

pub mod expand;
pub mod invariant;
pub mod rows;
pub mod scroll;
pub mod stream;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::events::{AgentStep, AgentWake, Phase};
use bough_plugin_agents::{initiator, AgentId, Agents, AgentsHandle};
use bough_plugin_ledger::{
    Ledger, LedgerHandle, LedgerStep, Order, Seq, Step, StepId, StepQuery, TrajId,
};
use bough_plugin_llm::LlmStreamEvent;
use bough_plugin_tui_render::ToolCallView;
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{FocusRequest, PaneId, Theme, Tui, TuiHandle};
use crossterm::event::{KeyCode, KeyEvent};
use parking_lot::Mutex;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub use expand::{call_of_hit, hit_for_call, Expanded};
pub use rows::{rows_from_steps, trailing_durable, Row};
pub use scroll::Scroll;
pub use stream::{apply_tee, tee_for, tee_stream, trailing_text, LiveText};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-focus";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FocusConfig {
    /// Rows held in memory; older ones are paged from the ledger on demand.
    pub max_rows: usize,
    /// Fold marker past this many lines of one tool body.
    pub max_tool_lines: usize,
    pub page_lines: u16,
    pub expand_new_tools: bool,
    pub show_reasoning: bool,
}

/// Everything the pane holds between frames. `render` reads it and nothing else.
#[derive(Default)]
pub struct FocusState {
    pub agent: Option<AgentId>,
    pub traj: Option<TrajId>,
    /// Held in seq order. The rows are recomputed from this whenever it changes, so `rows` and
    /// `steps` can never disagree.
    pub steps: Vec<Step>,
    pub rows: Vec<Row>,
    pub scroll: Scroll,
    pub expanded: Expanded,
    /// The step a `FocusRequest { step: Some(..) }` asked to show, flashed in `theme.accent`.
    pub anchor: Option<StepId>,
    /// The viewport height of the LAST frame; scroll maths needs it and `handle` has no `area`.
    pub height: u16,
    /// How many RENDERED lines the last frame produced. `render` scrolls a `Paragraph` by a line
    /// index, so the scroll maths has to clamp against lines, not against `rows`: one row wraps to
    /// many lines and an expanded tool call to dozens. Clamping against `rows.len()` made
    /// `max_top` zero for any trajectory that fit in a handful of steps, so every wheel and key
    /// scroll silently re-armed `Follow` (V3's `the_wheel_scrolls_the_trajectory`).
    pub lines: usize,
    /// `false` once the ledger has been paged back to the beginning of the trajectory.
    pub more_above: bool,
}

impl FocusState {
    /// Replace the whole step window, recomputing the rows.
    pub fn set_steps(&mut self, steps: Vec<Step>) {
        self.rows = rows_from_steps(&steps);
        self.steps = steps;
    }

    /// One appended step. `Follow` keeps following; `Anchored` does not move (V3).
    pub fn push_step(&mut self, step: Step, max_rows: usize) {
        if self.steps.iter().any(|s| s.id == step.id) {
            // The backfill and the listener race on a boot-time step. Idempotent by id, so the
            // step is rendered once whichever wins.
            return;
        }
        self.steps.push(step);
        if self.steps.len() > max_rows {
            let drop = self.steps.len() - max_rows;
            self.steps.drain(..drop);
            self.more_above = true;
        }
        let before = self.rows.len();
        self.rows = rows_from_steps(&self.steps);
        self.scroll = self
            .scroll
            .on_rows_appended(self.rows.len().saturating_sub(before));
    }

    /// The oldest seq held, for paging further back.
    pub fn oldest_seq(&self) -> Option<Seq> {
        self.steps.first().map(|s| s.seq)
    }

    /// The row index of a step, for anchoring.
    pub fn row_of(&self, step: &StepId) -> Option<usize> {
        self.rows.iter().position(|r| r.step() == step)
    }
}

/// The trajectory pane.
pub struct FocusPane {
    cfg: Arc<FocusConfig>,
    state: Arc<Mutex<FocusState>>,
    live: Arc<Mutex<LiveText>>,
}

impl FocusPane {
    /// A pane over shared state. Public so a test can drive it without a composed tree.
    pub fn new(
        cfg: Arc<FocusConfig>,
        state: Arc<Mutex<FocusState>>,
        live: Arc<Mutex<LiveText>>,
    ) -> FocusPane {
        FocusPane { cfg, state, live }
    }

    /// PURE: the whole pane, as lines. Split from `render` so the geometry (which line belongs to
    /// which tool header) is computable without a frame.
    pub fn lines(
        &self,
        state: &FocusState,
        live: &LiveText,
        width: u16,
        theme: &Theme,
    ) -> (Vec<Line<'static>>, Vec<(bough_plugin_llm::ToolCallId, u16)>) {
        let durable = trailing_durable(&state.rows);
        // The LAST text row of the trailing step is the one the live tail supersedes; every
        // earlier row is settled and is drawn from the ledger.
        let trailing_row = state
            .rows
            .iter()
            .rposition(|r| matches!(r, Row::Text { .. }));
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut headers = Vec::new();

        for (i, row) in state.rows.iter().enumerate() {
            let flash = state.anchor.as_ref() == Some(row.step());
            match row {
                Row::Andrey { text, .. } => {
                    lines.push(label("andrey", theme.accent));
                    lines.extend(bough_plugin_tui_render::markdownish(text, width, theme));
                }
                Row::Mail { from, subject, .. } => {
                    lines.push(Line::from(vec![
                        Span::styled("✉ ", Style::default().fg(theme.evidence)),
                        Span::styled(from.clone(), Style::default().fg(theme.evidence)),
                        Span::raw("  "),
                        Span::styled(subject.clone(), Style::default().fg(theme.fg)),
                    ]));
                }
                Row::Text { text, .. } => {
                    // P3-D12: exactly one of the two is drawn, chosen by length.
                    let shown = if Some(i) == trailing_row {
                        trailing_text(&durable, &live.text)
                    } else {
                        text.as_str()
                    };
                    lines.extend(bough_plugin_tui_render::markdownish(shown, width, theme));
                }
                Row::Reasoning { text, .. } => {
                    if self.cfg.show_reasoning {
                        for l in bough_plugin_tui_render::wrap(text, width) {
                            lines.push(Line::styled(
                                l,
                                Style::default()
                                    .fg(theme.thought)
                                    .add_modifier(Modifier::ITALIC),
                            ));
                        }
                    }
                }
                Row::Tool {
                    call,
                    name,
                    intent,
                    args,
                    result,
                    ..
                } => {
                    let expanded = state.expanded.is_expanded(call);
                    let view = ToolCallView {
                        name,
                        intent: *intent,
                        args,
                        result: result.as_ref(),
                        expanded,
                        width,
                        theme,
                    };
                    headers.push((call.clone(), lines.len() as u16));
                    lines.push(bough_plugin_tui_render::tool_header(&view));
                    if expanded {
                        lines.extend(bough_plugin_tui_render::tool_body(
                            &view,
                            self.cfg.max_tool_lines,
                        ));
                    }
                }
                Row::WakeMark { phase, reason, .. } => {
                    let word = match phase {
                        Phase::Start => "wake".to_string(),
                        Phase::End => format!("wake end · {}", reason.clone().unwrap_or_default()),
                    };
                    lines.push(Line::styled(
                        format!("── {word} "),
                        Style::default().fg(theme.dim),
                    ));
                }
                Row::About { view, .. } => {
                    lines.push(Line::styled(
                        view.state.clone(),
                        Style::default().fg(theme.evidence),
                    ));
                }
                Row::Other { kind, .. } => {
                    // TOTAL: a type this binary does not know still gets a line, and never a panic.
                    lines.push(Line::styled(
                        format!("· {kind}"),
                        Style::default().fg(theme.dim),
                    ));
                }
            }
            if flash {
                if let Some(last) = lines.last_mut() {
                    // The accent has to reach the SPANS, not only the line: a span's own `fg`
                    // (every `markdownish` span carries one) is patched OVER the line style by
                    // ratatui, so a line-level flash was invisible on screen for exactly the rows
                    // a search hit lands on (P3-D27).
                    for span in last.spans.iter_mut() {
                        span.style = span.style.fg(theme.accent);
                    }
                    *last = last.clone().style(Style::default().fg(theme.accent));
                }
            }
        }
        // The live tail of a turn whose first `thought/text` has not landed yet: without this the
        // first token of every answer would be invisible until the first flush.
        if trailing_row.is_none() && !live.text.is_empty() {
            lines.extend(bough_plugin_tui_render::markdownish(
                &live.text, width, theme,
            ));
        }
        (lines, headers)
    }

    /// PURE: a key ⇒ the next scroll state, or `None` if the key is not the pane's.
    pub fn scroll_for_key(&self, key: KeyEvent, state: &FocusState) -> Option<Scroll> {
        let page = self.cfg.page_lines as i32;
        let delta = match key.code {
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            KeyCode::PageUp => -page,
            KeyCode::PageDown => page,
            KeyCode::Home => i32::MIN / 2,
            KeyCode::End => i32::MAX / 2,
            _ => return None,
        };
        Some(state.scroll.scrolled(delta, state.lines, state.height))
    }
}

fn label(who: &str, color: ratatui::style::Color) -> Line<'static> {
    Line::styled(
        format!("{who}:"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

#[async_trait::async_trait]
impl Pane for FocusPane {
    fn render(&self, cx: &mut RenderCx<'_>) {
        let mut state = self.state.lock();
        // `handle` has no `area`, so the viewport height its scroll maths needs is whatever the
        // last frame actually had. Nothing was writing it, which left every keyboard and wheel
        // scroll clamped against a height of 0.
        state.height = cx.area.height;
        let live = self.live.lock().clone();
        let theme = *cx.theme();
        let area = cx.area;
        let (lines, headers) = self.lines(&state, &live, area.width, &theme);
        state.lines = lines.len();
        invariant::record_frame(&state.rows, &live);
        let top = state.scroll.top(lines.len(), area.height);
        drop(state);

        for (call, line) in headers {
            if line < top as u16 {
                continue;
            }
            let y = line - top as u16;
            if y >= area.height {
                break;
            }
            cx.hit(
                Rect {
                    x: area.x,
                    y: area.y + y,
                    width: area.width,
                    height: 1,
                },
                expand::hit_for_call(&call),
            );
        }
        cx.frame
            .render_widget(Paragraph::new(lines).scroll((top as u16, 0)), area);
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Click { hit, .. } => {
                let out = {
                    let mut state = self.state.lock();
                    let mut expanded = std::mem::take(&mut state.expanded);
                    let out = expand::on_click(&mut expanded, hit.as_ref());
                    state.expanded = expanded;
                    out
                };
                if out == PaneOutcome::Handled {
                    cx.tui.redraw();
                }
                out
            }
            PaneEvent::Scroll { delta } => {
                let mut state = self.state.lock();
                state.scroll = state
                    .scroll
                    .scrolled(delta as i32, state.lines, state.height);
                drop(state);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::Key(key) => {
                let next = {
                    let state = self.state.lock();
                    self.scroll_for_key(key, &state)
                };
                match next {
                    Some(s) => {
                        self.state.lock().scroll = s;
                        // Scrolling to the very top is the request for older rows: the pane pages
                        // rather than pretending the trajectory starts where its window does.
                        if matches!(s, Scroll::Anchored { top: 0, .. }) {
                            let ledger = cx.ctx.get::<Ledger>().ok();
                            if let Some(ledger) = ledger {
                                page_older(
                                    &LedgerHandle(ledger.0.clone()),
                                    &self.state,
                                    self.cfg.max_rows,
                                )
                                .await;
                            }
                        }
                        cx.tui.redraw();
                        PaneOutcome::Handled
                    }
                    None => PaneOutcome::Ignored,
                }
            }
            PaneEvent::Focus(req) => {
                let agents = cx.ctx.get::<Agents>().ok();
                let ledger = cx.ctx.get::<Ledger>().ok();
                if let (Some(agents), Some(ledger)) = (agents, ledger) {
                    retarget(
                        &AgentsHandle(agents.0.clone()),
                        &LedgerHandle(ledger.0.clone()),
                        &self.state,
                        &req,
                        self.cfg.max_rows,
                    )
                    .await;
                }
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑/↓", "scroll"),
            ("PgUp/PgDn", "page"),
            ("End", "follow"),
            ("click", "expand a tool call"),
        ]
    }
}

/// Point the pane at an agent, and optionally at one step inside it.
pub async fn retarget(
    agents: &AgentsHandle,
    ledger: &LedgerHandle,
    state: &Arc<Mutex<FocusState>>,
    req: &FocusRequest,
    max_rows: usize,
) {
    if let Some(id) = req.agent.clone() {
        let traj = agents.get(&id).map(|a| a.traj().clone());
        let changed = state.lock().agent.as_ref() != Some(&id);
        if changed {
            let steps = match &traj {
                Some(t) => newest_steps(ledger, t, max_rows).await,
                None => Vec::new(),
            };
            let mut held = state.lock();
            held.agent = Some(id);
            held.traj = traj;
            held.set_steps(steps);
            held.scroll = Scroll::Follow;
            held.anchor = None;
        }
    }
    if let Some(step) = req.step.clone() {
        let mut held = state.lock();
        if let Some(row) = held.row_of(&step) {
            held.scroll = Scroll::anchored_on(row);
        }
        held.anchor = Some(step);
    }
}

/// The newest `limit` steps of a trajectory, oldest first.
pub async fn newest_steps(ledger: &LedgerHandle, traj: &TrajId, limit: usize) -> Vec<Step> {
    let mut steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            order: Order::SeqDesc,
            limit: Some(limit),
            ..Default::default()
        })
        .await
        .unwrap_or_default();
    steps.reverse();
    steps
}

/// Page one window of OLDER steps in from the ledger, prepending them.
pub async fn page_older(ledger: &LedgerHandle, state: &Arc<Mutex<FocusState>>, page: usize) {
    let (traj, before) = {
        let held = state.lock();
        (held.traj.clone(), held.oldest_seq())
    };
    let (Some(traj), Some(before)) = (traj, before) else {
        return;
    };
    let mut older = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj],
            before: Some(before),
            order: Order::SeqDesc,
            limit: Some(page),
            ..Default::default()
        })
        .await
        .unwrap_or_default();
    older.reverse();
    if older.is_empty() {
        state.lock().more_above = false;
        return;
    }
    let mut held = state.lock();
    let added = older.len();
    let mut steps = older;
    steps.append(&mut held.steps);
    held.set_steps(steps);
    // The rows Andrey was looking at moved DOWN by what was prepended. Keeping the same absolute
    // index would silently scroll the viewport, which is the one thing anchoring exists to prevent.
    if let Scroll::Anchored { top, offset } = held.scroll {
        held.scroll = Scroll::Anchored {
            top: top + added,
            offset,
        };
    }
}

/// The row.
pub struct FocusPlugin;

#[async_trait::async_trait]
impl Plugin for FocusPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = FocusConfig;

    fn inject() -> Inject {
        Inject::required(["tui", "agents", "ledger", "llm"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let tui = TuiHandle(tui.0.clone());
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = AgentsHandle(agents.0.clone());
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());

        let state = Arc::new(Mutex::new(FocusState::default()));
        let live: Arc<Mutex<LiveText>> = Arc::new(Mutex::new(LiveText::default()));

        // Whatever the shell is already focused on, so a pane that mounts second is not blank.
        if let Some(id) = tui.focused_agent() {
            retarget(
                &agents,
                &ledger,
                &state,
                &FocusRequest {
                    agent: Some(id),
                    ..Default::default()
                },
                cfg.max_rows,
            )
            .await;
        }

        let pane = Arc::new(FocusPane::new(cfg.clone(), state.clone(), live.clone()));
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new("tui.focus"),
                slot: Slot::Main,
                order: 0,
                size: SlotSize::Fill(1),
                title: "trajectory".into(),
                focusable: true,
                pane: pane.clone(),
            },
        )
        .await?;

        // The durable half: every step of the focused trajectory becomes a row.
        let (s, t, c) = (state.clone(), tui.clone(), cfg.clone());
        ctx.on::<LedgerStep, _, _>(move |step| {
            let (s, t, c) = (s.clone(), t.clone(), c.clone());
            async move {
                let mine = s.lock().traj.as_ref() == Some(&step.traj);
                if !mine {
                    return;
                }
                s.lock().push_step(step.as_ref().clone(), c.max_rows);
                t.redraw();
            }
        })
        .await?;

        // The live half: a tee on `llm/stream`, keyed by the AMBIENT initiator (§2). It replaces
        // nothing and short-circuits nothing — `next` runs first, and what comes back is what is
        // returned, with at most a wrapper around the stream it already carries.
        let (s, l, t) = (state.clone(), live.clone(), tui.clone());
        ctx.on_waterfall::<LlmStreamEvent, _, _>(move |call, next| {
            let (s, l, t) = (s.clone(), l.clone(), t.clone());
            let who = initiator::current();
            async move {
                let filled = next.run(call).await;
                let focused = s.lock().agent.clone();
                let tui = t.clone();
                apply_tee(
                    &filled,
                    who,
                    focused.as_ref(),
                    l,
                    Arc::new(move || tui.redraw()),
                );
                filled
            }
        })
        .await?;

        // Clearing the tail: at both of these moments the durable steps are the whole truth, and
        // anything still held would be drawn a second time.
        let (l, t) = (live.clone(), tui.clone());
        ctx.on::<AgentStep, _, _>(move |ev| {
            let (l, t) = (l.clone(), t.clone());
            async move {
                if ev.phase == Phase::Start {
                    l.lock().clear();
                    t.redraw();
                }
            }
        })
        .await?;

        let (l, t) = (live.clone(), tui.clone());
        ctx.on::<AgentWake, _, _>(move |ev| {
            let (l, t) = (l.clone(), t.clone());
            async move {
                if ev.phase == Phase::End {
                    l.lock().clear();
                    t.redraw();
                }
            }
        })
        .await?;

        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(FocusPlugin);
