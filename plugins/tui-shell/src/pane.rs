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
#[derive(Clone, Debug, Default)]
pub struct HitMap {
    _private: (),
}

impl HitMap {
    /// An empty map.
    pub fn new() -> HitMap {
        todo!("WP-2")
    }
    /// Record a region. Later records win on overlap.
    pub fn push(&mut self, _rect: Rect, _id: HitId) {
        todo!("WP-2")
    }
    /// The topmost region covering a cell.
    pub fn at(&self, _col: u16, _row: u16) -> Option<HitId> {
        todo!("WP-2")
    }
}

/// What a pane renders into.
pub struct RenderCx<'a> {
    pub frame: &'a mut ratatui::Frame<'a>,
    pub area: Rect,
    pub view: &'a ShellView,
    #[allow(dead_code)] // WP-2 fills `hit`, which is the only reader.
    pub(crate) hits: &'a mut HitMap,
}

impl RenderCx<'_> {
    /// Record a clickable region for THIS frame. Later records win on overlap.
    pub fn hit(&mut self, _rect: Rect, _id: HitId) {
        todo!("WP-2")
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
pub struct PaneCx {
    pub ctx: bough_kernel::Context,
    pub tui: TuiHandle,
    /// The focused agent's live handle, when there is one.
    pub agent: Option<Agent>,
    pub at: DateTime<Utc>,
}

/// PURE: the slot rectangles, given the terminal and the live panes. A slot whose panes are all
/// gone takes ZERO rows/columns, and that is what makes the SWAP gate's reflow observable.
pub fn layout(_size: Rect, _panes: &[PaneInfo], _composer_height: u16) -> Vec<(PaneId, Rect)> {
    todo!("WP-2")
}
