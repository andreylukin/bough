//! Invariant: THIS PANE OFFERS NO SEND. Its key hints are `↑/↓ select`, `enter expand`, `y copy`,
//! and there is no code path from a key it handles to `ctx.actions`, to a network, or to anything
//! that could deliver a draft. A test asserts on the key hints AND on the rendered buffer, because
//! the absence is the whole point (§7, V4).
//!
//! It registers into `tui-shell`'s `Aux` slot as an EFFECT, listens on `ledger/step` for the two
//! draft kinds, and re-reads through `DraftsHandle::list`.
//!
//! Everything a test needs is a PURE function over state the pane already holds: `RenderCx`,
//! `PaneCx` and `TuiHandle` are only constructible inside `tui-shell`, so `render` is a thin shell
//! over [`lines`] and [`paint`] (the shape `tui-search` took, for the same reason).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_drafts::{DraftKind, DraftQuery, DraftRow, Drafts, DraftsHandle};
use bough_plugin_ledger::{Ledger, LedgerStep};
use bough_plugin_tui_shell::pane::{
    Pane, PaneCx, PaneEvent, PaneId, PaneOutcome, PaneSpec, RenderCx, Slot, SlotSize,
};
use bough_plugin_tui_shell::{Tui, TuiHandle};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-drafts";

/// The pane id this row registers under.
pub const PANE_ID: &str = "tui.drafts";

/// The pane's title. It says what the pane is FOR, because a reader must not have to infer that
/// nothing here was sent.
pub const TITLE: &str = "drafts — not sent";

/// The key hints. There is no send affordance, and the header line says so on every frame.
pub const KEY_HINTS: &[(&str, &str)] = &[("up/down", "select"), ("enter", "expand"), ("y", "copy")];

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftsPaneConfig {
    pub height_pct: u16,
    pub limit: usize,
    pub show_body_lines: usize,
}

/// Everything the pane holds between frames. [`lines`] is a pure function of it.
#[derive(Debug, Default)]
pub struct PaneState {
    pub rows: Vec<DraftRow>,
    pub selected: usize,
    pub expanded: bool,
}

/// PURE: the header line. It NAMES the boundary, because a pane full of Slack messages with no
/// caption is exactly the thing a reader misreads as an outbox.
pub fn header(n: usize) -> String {
    match n {
        0 => "drafts — nothing written yet. Nothing here was sent.".to_string(),
        1 => "1 draft — NOT sent. Andrey sends it or he does not.".to_string(),
        n => format!("{n} drafts — NOT sent. Andrey sends them or he does not."),
    }
}

/// PURE: the one summary line a draft row draws.
pub fn row_line(row: &DraftRow, selected: bool) -> String {
    let marker = if selected { ">" } else { " " };
    let kind = match row.kind {
        DraftKind::Message => "message",
        DraftKind::Ticket => "ticket",
    };
    format!(
        "{marker} {} {} → {}  {}",
        row.agent, kind, row.audience, row.subject
    )
}

/// PURE: the lines the pane paints. The selected draft's BODY is shown when expanded, clipped to
/// `show_body_lines`.
pub fn lines(state: &PaneState, show_body_lines: usize) -> Vec<String> {
    let mut out = vec![header(state.rows.len())];
    for (i, row) in state.rows.iter().enumerate() {
        let selected = i == state.selected;
        out.push(row_line(row, selected));
        if selected && state.expanded {
            for line in row.body.lines().take(show_body_lines.max(1)) {
                out.push(format!("    {line}"));
            }
        }
    }
    out
}

/// PURE: the text `y` copies — the draft itself, in the shape Andrey would paste. Copying is the
/// pane's ONLY outward move and it goes to the terminal's clipboard, never to an audience.
pub fn copy_text(row: &DraftRow) -> String {
    format!(
        "to: {}\nsubject: {}\n\n{}",
        row.audience, row.subject, row.body
    )
}

/// Paint the lines into a buffer. Split out of `render` so a test can assert on a real rendered
/// buffer without a `RenderCx`, which only `tui-shell` can build.
pub fn paint(painted: &[String], area: Rect, buf: &mut Buffer, selected_line: Option<usize>) {
    let out: Vec<Line> = painted
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let style = if i == 0 {
                Style::default().add_modifier(ratatui::style::Modifier::DIM)
            } else if Some(i) == selected_line {
                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(text.clone(), style))
        })
        .collect();
    Paragraph::new(out).render(area, buf);
}

/// PURE: which painted line the selection sits on, if any.
///
/// Only the SELECTED draft expands, and its body lines come after its own summary line, so every
/// row above it contributes exactly one line and the arithmetic is `1 + selected` (line 0 is the
/// header).
pub fn selected_line(state: &PaneState) -> Option<usize> {
    if state.rows.is_empty() {
        return None;
    }
    Some(1 + state.selected.min(state.rows.len() - 1))
}

/// The pane's own state: the rows it last read, and which is selected.
pub struct DraftsPane {
    cfg: Arc<DraftsPaneConfig>,
    drafts: DraftsHandle,
    pub state: parking_lot::Mutex<PaneState>,
}

impl DraftsPane {
    /// An empty pane over one drafts handle.
    pub fn new(cfg: Arc<DraftsPaneConfig>, drafts: DraftsHandle) -> Arc<DraftsPane> {
        Arc::new(DraftsPane {
            cfg,
            drafts,
            state: parking_lot::Mutex::new(PaneState::default()),
        })
    }

    /// Re-read from `DraftsHandle::list`. Called from `handle` and from the row's `ledger/step`
    /// listener, NEVER from `render` (§11: a pane renders from state it already holds).
    pub async fn refresh(&self) {
        let rows = self
            .drafts
            .list(&DraftQuery {
                agents: Vec::new(),
                kind: None,
                limit: Some(self.cfg.limit),
            })
            .await
            .unwrap_or_default();
        let mut st = self.state.lock();
        st.selected = st.selected.min(rows.len().saturating_sub(1));
        st.rows = rows;
    }
}

#[async_trait::async_trait]
impl Pane for DraftsPane {
    /// SYNCHRONOUS and non-blocking: renders from `rows`, which `handle` filled.
    fn render(&self, cx: &mut RenderCx<'_>) {
        let state = self.state.lock();
        invariant::record(&state.rows);
        let painted = lines(&state, self.cfg.show_body_lines);
        let sel = selected_line(&state);
        let area = cx.area;
        // Rows for what there is to show (ux-visual D-uxv-1, the search pane's rule): "nothing
        // written yet" took thirty percent of the frame at every launch. Empty, the pane keeps
        // ONE row — the line that says drafts exist and nothing left this machine is worth a
        // row from boot (§7) — and a draft landing gives it what it can fill, up to its
        // registered size.
        let wanted = (painted.len() as u16).max(1);
        drop(state);
        cx.report_aux_rows(wanted);
        paint(&painted, area, cx.frame.buffer_mut(), sel);
    }

    async fn handle(&self, ev: PaneEvent, cx: PaneCx) -> PaneOutcome {
        use crossterm::event::KeyCode;
        match ev {
            PaneEvent::Key(key) => match key.code {
                KeyCode::Down => {
                    let mut st = self.state.lock();
                    if !st.rows.is_empty() {
                        st.selected = (st.selected + 1).min(st.rows.len() - 1);
                    }
                    PaneOutcome::Handled
                }
                KeyCode::Up => {
                    let mut st = self.state.lock();
                    st.selected = st.selected.saturating_sub(1);
                    PaneOutcome::Handled
                }
                KeyCode::Enter => {
                    let mut st = self.state.lock();
                    st.expanded = !st.expanded;
                    PaneOutcome::Handled
                }
                KeyCode::Char('y') => {
                    // The ONLY outward move a key of this pane can make, and it goes to the
                    // terminal's clipboard. There is no key that reaches an audience.
                    let text = {
                        let st = self.state.lock();
                        st.rows.get(st.selected).map(copy_text)
                    };
                    match text {
                        None => PaneOutcome::Ignored,
                        Some(text) => {
                            cx.tui.copy(&text).await;
                            PaneOutcome::Handled
                        }
                    }
                }
                _ => PaneOutcome::Ignored,
            },
            // A click selects. It does not send, because there is nothing to send with.
            PaneEvent::Click { at: (_, row), .. } => {
                let mut st = self.state.lock();
                let index = (row as usize).saturating_sub(1);
                if index < st.rows.len() {
                    st.selected = index;
                    return PaneOutcome::Handled;
                }
                PaneOutcome::Ignored
            }
            _ => PaneOutcome::Ignored,
        }
    }

    fn key_hints(&self) -> Vec<(&'static str, &'static str)> {
        KEY_HINTS.to_vec()
    }
}

/// The row.
pub struct DraftsPanePlugin;

#[async_trait::async_trait]
impl Plugin for DraftsPanePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DraftsPaneConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tui", "drafts", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.height_pct == 0 || cfg.height_pct > 100 {
            return reject(format!(
                "height_pct must be in 1..=100; got {}",
                cfg.height_pct
            ));
        }
        // `limit: 0` is a pane that shows Andrey no draft, silently.
        if cfg.limit == 0 {
            return reject("limit must be greater than zero".to_string());
        }
        Ok(())
    }

    /// Register the pane as an effect, then subscribe to `ledger/step` so a draft written in a
    /// wake appears without Andrey touching anything.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let drafts = ctx
            .get::<Drafts>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // Declared and resolved, so the row's injection is honest even though the pane reads the
        // ledger only through `drafts`.
        let _ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry, e))?;

        let pane = DraftsPane::new(cfg, (*drafts).clone());
        pane.refresh().await;

        // The recorded frame is per-process and this row owns it: unloading forgets what it drew.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::forget);
            Ok(())
        })
        .await?;

        let watcher = pane.clone();
        let redraw: TuiHandle = (*tui).clone();
        ctx.on::<LedgerStep, _, _>(move |step| {
            let watcher = watcher.clone();
            let redraw = redraw.clone();
            async move {
                let kind = step.kind.as_str();
                if kind != bough_plugin_drafts::DRAFT_MESSAGE
                    && kind != bough_plugin_drafts::DRAFT_TICKET
                {
                    return;
                }
                watcher.refresh().await;
                redraw.redraw();
            }
        })
        .await?;

        tui.register_pane(
            &ctx,
            PaneSpec {
                id: PaneId::new(PANE_ID),
                slot: Slot::Aux,
                order: 10,
                size: SlotSize::Percent(pane.cfg.height_pct),
                title: TITLE.into(),
                focusable: true,
                pane,
            },
        )
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(DraftsPanePlugin);
