//! Invariant: NO STEP IS RENDERED TWICE. The live tail (what has streamed but not yet flushed to
//! `thought/text`) and the durable rows never overlap: the trailing step renders `live` whenever
//! `live.len() >= durable.len()` and the durable text otherwise (P3-D12), which makes the handover
//! flicker-free without any coordination between the `llm/stream` tee and the `ledger/step`
//! listener — two listeners that race by construction.
//!
//! This pane IS §11's `trajectory` pane (P3-D4): it owns the live tail AND the scrollback.

pub mod branches;
pub mod claims;
pub mod expand;
pub mod invariant;
pub mod program;
pub mod rowfocus;
pub mod rows;
pub mod scroll;
pub mod stream;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
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

pub use branches::{branches_from_edges, Branch, BranchPicker, PickerOutcome};
pub use claims::{claim_action_of_hit, hit_for_claim, ClaimAction};
pub use expand::{call_of_hit, hit_for_call, Expanded};
pub use program::{program_header, program_lines, ProgramError, ProgramSub, ProgramView, RUN_TOOL};
pub use rowfocus::{focus_marker, RowFocus};
pub use rows::{
    rows_from_steps, trailing_durable, trailing_text_row, trailing_text_rows, ClaimState, Row,
};
pub use scroll::{Scroll, Viewport};
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
    /// Rows that arrived while the viewport was NOT at the tail — what the `↓ N new` affordance
    /// counts (phase ux1 §2.2, B2). Zeroed by every route back to the tail, so the badge and the
    /// scroll state can never disagree.
    pub unseen: usize,
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
    /// The focused agent's OWN trajectory, remembered while `traj` is overridden by a branch, so
    /// `Esc` always returns to it (§11, `branches`).
    pub home_traj: Option<TrajId>,
    /// The branch picker, `^b`.
    pub picker: BranchPicker,
    /// The roving row focus (B6). `None` until the keyboard arrives in this pane.
    pub row_focus: RowFocus,
    /// Where each row's FIRST line landed in the last frame, by row index. `handle` has no
    /// geometry of its own, and moving the row focus has to be able to bring the row into view.
    pub row_lines: Vec<u16>,
    /// The focused agent's NAME, for the speaker label on its text (visual audit F2). `None`
    /// until `retarget` has looked it up; a text row then carries no label rather than a guess.
    pub agent_name: Option<String>,
    /// The pane's top row on screen in the last frame, so a click's absolute row can be turned
    /// into a line of the transcript (click-any-row, visual audit).
    pub area_y: u16,
}

impl FocusState {
    /// Replace the whole step window, recomputing the rows.
    pub fn set_steps(&mut self, steps: Vec<Step>) {
        self.rows = rows_from_steps(&steps);
        self.steps = steps;
    }

    /// One appended step. `Follow` keeps following; `Anchored` does not move (V3).
    pub fn push_step(&mut self, step: Step, max_rows: usize, expand_new_tools: bool) {
        if self.steps.iter().any(|s| s.id == step.id) {
            // The backfill and the listener race on a boot-time step. Idempotent by id, so the
            // step is rendered once whichever wins.
            return;
        }
        // `expand_new_tools`: a tool call arriving is drawn OPEN, so a run reads as it happens
        // rather than as a list of one-line headers to click. Keyed by call id like every other
        // expansion, so a later collapse sticks.
        if expand_new_tools && step.kind.as_str() == "tool/call" {
            if let Some(call) = step.body.get("call").and_then(|v| v.as_str()) {
                self.expanded
                    .insert(&bough_plugin_llm::ToolCallId::new(call));
            }
        }
        self.steps.push(step);
        if self.steps.len() > max_rows {
            let drop = self.steps.len() - max_rows;
            self.steps.drain(..drop);
            self.more_above = true;
        }
        let before = self.rows.len();
        self.rows = rows_from_steps(&self.steps);
        let added = self.rows.len().saturating_sub(before);
        self.scroll = self.scroll.on_rows_appended(added);
        if !self.scroll.is_following() {
            self.unseen = self.unseen.saturating_add(added);
        }
    }

    /// The oldest seq held, for paging further back.
    pub fn oldest_seq(&self) -> Option<Seq> {
        self.steps.first().map(|s| s.seq)
    }

    /// Show a branch: a PANE-LOCAL trajectory override. A fork has no agent, so this is never a
    /// `FocusRequest`; `agent` deliberately does not move.
    pub fn show_branch(&mut self, traj: TrajId, steps: Vec<Step>) {
        if self.home_traj.is_none() {
            self.home_traj = self.traj.clone();
        }
        self.traj = Some(traj);
        self.set_steps(steps);
        self.scroll = Scroll::Follow;
        self.unseen = 0;
        self.anchor = None;
    }

    /// Back to the agent's own chain. A no-op when no branch is being shown.
    pub fn restore_own_chain(&mut self, steps: Vec<Step>) -> bool {
        let Some(home) = self.home_traj.take() else {
            return false;
        };
        self.traj = Some(home);
        self.set_steps(steps);
        self.scroll = Scroll::Follow;
        self.unseen = 0;
        self.anchor = None;
        true
    }

    /// Whether the pane is showing a branch rather than the focused agent's own chain.
    pub fn on_branch(&self) -> bool {
        self.home_traj.is_some()
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
    /// The handles this ROW declared and injected. `PaneCx` no longer carries a `Context` (§0.3:
    /// resolving a service through the SHELL's committed view let any pane reach a key it never
    /// declared), so what `handle` may reach is exactly what `apply` was given.
    deps: Option<Deps>,
}

/// What the pane's `handle` does I/O through.
#[derive(Clone)]
struct Deps {
    agents: AgentsHandle,
    ledger: LedgerHandle,
}

impl FocusPane {
    /// A pane over shared state. Public so a test can drive it without a composed tree.
    pub fn new(
        cfg: Arc<FocusConfig>,
        state: Arc<Mutex<FocusState>>,
        live: Arc<Mutex<LiveText>>,
    ) -> FocusPane {
        FocusPane {
            cfg,
            state,
            live,
            deps: None,
        }
    }

    /// The injected handles, attached by `apply`. A pane built without them scrolls and expands
    /// but pages nothing: there is no ledger to page from.
    pub(crate) fn with_deps(mut self, agents: AgentsHandle, ledger: LedgerHandle) -> FocusPane {
        self.deps = Some(Deps { agents, ledger });
        self
    }

    /// PURE: the whole pane, as lines. Split from `render` so the geometry (which line belongs to
    /// which tool header) is computable without a frame.
    pub fn lines(
        &self,
        state: &FocusState,
        live: &LiveText,
        width: u16,
        theme: &Theme,
    ) -> (
        Vec<Line<'static>>,
        Vec<(bough_plugin_llm::ToolCallId, u16)>,
        Vec<claims::ClaimHit>,
    ) {
        let (lines, headers, hits, _) = self.lines_with_rows(state, live, width, theme);
        (lines, headers, hits)
    }

    /// The same pass, plus WHERE each row started. `render` needs the row geometry to draw the
    /// roving focus and to record a hit region for a whole tool block; `lines` is the three-value
    /// view every existing caller already has.
    #[allow(clippy::type_complexity)]
    pub fn lines_with_rows(
        &self,
        state: &FocusState,
        live: &LiveText,
        width: u16,
        theme: &Theme,
    ) -> (
        Vec<Line<'static>>,
        Vec<(bough_plugin_llm::ToolCallId, u16)>,
        Vec<claims::ClaimHit>,
        Vec<u16>,
    ) {
        // The picker takes the whole pane while it is open: it is a choice about WHAT the pane
        // shows, and showing it beside the thing it would replace reads as two trajectories.
        if state.picker.open {
            return (
                state.picker.lines(width, theme),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
        }
        let durable = trailing_durable(&state.rows);
        // Since P5-D14 the flushes of one step index are already ONE row, so the only choice left
        // here is P3-D12's: the trailing row draws either its durable text or the live tail.
        let trailing = rows::trailing_text_row(&state.rows);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut headers = Vec::new();
        let mut claim_hits: Vec<claims::ClaimHit> = Vec::new();

        // The window is not the trajectory: `max_rows` steps back is as far as this pane holds,
        // and saying so is what stops an elided beginning from reading as the whole story.
        if state.more_above {
            lines.push(Line::styled(
                "\u{2026} older steps above (PgUp)",
                Style::default().fg(theme.dim),
            ));
        }

        let mut row_lines: Vec<u16> = Vec::with_capacity(state.rows.len());
        for (i, row) in state.rows.iter().enumerate() {
            let flash = state.anchor.as_ref() == Some(row.step());
            let row_start = lines.len();
            row_lines.push(row_start as u16);
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
                    // The speaker (visual audit F2): Andrey's rows said `andrey:` and the agent's
                    // said nothing, so the two halves of the conversation were told apart by
                    // nothing but position. The name opens each span the agent speaks.
                    if rows::opens_speech(&state.rows, i) {
                        if let Some(name) = &state.agent_name {
                            lines.push(label(name, theme.accent));
                        }
                    }
                    // ONE paragraph, wrapped at `width`: the joined row is a single string, so it
                    // flows rather than breaking at every flush boundary (the field bug).
                    let shown = if Some(i) == trailing {
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
                // Code mode's ONE row: the header, and when it is open the JS source, the console
                // output beneath it, and the sub-calls as nested tool rows (`program.rs`).
                Row::Program {
                    call,
                    source,
                    console,
                    subs,
                    result,
                    error,
                    ms,
                    ..
                } => {
                    let view = program::ProgramView {
                        call,
                        source,
                        console,
                        subs,
                        result: result.as_ref(),
                        error: error.as_ref(),
                        ms: *ms,
                        expanded: &state.expanded,
                        width,
                        theme,
                        max_tool_lines: self.cfg.max_tool_lines,
                    };
                    let (block, hs) = program::program_lines(&view);
                    let base = lines.len() as u16;
                    headers.extend(hs.into_iter().map(|(c, off)| (c, base + off)));
                    lines.extend(block);
                }
                Row::WakeMark {
                    phase,
                    reason,
                    cause,
                    ..
                } => {
                    // Turn/message vocabulary at BODY contrast (nit 37, M22): the rhythm the
                    // personas praised, in words they use.
                    let word = rows::turn_mark_words(phase, reason.as_deref(), cause.as_deref());
                    lines.push(Line::styled(
                        format!("── {word} "),
                        Style::default().fg(theme.fg),
                    ));
                }
                Row::About { view, .. } => {
                    lines.push(Line::styled(
                        view.state.clone(),
                        Style::default().fg(theme.evidence),
                    ));
                }
                Row::Claim {
                    claim,
                    kind,
                    title,
                    body,
                    state,
                    ..
                } => {
                    let (card, regions) = claims::card(
                        claim,
                        kind,
                        title,
                        body,
                        state,
                        lines.len() as u16,
                        width,
                        theme,
                    );
                    lines.extend(card);
                    claim_hits.extend(regions);
                }
                Row::Other { kind, .. } => {
                    // TOTAL: a type this binary does not know still gets a line, and never a panic.
                    lines.push(Line::styled(
                        format!("· {kind}"),
                        Style::default().fg(theme.dim),
                    ));
                }
            }
            // The roving row focus, drawn NEVER BY COLOUR ALONE (audit delight 3): a marker glyph
            // in the gutter column of every line of the row, and a `sel_bg` fill behind it.
            if state.row_focus.is_on(i) {
                for (n, line) in lines.iter_mut().enumerate().skip(row_start) {
                    let marker = if n == row_start { focus_marker() } else { ' ' };
                    line.spans.insert(
                        0,
                        Span::styled(marker.to_string(), Style::default().fg(theme.accent)),
                    );
                    *line = line.clone().style(Style::default().bg(theme.sel_bg));
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
        if trailing.is_none() && !live.text.is_empty() {
            lines.extend(bough_plugin_tui_render::markdownish(
                &live.text, width, theme,
            ));
        }
        (lines, headers, claim_hits, row_lines)
    }

    /// Compute the focused agent's branches and open the picker over them. With no injected
    /// handles the picker opens EMPTY rather than not at all: "no branches" is an answer.
    pub async fn open_picker(&self) {
        let traj = {
            let held = self.state.lock();
            held.home_traj.clone().or_else(|| held.traj.clone())
        };
        let branches = match (&self.deps, traj) {
            (Some(deps), Some(traj)) => branches_for(&deps.ledger, &deps.agents, &traj).await,
            _ => Vec::new(),
        };
        self.state.lock().picker.open_with(branches);
    }

    /// What the pane does with the picker's answer.
    async fn after_picker(&self, out: PickerOutcome, cx: &PaneCx) -> PaneOutcome {
        match out {
            PickerOutcome::Ignored => PaneOutcome::Ignored,
            PickerOutcome::Moved => {
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PickerOutcome::Show(traj) => {
                let steps = match &self.deps {
                    Some(deps) => newest_steps(&deps.ledger, &traj, self.cfg.max_rows).await,
                    None => Vec::new(),
                };
                self.state.lock().show_branch(traj, steps);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PickerOutcome::Restore => {
                let home = self.state.lock().home_traj.clone();
                let steps = match (&self.deps, &home) {
                    (Some(deps), Some(t)) => newest_steps(&deps.ledger, t, self.cfg.max_rows).await,
                    _ => Vec::new(),
                };
                self.state.lock().restore_own_chain(steps);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
        }
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

/// PURE: the scroll that brings `line` into a `height`-tall window, leaving an already-visible
/// line — and `Follow` — exactly where it is. A focus indicator off screen is not an indicator.
pub fn reveal(scroll: Scroll, line: usize, lines: usize, height: u16) -> Scroll {
    if height == 0 {
        return scroll;
    }
    let top = scroll.top(lines, height);
    let h = height as usize;
    if line < top {
        Scroll::anchored_on(line)
    } else if line >= top + h {
        Scroll::anchored_on(line + 1 - h)
    } else {
        scroll
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
        state.area_y = cx.area.y;
        let live = self.live.lock().clone();
        let theme = *cx.theme();
        // The FOCUS RING (B1/M16): one column, ALWAYS reserved, painted only when this pane holds
        // the keyboard. Reserving it unconditionally is the point — the transcript must not
        // reflow every time Tab moves the keyboard, and a ring drawn over column 0 would eat a
        // character of every line.
        let full = cx.area;
        let ring_w = 1u16.min(full.width);
        let area = Rect {
            x: full.x + ring_w,
            y: full.y,
            width: full.width - ring_w,
            height: full.height,
        };
        // THE PROSE MEASURE (M13): text a human reads wraps at `min(width, measure_cols)`, so a
        // 200-column terminal gets a 90-column paragraph and the rest is margin.
        let measure = bough_plugin_tui_shell::measure(area.width, cx.view.measure_cols);
        let (lines, headers, claim_hits, row_lines) =
            self.lines_with_rows(&state, &live, measure, &theme);
        state.lines = lines.len();
        state.row_lines = row_lines.clone();
        let tool_rows: Vec<(usize, bough_plugin_llm::ToolCallId)> = state
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Row::Tool { call, .. } => Some((i, call.clone())),
                _ => None,
            })
            .collect();
        // A program row is NOT a block-sized target: its nested calls are targets of their own,
        // and a block hit over the whole thing would turn every click on a sub-row into "collapse
        // the program". Its headers get one-line hits below instead.
        let program_calls: Vec<String> = state
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Program { call, .. } => Some(call.to_string()),
                _ => None,
            })
            .collect();
        invariant::record_frame(&state.rows, &live);
        let top = state.scroll.top(lines.len(), area.height);
        // The unread affordance (phase ux1 §2.2, B2): scrolled up with rows arriving below, the
        // pane SAYS how many and what to press. Nothing is drawn while following, so a reader at
        // the tail never sees chrome for a state they are not in.
        let badge = (!state.scroll.is_following() && state.unseen > 0)
            .then(|| format!("\u{2193} {} new · End", state.unseen));
        let is_following = state.scroll.is_following();
        let row_focus_ix = state.row_focus.index;
        drop(state);

        // The clickable region of a tool call is its WHOLE BLOCK — the header AND, when it is
        // open, its body (M26). A one-line target on a row a user has to aim at with a mouse is
        // what made every click a guess; a block-sized target cannot be missed by one row, and
        // clicking an open tool anywhere collapses it.
        //
        // The block is the ROW's line span, so a text row that follows a tool call is never
        // swallowed into it: `row_lines[i]..row_lines[i + 1]`.
        let total = lines.len() as u16;
        for (call, line) in headers.iter() {
            let id = call.to_string();
            let owner = id.split('.').next().unwrap_or(&id);
            if !program_calls.iter().any(|p| p == owner) {
                continue;
            }
            if *line < top as u16 {
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
                expand::hit_for_call(call),
            );
        }
        for (i, call) in tool_rows.iter() {
            let first = row_lines.get(*i).copied().unwrap_or(0);
            let last = row_lines
                .get(i + 1)
                .copied()
                .unwrap_or(total)
                .max(first + 1);
            if last <= top as u16 {
                continue;
            }
            let y = first.saturating_sub(top as u16);
            if y >= area.height {
                break;
            }
            let height = (last - top as u16 - y).min(area.height - y);
            cx.hit(
                Rect {
                    x: area.x,
                    y: area.y + y,
                    width: area.width,
                    height,
                },
                expand::hit_for_call(call),
            );
        }
        for hit in claim_hits {
            if hit.line < top as u16 {
                continue;
            }
            let y = hit.line - top as u16;
            if y >= area.height {
                break;
            }
            cx.hit(
                Rect {
                    x: area.x + hit.x.min(area.width),
                    y: area.y + y,
                    width: hit.width.min(area.width.saturating_sub(hit.x)),
                    height: 1,
                },
                hit.id,
            );
        }
        cx.frame
            .render_widget(Paragraph::new(lines).scroll((top as u16, 0)), area);
        // The ring itself. A GLYPH and a colour, never a colour alone.
        if ring_w > 0 {
            let glyph = if cx.view.is_focused { "\u{258e}" } else { " " };
            let style = Style::default().fg(cx.theme().accent);
            for dy in 0..full.height {
                cx.frame.render_widget(
                    Paragraph::new(Line::styled(glyph, style)),
                    Rect {
                        x: full.x,
                        y: full.y + dy,
                        width: ring_w,
                        height: 1,
                    },
                );
            }
        }
        // What only this pane can see, for the next frame's `ShellView` (§2.12).
        cx.report_rows(row_focus_ix, is_following);
        if let Some(text) = badge {
            if area.height > 0 {
                let w = (text.chars().count() as u16).min(area.width);
                let rect = Rect {
                    x: area.x + area.width.saturating_sub(w),
                    y: area.y + area.height - 1,
                    width: w,
                    height: 1,
                };
                cx.frame.render_widget(
                    Paragraph::new(Line::styled(
                        text,
                        Style::default().fg(cx.theme().bg).bg(cx.theme().accent),
                    )),
                    rect,
                );
            }
        }
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        match ev {
            PaneEvent::Click { hit, at, .. } => {
                // Click-any-row (visual audit): the row marker goes to the row the click landed
                // on, whatever the row is — before this only a tool header or a claim button
                // answered a click, and a click on prose did nothing visible. The keyboard stays
                // where it was (B1); the MARKER moves, so Up/Down and Enter continue from here
                // once Tab brings the keyboard over.
                {
                    let mut state = self.state.lock();
                    let top = state.scroll.top(state.lines, state.height);
                    let line = top + usize::from(at.1.saturating_sub(state.area_y));
                    if let Some(i) = RowFocus::row_at_line(&state.row_lines, line, state.lines) {
                        state.row_focus = RowFocus { index: Some(i) };
                    }
                }
                cx.tui.redraw();
                // A claim card's button. A click is Andrey's hand on the keyboard (§16), and it
                // dispatches the SAME command line the keyboard path types, so the two surfaces
                // cannot drift apart.
                if let Some((claim, action)) = hit.as_ref().and_then(claims::claim_action_of_hit) {
                    let body = {
                        let state = self.state.lock();
                        state
                            .rows
                            .iter()
                            .find_map(|r| match r {
                                Row::Claim { claim: c, body, .. } if *c == claim => {
                                    Some(body.clone())
                                }
                                _ => None,
                            })
                            .unwrap_or_default()
                    };
                    let (line, run) = claims::line_for(&claim, action, &body);
                    return if run {
                        PaneOutcome::Command(line)
                    } else {
                        PaneOutcome::Compose(line)
                    };
                }
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
                if state.scroll.is_following() {
                    state.unseen = 0;
                }
                drop(state);
                cx.tui.redraw();
                PaneOutcome::Handled
            }
            PaneEvent::Key(key) => {
                // The picker owns the keyboard while it is open, and `^b` is what opens it.
                let picking = self.state.lock().picker.open;
                if picking {
                    let out = self.state.lock().picker.on_key(key);
                    return self.after_picker(out, &cx).await;
                }
                if key.code == KeyCode::Char('b')
                    && key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL)
                {
                    self.open_picker().await;
                    cx.tui.redraw();
                    return PaneOutcome::Handled;
                }
                // B6: with the keyboard in this pane, Up/Down move the ROVING ROW FOCUS and
                // Enter/Space toggle the focused row's disclosure. There was no keyboard path to
                // a tool call at all before this: the diff behind a write was mouse-only.
                match key.code {
                    KeyCode::Up | KeyCode::Down => {
                        let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                        let mut state = self.state.lock();
                        let rows = state.rows.len();
                        state.row_focus = std::mem::take(&mut state.row_focus).moved(delta, rows);
                        // Bring it into view: a focus indicator off screen is not an indicator.
                        if let Some(i) = state.row_focus.index {
                            if let Some(line) = state.row_lines.get(i).copied() {
                                let (lines, height) = (state.lines, state.height);
                                state.scroll = reveal(state.scroll, line as usize, lines, height);
                            }
                        }
                        drop(state);
                        cx.tui.redraw();
                        return PaneOutcome::Handled;
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let toggled = {
                            let mut state = self.state.lock();
                            let call =
                                state.row_focus.index.and_then(|i| match state.rows.get(i) {
                                    Some(Row::Tool { call, .. })
                                    | Some(Row::Program { call, .. }) => Some(call.clone()),
                                    _ => None,
                                });
                            match call {
                                Some(call) => {
                                    state.expanded.toggle(&call);
                                    true
                                }
                                None => false,
                            }
                        };
                        if toggled {
                            cx.tui.redraw();
                            return PaneOutcome::Handled;
                        }
                        return PaneOutcome::Ignored;
                    }
                    _ => {}
                }
                let next = {
                    let state = self.state.lock();
                    self.scroll_for_key(key, &state)
                };
                match next {
                    Some(s) => {
                        {
                            let mut state = self.state.lock();
                            state.scroll = s;
                            if s.is_following() {
                                state.unseen = 0;
                            }
                        }
                        // Scrolling to the very top is the request for older rows: the pane pages
                        // rather than pretending the trajectory starts where its window does.
                        if matches!(s, Scroll::Anchored { top: 0 }) {
                            if let Some(deps) = &self.deps {
                                page_older(&deps.ledger, &self.state, self.cfg.max_rows).await;
                            }
                        }
                        cx.tui.redraw();
                        PaneOutcome::Handled
                    }
                    None => PaneOutcome::Ignored,
                }
            }
            PaneEvent::Focus(req) => {
                if let Some(deps) = &self.deps {
                    retarget(
                        &deps.agents,
                        &deps.ledger,
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
            ("up/down", "scroll"),
            ("pgup/pgdn", "page"),
            ("end", "follow the latest"),
            ("click", "expand a tool call"),
            ("ctrl+b", "branches"),
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
            let name = agents.get(&id).map(|a| a.name().to_string());
            let mut held = state.lock();
            held.agent = Some(id);
            held.agent_name = name;
            held.traj = traj;
            // A new agent ends any branch view: the override belonged to the agent left behind.
            held.home_traj = None;
            held.picker = BranchPicker::default();
            held.set_steps(steps);
            held.scroll = Scroll::Follow;
            held.unseen = 0;
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

/// The focused trajectory's branches: its `EdgeKind::Ancestor` children, each labelled a LANE if
/// an `agents` row lives on it and a FORK if none does (§4). Oldest first.
pub async fn branches_for(
    ledger: &LedgerHandle,
    agents: &AgentsHandle,
    traj: &TrajId,
) -> Vec<Branch> {
    let edges = ledger.0.edges(traj).await.unwrap_or_else(|e| {
        tracing::warn!(target: "tui.focus", %traj, error = %e, "reading the trajectory's edges failed");
        Vec::new()
    });
    // One snapshot of the roster, so the label is decided from ONE view of the world rather than
    // re-read per child.
    let lanes: Vec<(TrajId, bough_plugin_ledger::AgentName)> = agents
        .list()
        .iter()
        .map(|a| (a.traj().clone(), a.name().clone()))
        .collect();
    let lane_of = |t: &TrajId| lanes.iter().find(|(lt, _)| lt == t).map(|(_, n)| n.clone());
    let mut counted = branches_from_edges(&edges, traj, &lane_of, &|_| 0);
    for b in counted.iter_mut() {
        b.steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![b.traj.clone()],
                order: Order::SeqDesc,
                ..Default::default()
            })
            .await
            .map(|s| s.len())
            .unwrap_or(0);
    }
    counted
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
        .unwrap_or_else(|e| {
            // Reported, not swallowed: an empty trajectory and a failed read look identical on
            // screen, and Phase 1 ships a handle that CAN outlive its row.
            tracing::warn!(target: "tui.focus", %traj, error = %e, "reading the newest steps failed");
            Vec::new()
        });
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
        .unwrap_or_else(|e| {
            tracing::warn!(target: "tui.focus", error = %e, "paging older steps failed");
            Vec::new()
        });
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
    if let Scroll::Anchored { top } = held.scroll {
        held.scroll = Scroll::Anchored { top: top + added };
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

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        // `max_rows: 0` made `push_step` drain every step it was handed and `newest_steps` issue
        // `LIMIT 0`, so the trajectory rendered permanently empty with no error anywhere.
        if cfg.max_rows == 0 {
            return reject("max_rows must be > 0".to_string());
        }
        if cfg.max_tool_lines == 0 {
            return reject("max_tool_lines must be > 0".to_string());
        }
        if cfg.page_lines == 0 {
            return reject("page_lines must be > 0".to_string());
        }
        Ok(())
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

        // The recorded frame is per-process and this row owns it: unloading forgets what it drew,
        // so a reload is never checked against its predecessor's screen.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

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

        let pane = Arc::new(
            FocusPane::new(cfg.clone(), state.clone(), live.clone())
                .with_deps(agents.clone(), ledger.clone()),
        );
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
                s.lock()
                    .push_step(step.as_ref().clone(), c.max_rows, c.expand_new_tools);
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
