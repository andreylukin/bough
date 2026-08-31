//! Invariant: NO STEP IS RENDERED TWICE. The live tail (what has streamed but not yet flushed to
//! `thought/text`) and the durable rows never overlap: the trailing step renders `live` whenever
//! `live.len() >= durable.len()` and the durable text otherwise (P3-D12), which makes the handover
//! flicker-free without any coordination between the `llm/stream` tee and the `ledger/step`
//! listener — two listeners that race by construction.
//!
//! This pane IS §11's `trajectory` pane (P3-D4): it owns the live tail AND the scrollback.

pub mod branches;
pub mod context;
pub mod draft;
pub mod expand;
pub mod hit;
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
    AgentName, Ledger, LedgerHandle, LedgerStep, Order, Seq, Step, StepId, StepQuery, TrajId,
};
use bough_plugin_llm::LlmStreamEvent;
use bough_plugin_projection::{AssembleRequest, Projection, ProjectionHandle};
use bough_plugin_tui_render::ToolCallView;
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{FocusRequest, PaneId, Theme, Tui, TuiHandle, TuiKeyEvent};
use crossterm::event::{KeyCode, KeyEvent};
use parking_lot::Mutex;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub use branches::{branches_from_edges, Branch, BranchPicker, PickerOutcome};
pub use expand::{call_of_hit, hit_for_call, Expanded};
pub use program::{program_header, program_lines, ProgramError, ProgramSub, ProgramView, RUN_TOOL};
pub use rowfocus::{focus_marker, RowFocus};
pub use rows::{rows_from_steps, trailing_durable, trailing_text_row, trailing_text_rows, Row};
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
    /// The truth machinery (round 11, amended by the conversation brief, 2026-08-31): the pane
    /// is a CHAT whose truth surfaces on demand, the peek while a message is typed and `^p` for
    /// the full context view. `false` is the plain transcript: no assembly, no bands, no tray.
    #[serde(default = "default_true")]
    pub context: bool,
    /// Debounce on `ledger/step` before re-assembling. Assembly is deterministic but not free.
    #[serde(default = "default_refresh_ms")]
    pub context_refresh_ms: u64,
}

fn default_true() -> bool {
    true
}

fn default_refresh_ms() -> u64 {
    150
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
    /// The view's `now` as of the last frame, stamped by `render` so the pure line builder can
    /// say how long the in-flight call has run (round 5).
    pub now: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether the focused agent's turn is running, stamped by `render` (round 6, queued tags).
    pub running: bool,
    /// Whether this pane has the keyboard, stamped by `render` (round 10: the row marker is
    /// drawn only then).
    pub keyboard_here: bool,
    /// The focused agent's NAME, for the speaker label on its text (visual audit F2). `None`
    /// until `retarget` has looked it up; a text row then carries no label rather than a guess.
    pub agent_name: Option<String>,
    /// The model's next context, as last assembled (round 11). Empty until the first refresh.
    pub context: context::ContextView,
    /// `^p` (the conversation brief, 2026-08-31): the FULL context view pinned on; off, the pane
    /// is a chat and truth surfaces in the peek while a message is typed.
    pub context_pinned: bool,
}

impl FocusState {
    /// Each held step's seq, for the context plan.
    pub fn seq_of(&self) -> std::collections::HashMap<StepId, Seq> {
        self.steps.iter().map(|s| (s.id.clone(), s.seq)).collect()
    }

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
    /// Re-assembles the context (round 11). `None` in a test that drives the pane directly.
    refresher: Option<Arc<Refresher>>,
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
            refresher: None,
        }
    }

    /// The context refresher, attached by `apply`.
    pub(crate) fn with_refresher(mut self, r: Arc<Refresher>) -> FocusPane {
        self.refresher = Some(r);
        self
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
        Vec<hit::Hit>,
    ) {
        let depth = self.depth(state, false, false);
        let (lines, headers, hits, _) = self.lines_with_rows(state, live, width, theme, depth);
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
        depth: Option<context::Depth>,
    ) -> (
        Vec<Line<'static>>,
        Vec<(bough_plugin_llm::ToolCallId, u16)>,
        Vec<hit::Hit>,
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
        let cx_is_focused = state.keyboard_here;
        let durable = trailing_durable(&state.rows);
        // Since P5-D14 the flushes of one step index are already ONE row, so the only choice left
        // here is P3-D12's: the trailing row draws either its durable text or the live tail.
        let trailing = rows::trailing_text_row(&state.rows);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut headers = Vec::new();
        let mut hits_out: Vec<hit::Hit> = Vec::new();

        // The window is not the trajectory: `max_rows` steps back is as far as this pane holds,
        // and saying so is what stops an elided beginning from reading as the whole story.
        if state.more_above {
            lines.push(Line::styled(
                "\u{2026} older steps above (PgUp)",
                Style::default().fg(theme.dim),
            ));
        }
        let now = state.now.unwrap_or_else(chrono::Utc::now);

        // In a chat the STANDING block lives at the TOP OF THE SCROLLBACK (the conversation
        // brief): the room's fixtures, above the oldest loaded row, seen by scrolling to the
        // beginning. Only the full view (`^p`) pins it. Skipped while older pages are still
        // unloaded, where it would sit above history that is not the beginning.
        if matches!(depth, Some(context::Depth::Chat | context::Depth::Peek))
            && !state.more_above
            && state.context.is_on()
        {
            let (standing, s_hits) = context::standing_lines(
                &state.context,
                (state.height as usize / 3).max(3),
                width,
                theme,
                lines.len() as u16,
                now,
            );
            hits_out.extend(s_hits);
            lines.extend(standing);
        }

        // The empty transcript says what it is for (visual audit F15): a first launch used to be
        // a blank pane — nothing said the machine was ready, whose conversation this was, or that
        // the composer below was where to start. Only when there is genuinely nothing: no rows,
        // no live tail, and no older page the window has scrolled past.
        if state.rows.is_empty() && live.text.is_empty() && !state.more_above {
            let opener = match &state.agent_name {
                Some(name) => {
                    format!("Nothing here yet \u{2014} {name} is waiting for your first message.")
                }
                None => "Nothing here yet.".to_string(),
            };
            lines.push(Line::styled(opener, Style::default().fg(theme.fg)));
            lines.extend(
                bough_plugin_tui_render::wrap(
                    "Type below and press enter \u{b7} / for commands \u{b7} ? for help",
                    width,
                )
                .into_iter()
                .map(|l| Line::styled(l, Style::default().fg(theme.dim))),
            );
        }

        // Where each turn's agent rows begin, for the `✎ changed …` line at its end (round 6).
        let mut turn_start: usize = 0;
        let changed_line = |rows: &[Row], theme: &Theme| -> Option<Line<'static>> {
            let files = rows::changed_files(rows);
            if files.is_empty() {
                return None;
            }
            Some(Line::styled(
                format!("\u{270e} changed {}", files.join(" \u{b7} ")),
                Style::default().fg(theme.added),
            ))
        };
        let folds = rows::retry_folds(&state.rows);
        let mut row_lines: Vec<u16> = vec![0; state.rows.len()];
        let mut skip_until: Option<usize> = None;
        // The truth plan (round 11, the conversation brief): which rows are IN the next context,
        // which a tier summarises, which are gone, which are mail, and what the depth says
        // between them.
        let plan = depth.map(|d| {
            let seq_of = state.seq_of();
            context::plan(&state.context, &state.rows, &seq_of, now, width, theme, d)
        });
        let emit = |pieces: &[context::Piece],
                    lines: &mut Vec<Line<'static>>,
                    hits: &mut Vec<hit::Hit>| {
            for piece in pieces {
                if let Some((id, w)) = &piece.hit {
                    hits.push(hit::Hit {
                        id: id.clone(),
                        line: lines.len() as u16,
                        x: 0,
                        width: *w,
                    });
                }
                lines.push(piece.line.clone());
            }
        };
        for (i, row) in state.rows.iter().enumerate() {
            if let Some(p) = &plan {
                if p.mail[i] {
                    row_lines[i] = lines.len().saturating_sub(1) as u16;
                    continue;
                }
                if let Some(pieces) = p.before.get(&i) {
                    emit(pieces, &mut lines, &mut hits_out);
                }
                if !p.show[i] {
                    row_lines[i] = lines.len().saturating_sub(1) as u16;
                    continue;
                }
            }
            // Failed attempts folded under the call that succeeded (round 8): one line, a
            // click opens them. A folded row's line is the fold line, so the row focus and the
            // click map still have somewhere to point.
            if let Some(until) = skip_until {
                if i < until {
                    row_lines[i] = lines.len().saturating_sub(1) as u16;
                    continue;
                }
                skip_until = None;
            }
            // An empty program draws nothing (round 9); its line is the previous line so the
            // row focus and the click map still have somewhere to point.
            if rows::is_empty_program(row) {
                row_lines[i] = lines.len().saturating_sub(1) as u16;
                continue;
            }
            if let Some(fold) = folds.iter().find(|f| f.start == i) {
                let key = retry_key(&state.rows[fold.end]);
                let opened = state.expanded.is_expanded(&key);
                let (marker, verb) = if opened {
                    ("\u{25be}", "close")
                } else {
                    ("\u{25b8}", "open")
                };
                let noun = if fold.attempts == 1 {
                    "attempt"
                } else {
                    "attempts"
                };
                let text = format!("{marker} {} failed {noun} \u{b7} {verb}", fold.attempts);
                hits_out.push(hit::Hit {
                    id: retry_hit(&key),
                    line: lines.len() as u16,
                    x: 0,
                    width: text.chars().count() as u16,
                });
                lines.push(Line::styled(text, Style::default().fg(theme.warn)));
                if !opened {
                    row_lines[i] = (lines.len() - 1) as u16;
                    skip_until = Some(fold.end);
                    continue;
                }
            }
            // A new message from Andrey closes the previous turn: say what that turn changed.
            if matches!(row, Row::Andrey { .. }) && i > turn_start {
                if let Some(l) = changed_line(&state.rows[turn_start..i], theme) {
                    lines.push(l);
                }
                turn_start = i;
            }
            let flash = state.anchor.as_ref() == Some(row.step());
            let row_start = lines.len();
            row_lines[i] = row_start as u16;
            // The speaker (visual audit F2): Andrey's rows said `andrey:` and the agent's said
            // nothing, so the two halves of the conversation were told apart by nothing but
            // position. The name opens each span the agent acts in — words or a tool call. A
            // reasoning row hidden by config still opens the span: the label lands above the
            // first VISIBLE line of the span, not on nothing.
            if rows::opens_speech(&state.rows, i) {
                if let Some(name) = &state.agent_name {
                    lines.push(label(name, theme.accent));
                }
            }
            match row {
                Row::Queued { text, .. } => {
                    // Sent while a turn was running (round 8): the tag says it waits.
                    lines.push(Line::from(vec![
                        Span::styled(
                            "andrey:",
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" \u{b7} queued", Style::default().fg(theme.dim)),
                    ]));
                    lines.extend(bough_plugin_tui_render::markdownish(text, width, theme));
                }
                Row::Andrey { text, .. } => {
                    lines.push(label("andrey", theme.accent));
                    lines.extend(bough_plugin_tui_render::markdownish(text, width, theme));
                }
                Row::Mail { from, subject, .. } => {
                    lines.push(mail_line(from, subject, theme));
                }
                Row::Text { text, .. } => {
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
                Row::Draft {
                    draft,
                    kind,
                    audience,
                    subject,
                    body,
                    ..
                } => {
                    let opened = state.expanded.is_expanded(&draft_key(draft));
                    let (card, regions) = draft::card(
                        draft,
                        kind,
                        audience,
                        subject,
                        body,
                        opened,
                        lines.len() as u16,
                        width,
                        theme,
                    );
                    lines.extend(card);
                    hits_out.extend(regions);
                }
                Row::Other { kind, .. } => {
                    // TOTAL: a type this binary does not know still gets a line, and never a panic.
                    lines.push(Line::styled(
                        format!("· {kind}"),
                        Style::default().fg(theme.dim),
                    ));
                }
            }
            // A row on screen that the model will NOT see (an unfolded tier's, the dropped
            // fold's) is dimmed span by span (the conversation brief): the band above it says so
            // in words, the dim says it at a glance. Span-level, for the same reason the flash
            // is: a span's own `fg` is patched OVER the line style by ratatui.
            if plan.as_ref().is_some_and(|p| p.summarized[i]) {
                for line in lines.iter_mut().skip(row_start) {
                    for span in line.spans.iter_mut() {
                        span.style = span.style.fg(theme.dim);
                    }
                }
            }
            // The roving row focus, drawn NEVER BY COLOUR ALONE (audit delight 3): a marker glyph
            // in the gutter column of every line of the row, and a `sel_bg` fill behind it.
            // …and only while this pane HAS the keyboard (round 10): a marker that stayed after
            // Esc returned the keys to the composer said the next keystroke would land here.
            if state.row_focus.is_on(i) && cx_is_focused {
                for (n, line) in lines.iter_mut().enumerate().skip(row_start) {
                    let marker = if n == row_start { focus_marker() } else { ' ' };
                    line.spans.insert(
                        0,
                        Span::styled(marker.to_string(), Style::default().fg(theme.accent)),
                    );
                    // PATCHED, not replaced: a label, a rule and the welcome carry their colour
                    // on the LINE style, and `Line::style` swapped it for the fill alone — so the
                    // focused `sol:` went pale under the highlight (visual audit, light theme).
                    *line = line.clone().patch_style(Style::default().bg(theme.sel_bg));
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
        // The mail band, last, where the model reads it (D11-3): every mail row under one band.
        // Only the FULL view draws it inline; the chat and the peek hold the queue in the tray.
        if let Some(p) = &plan {
            if depth == Some(context::Depth::Full) {
                for (i, row) in state.rows.iter().enumerate() {
                    if !p.mail[i] {
                        continue;
                    }
                    if let Some(pieces) = p.before.get(&i) {
                        emit(pieces, &mut lines, &mut hits_out);
                    }
                    row_lines[i] = lines.len() as u16;
                    if let Row::Mail { from, subject, .. } = row {
                        lines.push(context::washed(
                            mail_line(from, subject, theme),
                            width,
                            theme.wash_mail,
                        ));
                    }
                }
            }
            emit(&p.trailing, &mut lines, &mut hits_out);
        }
        // The last turn's `✎ changed …` line, once nothing is in flight (round 6).
        let last_in_flight = state
            .now
            .and_then(|now| rows::running_line(&state.rows, now))
            .is_some()
            || !live.text.is_empty();
        if !last_in_flight && turn_start < state.rows.len() {
            if let Some(l) = changed_line(&state.rows[turn_start..], theme) {
                lines.push(l);
            }
        }
        // The LIVE LINE (round 5): while a call is in flight, the bottom of the agent's span says
        // what is running and for how long — `▸ running · bash cargo test · 12s` — so a reader
        // who looks away and back knows what the wait is for without reading the status line.
        if let Some(live_line) = state
            .now
            .and_then(|now| rows::running_line(&state.rows, now))
        {
            lines.push(Line::styled(live_line, Style::default().fg(theme.accent)));
        }
        // The live tail of a turn whose first `thought/text` has not landed yet: without this the
        // first token of every answer would be invisible until the first flush.
        if trailing.is_none() && !live.text.is_empty() {
            // The label opens here too (F2): the first streamed words are the agent speaking,
            // and waiting for the first durable flush to say so would make the name pop in
            // mid-sentence.
            if !state.rows.last().is_some_and(rows::is_agent_row) {
                if let Some(name) = &state.agent_name {
                    lines.push(label(name, theme.accent));
                }
            }
            lines.extend(bough_plugin_tui_render::markdownish(
                &live.text, width, theme,
            ));
        }
        // The mail TRAY (chat and peek): the queue waits above the composer, never inline.
        if matches!(depth, Some(context::Depth::Chat | context::Depth::Peek)) {
            if let Some(p) = &plan {
                let pieces =
                    context::tray_pieces(&state.context, &state.rows, &p.mail, width, theme);
                emit(&pieces, &mut lines, &mut hits_out);
            }
        }
        if depth == Some(context::Depth::Full) {
            lines.push(context::footer(&state.context, now, theme));
        }
        (lines, headers, hits_out, row_lines)
    }

    /// The truth depth this frame (the conversation brief, 2026-08-31): `None` is the plain
    /// transcript (config off, or no assembly yet); `Full` is pinned by `^p`; `Peek` while a
    /// message is being typed; `Chat` otherwise.
    pub fn depth(
        &self,
        state: &FocusState,
        composer_focused: bool,
        composer_nonempty: bool,
    ) -> Option<context::Depth> {
        if !self.cfg.context || !state.context.is_on() {
            return None;
        }
        Some(if state.context_pinned {
            context::Depth::Full
        } else if composer_focused && composer_nonempty {
            context::Depth::Peek
        } else {
            context::Depth::Chat
        })
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

/// The disclosure key a retry fold toggles: keyed by the SUCCESSFUL call it sits under.
fn retry_key(success: &Row) -> bough_plugin_llm::ToolCallId {
    let id = match success {
        Row::Tool { call, .. } | Row::Program { call, .. } => call.to_string(),
        other => other.step().to_string(),
    };
    bough_plugin_llm::ToolCallId::new(format!("retries:{id}"))
}

const RETRY_HIT_PREFIX: &str = "retries:";

fn retry_hit(key: &bough_plugin_llm::ToolCallId) -> bough_plugin_tui_shell::pane::HitId {
    bough_plugin_tui_shell::pane::HitId::new(format!("{RETRY_HIT_PREFIX}{key}"))
}

/// The disclosure key a draft card's `open` toggles, in the same set tool calls use.
fn draft_key(draft: &str) -> bough_plugin_llm::ToolCallId {
    bough_plugin_llm::ToolCallId::new(format!("draft:{draft}"))
}

pub(crate) fn mail_line(from: &str, subject: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("\u{2709} ", Style::default().fg(theme.warn)),
        Span::styled(from.to_string(), Style::default().fg(theme.warn)),
        Span::raw("  "),
        Span::styled(subject.to_string(), Style::default().fg(theme.fg)),
    ])
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
        state.now = Some(cx.view.now);
        state.running = cx.view.running;
        state.keyboard_here = cx.view.is_focused;
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
        // The truth depth this frame (the conversation brief, 2026-08-31): the chat by default,
        // the peek while a message is typed, the full view while `^p` has it pinned.
        let depth = self.depth(&state, cx.view.composer_focused, cx.view.composer_nonempty);
        // The STANDING block, PINNED (round 11, D11-2), in the FULL view only: the folded head,
        // the digest and the pins never scroll. It takes up to a third of the pane (pins fold to
        // titles past that) and, opened, what it needs. In the chat and the peek the same block
        // lives at the top of the scrollback instead (`lines_with_rows`).
        let (standing, standing_hits) = if depth == Some(context::Depth::Full) {
            context::standing_lines(
                &state.context,
                (area.height as usize / 3).max(3),
                measure,
                &theme,
                0,
                cx.view.now,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let standing_h = (standing.len() as u16).min(area.height.saturating_sub(4));
        let standing_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: standing_h,
        };
        let area = Rect {
            x: area.x,
            y: area.y + standing_h,
            width: area.width,
            height: area.height - standing_h,
        };
        state.height = area.height;
        let (lines, headers, hits_out, row_lines) =
            self.lines_with_rows(&state, &live, measure, &theme, depth);
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
        // What this lane is waiting on from Andrey (round 10), for the status chip.
        let owed = rows::owed(&state.rows, cx.view.running);
        cx.report_owed(owed.question);
        let top = state.scroll.top(lines.len(), area.height);
        // The unread affordance (phase ux1 §2.2, B2): scrolled up with rows arriving below, the
        // pane SAYS how many and what to press. Nothing is drawn while following, so a reader at
        // the tail never sees chrome for a state they are not in.
        let badge = if state.scroll.is_following() {
            None
        } else if state.unseen > 0 {
            Some(format!("\u{2193} {} new · End", state.unseen))
        } else {
            // Scrolled up with nothing new (round 8): the badge still says where the newest is.
            Some("\u{2191} older · End for newest".to_string())
        };
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
        // Row starts in LINE order: with the context view a row can be drawn out of index order
        // (mail goes last), so "the next row's first line" is the next START, not `i + 1`.
        let mut starts: Vec<u16> = row_lines.clone();
        starts.sort_unstable();
        if standing_h > 0 {
            cx.frame.render_widget(
                Paragraph::new(
                    standing
                        .into_iter()
                        .take(standing_h as usize)
                        .collect::<Vec<_>>(),
                ),
                standing_area,
            );
            for hit in standing_hits {
                if hit.line >= standing_h {
                    continue;
                }
                cx.hit(
                    Rect {
                        x: standing_area.x + hit.x.min(standing_area.width),
                        y: standing_area.y + hit.line,
                        width: hit.width.min(standing_area.width.saturating_sub(hit.x)),
                        height: 1,
                    },
                    hit.id,
                );
            }
        }
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
            let last = starts
                .iter()
                .copied()
                .find(|&s| s > first)
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
        for hit in hits_out {
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
            PaneEvent::Click { hit, .. } => {
                // A click ACTS and nothing else (the TUI brief, D1): it opens a program, presses
                // a card's button, selects text by dragging. It leaves no row marker — the marker
                // is the KEYBOARD's row, and a click never moves the keyboard (B1), so a marker
                // placed by a click said the keys were somewhere they were not.
                // The context view's own buttons (round 11): the folded head, the standing
                // block, a tier's rows, the steps not in the context.
                if let Some(h) = hit.as_ref() {
                    if h.as_str().starts_with(context::HIT_PREFIX) {
                        let toggled = self.state.lock().context.toggle(h);
                        if toggled {
                            cx.tui.redraw();
                            return PaneOutcome::Handled;
                        }
                    }
                }
                // A retry fold's line (round 8): open or close the failed attempts.
                if let Some(rest) = hit
                    .as_ref()
                    .and_then(|h| h.as_str().strip_prefix(RETRY_HIT_PREFIX))
                {
                    let key = bough_plugin_llm::ToolCallId::new(rest);
                    let mut state = self.state.lock();
                    let mut expanded = std::mem::take(&mut state.expanded);
                    expanded.toggle(&key);
                    state.expanded = expanded;
                    drop(state);
                    cx.tui.redraw();
                    return PaneOutcome::Handled;
                }
                // A draft card's button (D6): copy puts the draft on the clipboard, open shows
                // the whole body in place. Neither sends anything.
                if let Some((id, action)) = hit.as_ref().and_then(draft::action_of_hit) {
                    let text = {
                        let state = self.state.lock();
                        state.rows.iter().find_map(|r| match r {
                            Row::Draft {
                                draft,
                                audience,
                                subject,
                                body,
                                ..
                            } if *draft == id => Some(draft::copy_text(audience, subject, body)),
                            _ => None,
                        })
                    };
                    match action {
                        draft::DraftAction::Copy => {
                            if let Some(text) = text {
                                cx.tui.copy(&text).await;
                            }
                        }
                        draft::DraftAction::Open => {
                            let mut state = self.state.lock();
                            let mut expanded = std::mem::take(&mut state.expanded);
                            expanded.toggle(&draft_key(&id));
                            state.expanded = expanded;
                        }
                    }
                    cx.tui.redraw();
                    return PaneOutcome::Handled;
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
                if let Some(r) = &self.refresher {
                    r.arm();
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
            ("ctrl+p", "context view"),
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
            // A worker's context is its own bands (D11-6): nothing of the last agent's shows
            // under the new name while the first refresh is in flight.
            held.context = context::ContextView::default();
        }
    }
    if let Some(step) = req.step.clone() {
        let mut held = state.lock();
        if let Some(row) = held.row_of(&step) {
            held.scroll = Scroll::anchored_on(row);
        }
        // The row marker lands on the hit too (round 10, keyboard-only): a jump the marker did
        // not follow left Up/Down/Enter working from wherever they were before the search.
        held.row_focus = RowFocus::on_step(&held.rows, &step);
        held.anchor = Some(step);
    }
}

/// Re-assembles the focused agent's context (round 11) through the SAME `assemble` the wake flow
/// calls (`tui-preview`'s rule): what the pane labels is what the model reads, by construction.
/// One refresh at a time, debounced by `refresh_ms`; a request that lands during one re-arms it.
pub struct Refresher {
    state: Arc<Mutex<FocusState>>,
    tui: TuiHandle,
    projection: ProjectionHandle,
    refresh_ms: u64,
}

impl Refresher {
    pub fn new(
        state: Arc<Mutex<FocusState>>,
        tui: TuiHandle,
        projection: ProjectionHandle,
        refresh_ms: u64,
    ) -> Refresher {
        Refresher {
            state,
            tui,
            projection,
            refresh_ms,
        }
    }

    /// Ask for a rebuild. Returns at once; the frame follows.
    pub fn arm(self: &Arc<Self>) {
        {
            let mut s = self.state.lock();
            if s.context.refreshing {
                s.context.dirty = true;
                return;
            }
            s.context.refreshing = true;
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(me.refresh_ms)).await;
                let agent = me.state.lock().agent_name.clone();
                let Some(agent) = agent else {
                    me.state.lock().context.refreshing = false;
                    return;
                };
                let now = chrono::Utc::now();
                let result = me
                    .projection
                    .0
                    .assemble(&AssembleRequest {
                        agent: AgentName::new(&agent),
                        wake: None,
                        at: now,
                        budget: None,
                        as_of: None,
                    })
                    .await;
                let mut s = me.state.lock();
                match result {
                    Ok(a) => {
                        if s.agent_name.as_deref() == Some(agent.as_str()) {
                            s.context.apply(&a, now);
                        } else {
                            s.context.dirty = true;
                        }
                    }
                    // Before the trajectory exists (a cold boot's first frames) there is no
                    // context to show; the plain transcript stands until the next step re-arms.
                    Err(e) => {
                        tracing::debug!(target: "tui.focus", error = %e, "assembling the context failed")
                    }
                }
                if s.context.dirty {
                    s.context.dirty = false;
                    continue;
                }
                s.context.refreshing = false;
                break;
            }
            me.tui.redraw();
        });
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
        Inject::required(["tui", "agents", "ledger", "llm", "projection"])
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
        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let projection = ProjectionHandle(projection.0.clone());

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

        let refresher = Arc::new(Refresher::new(
            state.clone(),
            tui.clone(),
            projection,
            cfg.context_refresh_ms,
        ));
        if cfg.context {
            refresher.arm();
        }
        let pane = Arc::new(
            FocusPane::new(cfg.clone(), state.clone(), live.clone())
                .with_deps(agents.clone(), ledger.clone())
                .with_refresher(refresher.clone()),
        );
        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new("tui.focus"),
                slot: Slot::Main,
                order: 0,
                size: SlotSize::Fill(1),
                // "conversation" (round 7): one name on screen — the key hints already said
                // "search the conversation"; `/help` and the branch picker said "trajectory".
                title: "conversation".into(),
                focusable: true,
                pane: pane.clone(),
            },
        )
        .await?;

        // The durable half: every step of the focused trajectory becomes a row.
        let (s, t, c, r) = (state.clone(), tui.clone(), cfg.clone(), refresher.clone());
        ctx.on::<LedgerStep, _, _>(move |step| {
            let (s, t, c, r) = (s.clone(), t.clone(), c.clone(), r.clone());
            async move {
                let mine = s.lock().traj.as_ref() == Some(&step.traj);
                if !mine {
                    return;
                }
                s.lock()
                    .push_step(step.as_ref().clone(), c.max_rows, c.expand_new_tools);
                if c.context {
                    r.arm();
                }
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

        // `^p` without touching the shell's keymap: the `tui/key` waterfall (P3-D18), the same
        // seam the panel's `^t` rides. Pin and unpin the FULL context view; the chat and the
        // peek need no key, they are the resting and the typing state.
        let (s, t) = (state.clone(), tui.clone());
        ctx.on_waterfall::<TuiKeyEvent, _, _>(move |mut dispatch, next| {
            let (s, t) = (s.clone(), t.clone());
            async move {
                let is_toggle = matches!(dispatch.key.code, KeyCode::Char('p'))
                    && dispatch
                        .key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL);
                if is_toggle && !dispatch.handled {
                    dispatch.handled = true;
                    let mut held = s.lock();
                    held.context_pinned = !held.context_pinned;
                    drop(held);
                    t.redraw();
                }
                next.run(dispatch).await
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

        let (l, t, r, c) = (live.clone(), tui.clone(), refresher.clone(), cfg.clone());
        ctx.on::<AgentWake, _, _>(move |ev| {
            let (l, t, r, c) = (l.clone(), t.clone(), r.clone(), c.clone());
            async move {
                if ev.phase == Phase::End {
                    l.lock().clear();
                    // A wake's end is when a digest or a tier can have sealed (round 11).
                    if c.context {
                        r.arm();
                    }
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
