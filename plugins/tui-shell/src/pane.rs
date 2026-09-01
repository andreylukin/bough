//! Invariant: a pane RENDERS FROM STATE IT ALREADY HOLDS. `Pane::render` is synchronous and
//! non-blocking — no I/O, no clock, no `block_on` — so one slow pane can never stall the frame or
//! the terminal it is drawing into. Everything that needs to await happens in `handle`.

use std::collections::HashMap;
use std::sync::Arc;

use bough_kernel::EntryId;
use bough_plugin_agents::{Agent, AgentId};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyEvent, MouseButton};
use ratatui::layout::Rect;

use crate::events::FocusRequest;
use crate::theme::Theme;
use crate::TuiHandle;

bough_util::brand_id! {
    /// A registered pane's id. Ties in layout order are broken by it, so two rows in one slot lay
    /// out deterministically.
    pub struct PaneId;
}

bough_util::brand_id! {
    /// A clickable region recorded for ONE frame. Panes mint these by convention:
    /// `tool:<call_id>`, `hit:<step_id>`, `rail:<agent_id>`.
    pub struct HitId;
}

/// Where a pane sits.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// Left rail, full height.
    Strip,
    /// The rest of the width: the focused agent.
    Main,
    /// Under `Main`: search, the panel, and Phase 8's preview/timeline/drift.
    Aux,
    /// One line above the composer: toasts and key hints. (The composition fingerprint is the
    /// panel's config tab, not this line's — the claim used to live here and nothing rendered it.)
    Status,
}

/// How much of its slot a pane asks for. A slot whose panes are all gone takes ZERO rows/columns.
#[derive(
    Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum SlotSize {
    Cells(u16),
    Percent(u16),
    /// Share of what is left, by weight.
    Fill(u16),
    /// A BREAKPOINT (phase ux1 §2.5, M13): zero columns below `collapse`, else `preferred`
    /// clamped to `min..=max`. A rail that is 34 columns at 80 and at 200 is the bug this
    /// variant removes; a slot size is the only place the rule can live, because layout — not
    /// the pane — decides how many columns the slot costs.
    Responsive {
        collapse: u16,
        preferred: u16,
        min: u16,
        max: u16,
    },
}

/// PURE: the breakpoint rule of [`SlotSize::Responsive`], spelled once so the pane that registers
/// the size and the layout that honours it cannot disagree.
pub fn responsive_width(total: u16, collapse: u16, preferred: u16, min: u16, max: u16) -> u16 {
    if total < collapse {
        return 0;
    }
    let hi = max.max(min);
    preferred.clamp(min.min(hi), hi).min(total)
}

/// What a row hands to [`TuiHandle::register_pane`].
#[derive(Clone)]
pub struct PaneSpec {
    pub id: PaneId,
    pub slot: Slot,
    /// Ties are broken by id, so two rows in one slot lay out deterministically.
    pub order: i32,
    pub size: SlotSize,
    pub title: String,
    /// `false` ⇒ never takes keyboard focus (the status line).
    pub focusable: bool,
    pub pane: Arc<dyn Pane>,
}

/// A registered pane, as `panes()` lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub id: PaneId,
    pub slot: Slot,
    pub order: i32,
    pub size: SlotSize,
    pub title: String,
    pub focusable: bool,
    /// The row that registered it. The shell's invariant reads this.
    pub owner: EntryId,
}

/// What a pane does.
#[async_trait::async_trait]
pub trait Pane: Send + Sync + 'static {
    /// SYNCHRONOUS and non-blocking: no I/O, no clock, no `block_on`. Renders from state the pane
    /// already holds, and records this frame's clickable regions through `cx.hit`.
    fn render(&self, cx: &mut RenderCx<'_>);

    /// Input routed to this pane. Async, so a pane may call `ctx.agents` / `ctx.ledger`.
    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        let _ = (ev, cx);
        PaneOutcome::Ignored
    }

    /// `("↑/↓", "scroll")` pairs for the status line and `/help`.
    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

/// This frame's clickable regions, one map per frame.
///
/// A `Vec` and not a quadtree on purpose: a frame records tens of regions, the lookup happens once
/// per click, and a later record must win on overlap — which a reverse linear scan gives for free.
#[derive(Clone, Debug, Default)]
pub struct HitMap {
    regions: Vec<(Rect, HitId)>,
}

impl HitMap {
    /// An empty map.
    pub fn new() -> HitMap {
        HitMap {
            regions: Vec::new(),
        }
    }
    /// Record a region. Later records win on overlap.
    pub fn push(&mut self, rect: Rect, id: HitId) {
        self.regions.push((rect, id));
    }
    /// The topmost region covering a cell.
    pub fn at(&self, col: u16, row: u16) -> Option<HitId> {
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| {
                col >= r.x
                    && col < r.x.saturating_add(r.width)
                    && row >= r.y
                    && row < r.y.saturating_add(r.height)
            })
            .map(|(_, id)| id.clone())
    }
    /// How many regions this frame recorded.
    pub fn len(&self) -> usize {
        self.regions.len()
    }
    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

/// The drawing surface a pane is handed.
///
/// It stands in for `ratatui::Frame`, which cannot appear behind a `&'a mut Frame<'a>` in a struct:
/// `Frame` is INVARIANT in its buffer lifetime, so that type is uninhabited from inside
/// `Terminal::draw`. The methods a pane uses are the same ones (P3-D21, this document's §5).
pub struct PaneFrame<'a> {
    buf: &'a mut ratatui::buffer::Buffer,
}

impl<'a> PaneFrame<'a> {
    /// Wrap the frame's buffer.
    pub fn new(buf: &'a mut ratatui::buffer::Buffer) -> PaneFrame<'a> {
        PaneFrame { buf }
    }
    /// The whole drawable area, not the pane's slot.
    pub fn area(&self) -> Rect {
        self.buf.area
    }
    /// Draw a widget into `area`.
    pub fn render_widget<W: ratatui::widgets::Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.buf);
    }
    /// Draw a stateful widget into `area`.
    pub fn render_stateful_widget<W: ratatui::widgets::StatefulWidget>(
        &mut self,
        widget: W,
        area: Rect,
        state: &mut W::State,
    ) {
        widget.render(area, self.buf, state);
    }
    /// The buffer itself, for a pane that writes cells directly.
    pub fn buffer_mut(&mut self) -> &mut ratatui::buffer::Buffer {
        self.buf
    }
}

/// What a pane renders into.
pub struct RenderCx<'a> {
    pub frame: PaneFrame<'a>,
    pub area: Rect,
    pub view: &'a ShellView,
    pub(crate) hits: &'a mut HitMap,
    pub(crate) report: &'a mut RowReport,
}

/// What a pane tells the shell about state only the pane can see (§2.12): its roving row focus,
/// and — for the transcript — whether the viewport is pinned to the tail. The shell folds this
/// into the NEXT frame's [`ShellView`], so a sibling pane can read it. A pane that never reports
/// leaves the documented defaults and renders exactly as before.
#[derive(Clone, Copy, Debug)]
pub struct RowReport {
    pub row_focus: Option<usize>,
    pub following: bool,
    /// How many rows an `Aux` pane wants NEXT frame (visual audit F1). `None` — the pane never
    /// said — keeps its registered [`SlotSize`], so a pane that ignores this renders exactly as
    /// before. A pane that reports `Some(0)` takes no rows until it has something to show or
    /// the keyboard is moved to it: a focused `Aux` pane always gets its registered size, which
    /// is how it comes back from zero.
    pub aux_rows: Option<u16>,
    /// What the transcript's lane is waiting on from Andrey (round 10): whether its last message
    /// was a question. `None` from a pane that never reports it.
    pub owed: Option<bool>,
}

impl Default for RowReport {
    fn default() -> RowReport {
        RowReport {
            row_focus: None,
            following: true,
            aux_rows: None,
            owed: None,
        }
    }
}

impl RenderCx<'_> {
    /// Record a clickable region for THIS frame. Later records win on overlap.
    pub fn hit(&mut self, rect: Rect, id: HitId) {
        self.hits.push(rect, id);
    }

    /// The active theme's named roles.
    pub fn theme(&self) -> &Theme {
        &self.view.theme
    }

    /// The PROSE MEASURE for this pane: `min(area.width, TuiConfig::measure_cols)` (M13). Text a
    /// human reads is wrapped to this, not to the pane width, so a 200-column terminal gets a
    /// 90-column paragraph and the rest is margin. A pane drawing a table or a rule still uses
    /// `area.width`.
    pub fn measure(&self) -> u16 {
        measure(self.area.width, self.view.measure_cols)
    }

    /// Report how many rows this `Aux` pane wants next frame (see [`RowReport::aux_rows`]).
    pub fn report_aux_rows(&mut self, rows: u16) {
        self.report.aux_rows = Some(rows);
    }

    /// Report what the lane is waiting on from Andrey (round 10).
    pub fn report_owed(&mut self, question: bool) {
        self.report.owed = Some(question);
    }

    /// Report this pane's roving state to the shell, for the next frame's [`ShellView`].
    pub fn report_rows(&mut self, row_focus: Option<usize>, following: bool) {
        self.report.row_focus = row_focus;
        self.report.following = following;
    }
}

/// Read-only shell state handed to every render. `now` is passed in; a pane never reads a clock.
#[derive(Clone, Debug)]
pub struct ShellView {
    pub focused_agent: Option<AgentId>,
    pub focused_pane: PaneId,
    /// Whether THIS pane has keyboard focus.
    pub is_focused: bool,
    pub selection: Option<Rect>,
    pub size: Rect,
    pub theme: Theme,
    pub now: DateTime<Utc>,
    pub composer_focused: bool,
    /// Whether the composer holds any typed text this frame. With `composer_focused` it is the
    /// PEEK trigger (the conversation brief, 2026-08-31): a message being written is the moment
    /// the transcript annotates itself with what that message will land on.
    pub composer_nonempty: bool,
    /// The pane's roving row focus this frame, when it has one (phase ux1 §2.1, B6). A pane that
    /// ignores it renders exactly as before.
    pub row_focus: Option<usize>,
    /// Whether the transcript is pinned to the tail (phase ux1 §2.2, B2). Reported by the
    /// pane named by [`crate::TuiConfig::transcript_pane`]; `true` when it reported nothing.
    pub following: bool,
    /// The prose measure cap in columns ([`crate::TuiConfig::measure_cols`]). Read through
    /// [`RenderCx::measure`] rather than directly.
    pub measure_cols: u16,
    /// The focused agent's NAME (`AgentId` is a fresh handle per life; the name is what a person
    /// reads). For a surface that must say who is being spoken to when the rail cannot.
    pub focused_name: Option<String>,
    /// Whether the focused agent's turn is running (the composer's `stop` chip, D7).
    pub running: bool,
    /// The focused lane's pending question, from the transcript's last report (round 10): the
    /// status line's "what do I owe" chip.
    pub owed_question: bool,
    /// No `Strip` pane has any columns this frame: the rail collapses under its `collapse_cols`
    /// (100 by default), so at 80×24 nothing on screen names the lane unless a pane reads this.
    pub rail_collapsed: bool,
    /// A notice that waits for a key is up (a command's output, `/help`, an error). The status
    /// row says `esc to close` while it is, the way it says `esc to interrupt` while a turn runs.
    pub notice_pinned: bool,
}

/// PURE: the prose measure. `min(width, cap)`; `cap` is [`crate::TuiConfig::measure_cols`], and
/// `0` means NO CAP — the pane's full width (drivability, 2026-08-31: "make the setup stretch
/// out fully"). With a cap, a 200-column terminal gets a 90-column paragraph and the rest is
/// margin (M13).
pub fn measure(width: u16, cap: u16) -> u16 {
    if cap == 0 {
        return width.max(1);
    }
    width.min(cap).max(1)
}

/// Input, as the shell routed it to one pane.
#[derive(Clone, Debug)]
pub enum PaneEvent {
    Key(KeyEvent),
    Click {
        at: (u16, u16),
        hit: Option<HitId>,
        button: MouseButton,
        clicks: u8,
    },
    Scroll {
        delta: i16,
    },
    /// Keyboard focus entered/left this pane.
    FocusChanged(bool),
    /// The focused agent changed, or a step focus was requested.
    Focus(FocusRequest),
    Tick,
}

/// What the pane did with it.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneOutcome {
    /// Not mine: the shell tries the next handler (its own keymap).
    Ignored,
    /// Handled; redraw.
    Handled,
    /// Handled; the shell moves focus.
    Focus(FocusRequest),
    /// Handled; the shell dispatches this line through `ctx.commands`.
    Command(String),
    /// Handled; the shell puts this text in the composer.
    Compose(String),
}

/// What a pane's `handle` runs against.
///
/// Deliberately NOT a `Context`. It used to carry the SHELL's, so every pane resolved services
/// through tui-shell's committed view: a pane row that declared nothing but `tui` could reach
/// `agents`, `ledger` and `commands`, which is exactly the capability boundary §0.3's
/// declared-injection rule draws. A pane does its I/O through the handles its own `apply` was
/// given.
pub struct PaneCx {
    pub tui: TuiHandle,
    /// The focused agent's live handle, when there is one.
    pub agent: Option<Agent>,
    pub at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The order panes lay out in: slot, then order, then id. Total and deterministic, so two rows in
/// one slot cannot swap places between frames.
pub fn sorted(panes: &[PaneInfo]) -> Vec<PaneInfo> {
    let mut v = panes.to_vec();
    v.sort_by(|a, b| {
        a.slot
            .cmp(&b.slot)
            .then(a.order.cmp(&b.order))
            .then(a.id.cmp(&b.id))
    });
    v
}

/// One pane's requested length along its slot's axis, against a total. `Fill` is resolved in the
/// second pass and reports zero here.
fn fixed_len(size: SlotSize, total: u16) -> u16 {
    match size {
        SlotSize::Cells(n) => n.min(total),
        SlotSize::Percent(p) => ((total as u32 * p.min(100) as u32) / 100) as u16,
        SlotSize::Fill(_) => 0,
        SlotSize::Responsive {
            collapse,
            preferred,
            min,
            max,
        } => responsive_width(total, collapse, preferred, min, max),
    }
}

/// Split `total` among `sizes`: fixed requests first, then what is left shared by `Fill` weight.
/// A pane that would get zero cells still gets zero — the caller decides whether that is a bug;
/// a SLOT with no panes at all is what takes no space, and that is decided before this runs.
fn split(total: u16, sizes: &[SlotSize]) -> Vec<u16> {
    let mut out: Vec<u16> = sizes.iter().map(|s| fixed_len(*s, total)).collect();
    let fixed: u32 = out.iter().map(|n| *n as u32).sum();
    let mut left = (total as u32).saturating_sub(fixed) as u16;
    let weights: Vec<u16> = sizes
        .iter()
        .map(|s| match s {
            SlotSize::Fill(w) => (*w).max(1),
            _ => 0,
        })
        .collect();
    let total_weight: u32 = weights.iter().map(|w| *w as u32).sum();
    if let Some(total_weight) = std::num::NonZeroU32::new(total_weight) {
        let total_weight = total_weight.get();
        let mut given = 0u16;
        let last = weights.iter().rposition(|w| *w > 0).unwrap();
        for (i, w) in weights.iter().enumerate() {
            if *w == 0 {
                continue;
            }
            let share = if i == last {
                left - given
            } else {
                ((left as u32 * *w as u32) / total_weight) as u16
            };
            out[i] = share;
            given += share;
        }
        left = 0;
    }
    // Whatever is left over when nothing asked to fill goes to the last fixed pane, so a slot
    // never renders a gap it did not ask for.
    if left > 0 {
        if let Some(last) = out.last_mut() {
            *last += left;
        }
    }
    out
}

/// Stack rectangles down `area`, one per length.
fn stack_v(area: Rect, lens: &[u16]) -> Vec<Rect> {
    let mut y = area.y;
    let mut out = Vec::with_capacity(lens.len());
    for len in lens {
        let h = (*len).min(area.y + area.height - y.min(area.y + area.height));
        out.push(Rect {
            x: area.x,
            y,
            width: area.width,
            height: h,
        });
        y = y.saturating_add(h);
    }
    out
}

/// PURE: the slot rectangles, given the terminal and the live panes. A slot whose panes are all
/// gone takes ZERO rows/columns, and that is what makes the SWAP gate's reflow observable.
///
/// Geometry: the composer takes `composer_height` rows off the bottom; `Status` takes its rows off
/// what is left; `Strip` takes its columns off the left of the remainder; `Aux` takes its rows off
/// the bottom of what remains; `Main` gets everything still standing.
/// `gutter` (phase ux1 §2.5, M9): blank columns between the Strip slot and the rest. The Strip
/// slot takes `width + gutter` columns and the pane is handed only `width`, so the blank column
/// belongs to nobody and cannot be painted over by either side.
pub fn layout(
    size: Rect,
    panes: &[PaneInfo],
    composer_height: u16,
    gutter: u16,
) -> Vec<(PaneId, Rect)> {
    layout_with(
        size,
        panes,
        composer_height,
        gutter,
        false,
        None,
        &HashMap::new(),
        None,
    )
}

/// [`layout`] with what the panes REPORTED last frame (visual audit F1): an `Aux` pane whose
/// report names a row count gets that many rows instead of its registered size — unless it is
/// the focused pane, which always gets its registered size so a collapsed pane can be opened by
/// moving the keyboard to it. Panes that never reported are laid out exactly as before.
#[allow(clippy::too_many_arguments)]
pub fn layout_with(
    size: Rect,
    panes: &[PaneInfo],
    composer_height: u16,
    gutter: u16,
    border: bool,
    focused: Option<&PaneId>,
    aux_rows: &HashMap<PaneId, u16>,
    // A rail width the user DRAGGED (the divider is a handle): wins over every pane's
    // registered size, because an explicit hand on the divider outranks a breakpoint table.
    strip_cols: Option<u16>,
) -> Vec<(PaneId, Rect)> {
    let ordered = sorted(panes);
    let of = |slot: Slot| -> Vec<PaneInfo> {
        ordered.iter().filter(|p| p.slot == slot).cloned().collect()
    };
    let mut out: Vec<(PaneId, Rect)> = Vec::new();

    let mut rest = size;
    // The composer.
    let ch = composer_height.min(rest.height);
    rest.height -= ch;

    // Status: rows off the bottom of what is left.
    let status = of(Slot::Status);
    if !status.is_empty() {
        let lens: Vec<u16> = status
            .iter()
            .map(|p| match p.size {
                SlotSize::Fill(_) => 1,
                other => fixed_len(other, rest.height).max(1),
            })
            .collect();
        let total: u16 = lens.iter().sum::<u16>().min(rest.height);
        let band = Rect {
            x: rest.x,
            y: rest.y + rest.height - total,
            width: rest.width,
            height: total,
        };
        rest.height -= total;
        for (p, r) in status.iter().zip(stack_v(band, &lens)) {
            out.push((p.id.clone(), r));
        }
    }

    // Strip: columns off the left.
    let strip = of(Slot::Strip);
    if !strip.is_empty() {
        let width = strip_cols
            .unwrap_or_else(|| {
                strip
                    .iter()
                    .map(|p| match p.size {
                        SlotSize::Fill(_) => rest.width / 5,
                        other => fixed_len(other, rest.width),
                    })
                    .max()
                    .unwrap_or(0)
            })
            .min(rest.width);
        // The gutter (M9): the SLOT costs `width + gutter` columns and the pane is handed only
        // `width`, so the blank column between the rail and the transcript belongs to NOBODY and
        // neither side can paint a text run onto the other's baseline. A collapsed rail (zero
        // columns) costs no gutter either — an empty slot takes zero, as it always has.
        let g = if width == 0 {
            0
        } else {
            gutter.min(rest.width - width)
        };
        let column = Rect {
            x: rest.x,
            y: rest.y,
            width,
            height: rest.height,
        };
        rest.x += width + g;
        rest.width -= width + g;
        // Inside the rail, panes stack vertically and share the height.
        // A rail pane's `size` names its WIDTH; its height is an equal share of the rail.
        let heights = split(column.height, &vec![SlotSize::Fill(1); strip.len()]);
        for (p, r) in strip.iter().zip(stack_v(column, &heights)) {
            out.push((p.id.clone(), r));
        }
    }

    // Aux: rows off the bottom of the remainder.
    let aux = of(Slot::Aux);
    if !aux.is_empty() {
        let lens: Vec<u16> = aux
            .iter()
            .map(|p| {
                let registered = match p.size {
                    SlotSize::Fill(_) => rest.height / 3,
                    other => fixed_len(other, rest.height),
                };
                match aux_rows.get(&p.id) {
                    // Unfocused: exactly what it asked for (zero collapses it).
                    Some(&wanted) if focused != Some(&p.id) => wanted.min(registered),
                    // Focused: what it asked for, but never nothing — the keyboard is there,
                    // so its field must be. A pane that asked for zero gets one row to type in
                    // and grows as it fills; a pane that never reported keeps its full size.
                    Some(&wanted) => wanted.clamp(1, registered),
                    None => registered,
                }
            })
            .collect();
        let total: u16 = lens.iter().sum::<u16>().min(rest.height);
        let band = Rect {
            x: rest.x,
            y: rest.y + rest.height - total,
            width: rest.width,
            height: total,
        };
        rest.height -= total;
        for (p, r) in aux.iter().zip(stack_v(band, &lens)) {
            out.push((p.id.clone(), r));
        }
    }

    // Main: everything still standing, shared by weight.
    // The border row (Andrey, 2026-08-28): one row between the conversation and the bottom bands
    // for the `━` rule, taken from the conversation so no band moves.
    if border && rest.height > 1 {
        rest.height -= 1;
    }
    let main = of(Slot::Main);
    if !main.is_empty() {
        let heights = split(
            rest.height,
            &main.iter().map(|p| p.size).collect::<Vec<_>>(),
        );
        for (p, r) in main.iter().zip(stack_v(rest, &heights)) {
            out.push((p.id.clone(), r));
        }
    }

    // Back into (slot, order, id) order, so the caller renders in the order it registered against.
    let rank: std::collections::HashMap<PaneId, usize> = ordered
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    out.sort_by_key(|(id, _)| rank[id]);
    out
}

/// The composer's rectangle for a terminal of `size`. The one place the composer's geometry is
/// decided, so `layout` and the draw loop cannot disagree about it.
pub fn composer_rect(size: Rect, composer_height: u16) -> Rect {
    let h = composer_height.min(size.height);
    Rect {
        x: size.x,
        y: size.y + size.height - h,
        width: size.width,
        height: h,
    }
}

/// PURE: the lines a notice band actually paints, given how many rows it may borrow.
///
/// Two rules, both learned the hard way when Phase 5 added seven commands and `/help` grew past
/// the band: a notice is capped by the rows above the composer AND by `notice_max_lines`, and a
/// cap that DROPS lines says so on its last row. A truncation nobody can see is indistinguishable
/// from a command that never printed the line — which is exactly how `/quit` disappeared from
/// `/help` without a single test going red.
pub fn notice_band(text: &str, cap: u16, available: u16) -> Vec<String> {
    notice_band_from(text, cap, available, 0, u16::MAX)
}

/// PURE: a notice's lines folded to `width` columns (visual audit, 80×24): a one-line error such
/// as ``unknown command `x` · Enter again sends it as a message · try /help`` was clipped
/// mid-word at the right edge, and the part that was cut was the part that said what to do.
/// Folds at spaces; a continuation keeps its line's leading indent so a table stays a table; a
/// single run longer than the width is split hard rather than lost.
pub fn wrap_notice(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.chars().count() <= width {
            out.push(line.to_string());
            continue;
        }
        let indent: String = line.chars().take_while(|c| *c == ' ').collect();
        let room = width.saturating_sub(indent.chars().count()).max(1);
        let mut current = String::new();
        for word in line.trim_start().split(' ') {
            let mut word = word.to_string();
            loop {
                let wl = word.chars().count();
                let cl = current.chars().count();
                if cl == 0 && wl > room {
                    let head: String = word.chars().take(room).collect();
                    out.push(format!("{indent}{head}"));
                    word = word.chars().skip(room).collect();
                    continue;
                }
                let need = if cl == 0 { wl } else { cl + 1 + wl };
                if need <= room {
                    if cl > 0 {
                        current.push(' ');
                    }
                    current.push_str(&word);
                } else {
                    out.push(format!("{indent}{current}"));
                    current = word.clone();
                }
                break;
            }
        }
        out.push(format!("{indent}{current}"));
    }
    out
}

/// [`notice_band`] starting `skip` lines down (visual audit F4): a long `/help` scrolls with
/// PgUp/PgDn instead of ending in a marker nothing can get past. The markers say which key
/// reaches the rest.
pub fn notice_band_from(
    text: &str,
    cap: u16,
    available: u16,
    skip: usize,
    width: u16,
) -> Vec<String> {
    let wrapped = wrap_notice(text, width);
    let all: Vec<&str> = wrapped.iter().map(String::as_str).collect();
    let room = cap.max(1).min(available) as usize;
    if room == 0 {
        return Vec::new();
    }
    if all.len() <= room {
        return all.into_iter().map(str::to_string).collect();
    }
    let skip = skip.min(all.len().saturating_sub(1));
    let mut out: Vec<String> = Vec::with_capacity(room);
    // Each marker costs a row, so it replaces a row that WOULD have fitted.
    let head_marker = skip > 0;
    let body_room = room.saturating_sub(usize::from(head_marker));
    if head_marker {
        out.push(format!("… {skip} lines above (PgUp)"));
    }
    let rest = &all[skip..];
    if rest.len() <= body_room {
        out.extend(rest.iter().map(|l| (*l).to_string()));
        return out;
    }
    let shown = body_room.saturating_sub(1);
    out.extend(rest[..shown].iter().map(|l| (*l).to_string()));
    out.push(format!("… {} more lines (PgDn)", rest.len() - shown));
    out
}

/// PURE: the furthest a notice can be scrolled — the last page still shows a full band.
pub fn notice_scroll_max(text: &str, cap: u16, available: u16, width: u16) -> usize {
    let n = wrap_notice(text, width).len();
    let room = cap.max(1).min(available) as usize;
    n.saturating_sub(room.saturating_sub(1))
        .min(n.saturating_sub(1))
}

#[cfg(test)]
mod notice_tests {
    use super::{notice_band, wrap_notice};

    #[test]
    fn a_long_line_folds_at_spaces_and_keeps_its_indent() {
        assert_eq!(
            wrap_notice("unknown command `x` · try /help", 20),
            vec!["unknown command `x`".to_string(), "· try /help".to_string()]
        );
        assert_eq!(
            wrap_notice("  key   what it does in words", 16),
            vec!["  key   what it".to_string(), "  does in words".to_string()]
        );
        // A run with no spaces is split, never dropped.
        assert_eq!(
            wrap_notice("abcdefghij", 4),
            vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
        );
        // Short lines and blank lines pass through untouched.
        assert_eq!(
            wrap_notice("a\n\nb", 4),
            vec!["a".to_string(), String::new(), "b".to_string()]
        );
    }

    #[test]
    fn a_notice_that_fits_is_painted_whole() {
        let band = notice_band("a\nb\nc", 8, 20);
        assert_eq!(
            band,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn a_notice_over_the_cap_says_how_many_lines_it_dropped() {
        let band = notice_band("a\nb\nc\nd\ne", 3, 20);
        assert_eq!(
            band,
            vec![
                "a".to_string(),
                "b".to_string(),
                "… 3 more lines (PgDn)".to_string()
            ],
            "the cap must never drop lines silently"
        );
    }

    #[test]
    fn the_rows_above_the_composer_bound_the_cap() {
        let band = notice_band("a\nb\nc\nd\ne", 50, 2);
        assert_eq!(band.len(), 2, "the band never paints over the composer");
        assert_eq!(band[1], "… 4 more lines (PgDn)");
    }
}
