//! Invariant: a pane RENDERS FROM STATE IT ALREADY HOLDS. `Pane::render` is synchronous and
//! non-blocking — no I/O, no clock, no `block_on` — so one slow pane can never stall the frame or
//! the terminal it is drawing into. Everything that needs to await happens in `handle`.

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
    /// Under `Main`: search, and Phase 8's preview/timeline/drift.
    Aux,
    /// One line above the composer: toasts, key hints, the composition fingerprint.
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
pub fn layout(size: Rect, panes: &[PaneInfo], composer_height: u16) -> Vec<(PaneId, Rect)> {
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
        let width = strip
            .iter()
            .map(|p| match p.size {
                SlotSize::Fill(_) => rest.width / 5,
                other => fixed_len(other, rest.width),
            })
            .max()
            .unwrap_or(0)
            .min(rest.width);
        let column = Rect {
            x: rest.x,
            y: rest.y,
            width,
            height: rest.height,
        };
        rest.x += width;
        rest.width -= width;
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
            .map(|p| match p.size {
                SlotSize::Fill(_) => rest.height / 3,
                other => fixed_len(other, rest.height),
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
