//! ONE tabbed panel. Every non-chat surface is a tab of it (port of
//! `src/tui/components/Panel.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **there is exactly one place that is not the
//! chat.** One [`PanelState`], one reducer, and a tab that is either showing
//! or not. Adding a surface is adding a row to `keys::TABS`; it cannot add a
//! new way to be open.
//!
//! SECOND — **leaving the theme tab reverts an uncommitted preview.** That is
//! wired *inside* [`reduce_panel`] rather than at each exit key, because there
//! are five ways to leave a tab — ^t, escape, a chord, tab, shift-tab — and a
//! revert remembered at four of them is a TUI that silently keeps the theme
//! you last scrolled past. `cancel()` is idempotent, so the reducer calls it
//! on every departure from `theme` and on nothing else.
//!
//! THIRD — **the keymap is data, and it is not this file's data.** `TABS`
//! lives in `tui/keys.rs`; [`panel_action_for`] is the only translation
//! between the two: `Command` in, `PanelAction` out.
//!
//! ROW BUDGETS ARE CLAIMS. The TS tree's 100x12 corruption (rows rendering as
//! character-level interleavings of two different lines) came from a body that
//! emitted more rows than its box had. The ratatui equivalent of the two-box
//! fix is stated once, here: **a tab body must never emit more rows than its
//! budget, and the container truncates rather than scales** — every
//! `render_*` below paints `min(lines, area.height)` rows.

pub mod changes;
pub mod host;
pub mod mcp;
pub mod model;
pub mod skills;
pub mod tree;
pub mod workflows;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::ansi::{truncate_ansi, width};
use crate::keys::{tab_for_command, Command, PanelTab, PANEL_TABS, TABS};

use super::{ACCENT, BORDER, PANEL};

// ---------------------------------------------------------------------------
// The state machine (pure, but for the theme preview it must cancel)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelState {
    pub open: bool,
    pub tab: PanelTab,
}

/// The tree is the panel's home tab: it is the switcher AND the history, so it
/// is what `^t` with no further intent should land on.
pub const INITIAL_PANEL: PanelState = PanelState {
    open: false,
    tab: PanelTab::Tree,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelAction {
    Toggle,
    Close,
    Jump(PanelTab),
    /// Tab / shift-tab through the bar.
    Cycle(isize),
    /// Cursor movement inside the active tab.
    Move(isize),
    Confirm,
    /// ⏎'s sibling: branch AND summarize the abandoned path.
    ConfirmSummarize,
}

/// A resolved keymap command → a panel action, or `None` for "not mine".
///
/// This is the whole seam between `keys.rs` and the panel. It reads a
/// `Command` and never a keypress, so which key produced it — and whether the
/// composer had first refusal — is settled before anything here runs.
pub fn panel_action_for(command: Command) -> Option<PanelAction> {
    if let Some(tab) = tab_for_command(command) {
        return Some(PanelAction::Jump(tab));
    }
    match command {
        Command::PanelToggle => Some(PanelAction::Toggle),
        Command::PanelClose => Some(PanelAction::Close),
        Command::PanelNext => Some(PanelAction::Cycle(1)),
        Command::PanelPrev => Some(PanelAction::Cycle(-1)),
        Command::PanelConfirm => Some(PanelAction::Confirm),
        Command::PanelConfirmSummarize => Some(PanelAction::ConfirmSummarize),
        Command::MoveUp => Some(PanelAction::Move(-1)),
        Command::MoveDown => Some(PanelAction::Move(1)),
        _ => None,
    }
}

/// The live theme preview, when the theme tab is in use. The reducer drives
/// it: cursor movement previews, enter keeps, and LEAVING THE TAB REVERTS.
pub trait ThemePreview {
    fn move_by(&mut self, delta: isize);
    fn commit(&mut self);
    fn cancel(&mut self);
}

/// Apply an action. The only side effects are on the injected theme preview —
/// the panel owns "browsing never commits" because it is the thing that knows
/// you left.
pub fn reduce_panel(
    state: PanelState,
    action: PanelAction,
    mut theme: Option<&mut dyn ThemePreview>,
) -> PanelState {
    let leave = |next: PanelState, theme: &mut Option<&mut dyn ThemePreview>| -> PanelState {
        let leaving = state.open && (!next.open || next.tab != state.tab);
        if leaving && state.tab == PanelTab::Theme {
            if let Some(t) = theme.as_deref_mut() {
                t.cancel();
            }
        }
        next
    };
    match action {
        PanelAction::Toggle => leave(
            PanelState {
                open: !state.open,
                ..state
            },
            &mut theme,
        ),
        PanelAction::Close => leave(
            PanelState {
                open: false,
                ..state
            },
            &mut theme,
        ),
        PanelAction::Jump(tab) => {
            // The chord that brought you here takes you back: jumping to the
            // open tab closes.
            let next = if state.open && state.tab == tab {
                PanelState {
                    open: false,
                    ..state
                }
            } else {
                PanelState { open: true, tab }
            };
            leave(next, &mut theme)
        }
        PanelAction::Cycle(delta) => {
            let at = PANEL_TABS.iter().position(|t| *t == state.tab).unwrap_or(0) as isize;
            let n = PANEL_TABS.len() as isize;
            let next = PANEL_TABS[(((at + delta) % n + n) % n) as usize];
            leave(
                PanelState {
                    open: true,
                    tab: next,
                },
                &mut theme,
            )
        }
        PanelAction::Move(delta) => {
            if state.open && state.tab == PanelTab::Theme {
                if let Some(t) = theme.as_deref_mut() {
                    t.move_by(delta);
                }
            }
            state
        }
        PanelAction::Confirm => {
            if state.open && state.tab == PanelTab::Theme {
                if let Some(t) = theme {
                    t.commit();
                }
            }
            state
        }
        // Nothing to preview or revert: the host performs it.
        PanelAction::ConfirmSummarize => state,
    }
}

// ---------------------------------------------------------------------------
// Shared row arithmetic (format.ts::windowAround / legendLine)
// ---------------------------------------------------------------------------

/// Slice bounds for a viewport of `height` rows keeping `selected` centered,
/// clamped so the window never runs past either edge. A list shorter than the
/// viewport yields `start = 0` and the whole list — no blank-row padding.
pub fn window_around(selected: usize, total: usize, height: usize) -> (usize, usize) {
    let a = selected.saturating_sub(height / 2) as isize;
    let b = total as isize - height as isize;
    let start = a.min(b).max(0) as usize;
    (start, start + height)
}

/// A legend row that DEGRADES instead of being cut off: items drop out of the
/// middle, and the way out (the last item) is always kept.
pub fn legend_line(items: &[String], max: Option<usize>) -> String {
    let kept: Vec<&String> = items.iter().filter(|i| !i.trim().is_empty()).collect();
    let join = |parts: &[&str]| parts.join(" · ");
    let full = kept
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    let Some(w) = max.filter(|w| *w > 0) else {
        return full;
    };
    if width(&full) <= w {
        return full;
    }
    let Some(exit) = kept.last() else { return full };
    for n in (1..kept.len().saturating_sub(1)).rev() {
        let mut parts: Vec<&str> = kept[..n].iter().map(|s| s.as_str()).collect();
        parts.push("…");
        parts.push(exit.as_str());
        let candidate = join(&parts);
        if width(&candidate) <= w {
            return candidate;
        }
    }
    // Room for the way out and nothing else, or not even that: an honest
    // truncation beats a row of half a word.
    if kept.len() > 1 && width(exit) <= w {
        let pair = join(&[kept[0].as_str(), "…", exit.as_str()]);
        return if width(&pair) <= w {
            pair
        } else {
            exit.to_string()
        };
    }
    truncate_ansi(kept.first().map(|s| s.as_str()).unwrap_or(""), w, "…")
}

// ---------------------------------------------------------------------------
// The chrome
// ---------------------------------------------------------------------------

/// The tab whose title sits under 0-based column `col` of the strip, or None.
///
/// Pure, and it walks the SAME widths [`render_panel_tabs`] renders — an
/// inactive tab is `"  " + title`, an active one is `" [" + title + "]"`. Two
/// derivations of that layout would be two answers to "which tab did I just
/// click", which is the class of bug where the pointer lands one tab off at
/// the far end of the strip.
///
/// Only the TITLE is a target. The padding between tabs belongs to neither
/// neighbour, so a click in the gap does nothing.
pub fn tab_at_column(active: PanelTab, col: usize) -> Option<PanelTab> {
    let mut x = 0usize;
    for t in TABS.iter() {
        let on = t.id == active;
        x += 2; // "  " or " ["
        if col >= x && col < x + t.title.len() {
            return Some(t.id);
        }
        x += t.title.len() + usize::from(on); // the closing "]" only when active
    }
    None
}

/// The blank row between the tab strip and the body — the first thing to go.
/// At eight terminal rows the panel gets ONE content row, and spending it on
/// whitespace pushed the body onto the bottom border.
pub fn gap_rows(rows: usize) -> usize {
    usize::from(rows >= 5)
}

/// Rows a tab body may paint — the panel's own budget, minus its own chrome.
///
/// The floor is ZERO. `max(3, …)` was the original defect and `max(1, …)` is
/// the same defect one row smaller: a floor is a CLAIM about how much room
/// there is. A panel with no room for a body renders no body.
pub fn panel_body_rows(rows: usize) -> usize {
    rows.saturating_sub(1 /* the tab strip */ + gap_rows(rows))
}

/// The width at which the strip stops fitting and collapses.
fn strip_full_width() -> usize {
    TABS.iter().map(|t| t.title.len() + 2).sum::<usize>() + 12
}

/// The tab strip, as a line.
///
/// Which tab is open is marked with BRACKETS as well as with weight and hue,
/// because a character is the only encoding that always survives — a
/// colourblind reader, a NO_COLOR terminal and anything reading the screen as
/// text all see the same answer.
pub fn panel_tabs_line(tab: PanelTab, width_cols: Option<usize>) -> Line<'static> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let active = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    // A strip that does not fit is worse than no strip: truncation silently
    // drops the tabs at the end, so at 60 columns the theme tab did not appear
    // to exist and neither did the close hint.
    if let Some(w) = width_cols {
        if w < strip_full_width() {
            let at = TABS.iter().position(|t| t.id == tab).unwrap_or(0) + 1;
            return Line::from(vec![
                Span::styled(" [", dim),
                Span::styled(tab.id().to_string(), active),
                Span::styled(format!("] {at}/{} · ⇥ next · ^t close", TABS.len()), dim),
            ]);
        }
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for t in TABS.iter() {
        let on = t.id == tab;
        spans.push(Span::styled(if on { " [" } else { "  " }, dim));
        spans.push(Span::styled(
            t.title.to_string(),
            if on { active } else { Style::default() },
        ));
        if on {
            spans.push(Span::styled("]", dim));
        }
    }
    spans.push(Span::styled("   ^t close", dim));
    Line::from(spans)
}

/// What the panel is showing. `Text` is the honest answer for a tab whose data
/// has not arrived (`loading…`) or that this build does not paint yet.
pub enum PanelBody<'a> {
    Tree(tree::TreeProps<'a>),
    Changes(changes::ChangesProps<'a>),
    /// The theme tab. `None` is "the fetch has not landed" — theme.rs paints
    /// `loading theme…` for it rather than an empty list, which would read as
    /// a build with no presets.
    Theme(Option<&'a crate::theme::ThemePreview>),
    /// The workflow run view — runs, phases, agents, one agent, the script.
    Workflows(workflows::WorkflowsProps<'a>),
    /// Registry · grant · connection · credential, never conflated.
    Mcp(mcp::McpTabProps<'a>),
    /// The `/name` bundles this install can load.
    Skills(skills::SkillsTabProps<'a>),
    /// Both model tiers and the thinking depth, one list.
    Model(model::ModelPickerProps<'a>),
    Text(&'a str),
}

/// Paint the panel into `area` — border included, so `area.height` is
/// `rows + 2`. Nothing is ever painted outside it: the body is clamped to
/// [`panel_body_rows`] and the border is drawn last-in-first-out by ratatui's
/// own clipping.
pub fn render_panel(tab: PanelTab, body: &PanelBody, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        // The panel is a RAISED surface: `panel` existed for exactly this and
        // was painted by nothing, so a preset whose whole note is "deeper
        // surfaces" changed a border colour and left the panel transparent.
        .style(Style::default().bg(PANEL));
    let inner = block.inner(area);
    block.render(area, buf);
    // paddingX = 1 inside the border.
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let rows = inner.height as usize;
    buf.set_line(
        inner.x,
        inner.y,
        &panel_tabs_line(tab, Some(inner.width as usize)),
        inner.width,
    );

    let body_rows = panel_body_rows(rows);
    if body_rows == 0 {
        return;
    }
    let body_area = Rect {
        x: inner.x,
        y: inner.y + 1 + gap_rows(rows) as u16,
        width: inner.width,
        height: body_rows as u16,
    };
    // The tabs paint INSIDE the border and the horizontal padding, so a legend
    // measured against the panel's full width overran by exactly four columns.
    let body_cols = (area.width as usize).saturating_sub(4).max(20);
    match body {
        PanelBody::Tree(p) => tree::render_tree(p, body_area, buf),
        PanelBody::Changes(p) => changes::render_changes(p, body_cols, body_area, buf),
        PanelBody::Theme(preview) => crate::theme::render_theme_tab(*preview, body_area, buf),
        PanelBody::Workflows(p) => workflows::render_workflows(p, body_area, buf),
        PanelBody::Mcp(p) => mcp::render_mcp(p, body_area, buf),
        PanelBody::Skills(p) => skills::render_skills(p, body_area, buf),
        PanelBody::Model(p) => model::render_model(p, body_area, buf),
        PanelBody::Text(text) => {
            let dim = Style::default().add_modifier(Modifier::DIM);
            buf.set_line(
                body_area.x,
                body_area.y,
                &Line::from(Span::styled((*text).to_string(), dim)),
                body_area.width,
            );
        }
    }
}

/// Paint at most `area.height` rows, top first. THE truncation rule: a body
/// that overruns its budget loses its last rows — legible, and the row above
/// it stays a row — instead of dissolving.
pub(crate) fn paint_rows(lines: &[Line], area: Rect, buf: &mut Buffer) {
    for (i, line) in lines.iter().take(area.height as usize).enumerate() {
        buf.set_line(area.x, area.y + i as u16, line, area.width);
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/Panel.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_render {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Render one panel into a grid TALLER than the panel, so an overrun is
    /// VISIBLE as a painted row below the border rather than clipped away.
    pub fn draw_panel(tab: PanelTab, body: &PanelBody, cols: u16, rows: usize) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(cols, rows as u16 + 4)).unwrap();
        term.draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: cols,
                height: rows as u16 + 2,
            };
            render_panel(tab, body, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_render::draw_panel;
    use super::*;
    use crate::forest::fixtures::session_row;
    use crate::forest::{forest_rows, ForestInput};
    use crate::keys::{tab_for_chord, PANEL_TOGGLE};
    use bough_core::schema::parts::SessionKind;

    #[derive(Default)]
    struct Recorder {
        moves: Vec<isize>,
        commits: usize,
        cancels: usize,
    }

    impl ThemePreview for Recorder {
        fn move_by(&mut self, delta: isize) {
            self.moves.push(delta);
        }
        fn commit(&mut self) {
            self.commits += 1;
        }
        fn cancel(&mut self) {
            self.cancels += 1;
        }
    }

    #[test]
    fn ctrl_t_toggles_the_panel_and_every_other_chord_jumps_straight_to_its_tab() {
        let mut state = INITIAL_PANEL;
        assert!(!state.open);

        let toggle = panel_action_for(Command::PanelToggle).unwrap();
        assert_eq!(toggle, PanelAction::Toggle);
        state = reduce_panel(state, toggle, None);
        // The tree is the home tab.
        assert_eq!(
            state,
            PanelState {
                open: true,
                tab: PanelTab::Tree
            }
        );

        // A chord is a DIRECT jump: it works from any tab, and from a closed panel.
        for t in TABS.iter() {
            let action = panel_action_for(Command::Tab(t.id)).unwrap();
            assert_eq!(action, PanelAction::Jump(t.id), "{}", t.chord);
            let next = reduce_panel(
                PanelState {
                    open: false,
                    tab: PanelTab::Tree,
                },
                action,
                None,
            );
            assert_eq!(
                next,
                PanelState {
                    open: true,
                    tab: t.id
                }
            );
        }

        // The chord that brought you here takes you back.
        let closed = reduce_panel(
            PanelState {
                open: true,
                tab: PanelTab::Changes,
            },
            PanelAction::Jump(PanelTab::Changes),
            None,
        );
        assert_eq!(
            closed,
            PanelState {
                open: false,
                tab: PanelTab::Changes
            }
        );
    }

    #[test]
    fn tab_cycles_the_bar_in_both_directions_and_wraps() {
        let first = PANEL_TABS[0];
        let last = PANEL_TABS[PANEL_TABS.len() - 1];
        let state = reduce_panel(
            PanelState {
                open: true,
                tab: first,
            },
            PanelAction::Cycle(1),
            None,
        );
        assert_eq!(state.tab, PANEL_TABS[1]);
        let state = reduce_panel(
            PanelState {
                open: true,
                tab: first,
            },
            PanelAction::Cycle(-1),
            None,
        );
        assert_eq!(state.tab, last);
        let state = reduce_panel(
            PanelState {
                open: true,
                tab: last,
            },
            PanelAction::Cycle(1),
            None,
        );
        assert_eq!(state.tab, first);
    }

    #[test]
    fn panel_action_for_claims_the_panels_commands_and_nothing_else() {
        assert_eq!(
            panel_action_for(Command::PanelClose),
            Some(PanelAction::Close)
        );
        assert_eq!(
            panel_action_for(Command::PanelNext),
            Some(PanelAction::Cycle(1))
        );
        assert_eq!(
            panel_action_for(Command::PanelPrev),
            Some(PanelAction::Cycle(-1))
        );
        assert_eq!(
            panel_action_for(Command::MoveDown),
            Some(PanelAction::Move(1))
        );
        assert_eq!(
            panel_action_for(Command::MoveUp),
            Some(PanelAction::Move(-1))
        );
        assert_eq!(
            panel_action_for(Command::PanelConfirm),
            Some(PanelAction::Confirm)
        );
        // Chat's own commands pass straight through — the panel is not a key sink.
        assert_eq!(panel_action_for(Command::Send), None);
        assert_eq!(panel_action_for(Command::DeleteWordBack), None);
        assert_eq!(panel_action_for(Command::RailEnter), None);
        // Nor are the workflow verbs: they belong to one TAB, and the host routes them.
        assert_eq!(panel_action_for(Command::WfPause), None);
    }

    #[test]
    fn the_keymap_is_data_with_no_duplicate_binding() {
        let mut chords: Vec<&str> = vec![PANEL_TOGGLE];
        chords.extend(TABS.iter().map(|t| t.chord));
        let unique: std::collections::HashSet<&&str> = chords.iter().collect();
        assert_eq!(unique.len(), chords.len(), "{}", chords.join(","));
        let tabs: std::collections::HashSet<PanelTab> = TABS.iter().map(|t| t.id).collect();
        assert_eq!(tabs.len(), TABS.len());
        assert_eq!(tab_for_chord(PANEL_TOGGLE), None); // ^t is the toggle, never a tab
        assert_eq!(tab_for_chord("zzz"), None);
    }

    // ---- the theme preview: browsing never commits -------------------------

    #[test]
    fn every_departure_reverts_toggle_escape_chord_tab_and_shift_tab() {
        let departures = [
            PanelAction::Toggle,
            PanelAction::Close,
            PanelAction::Jump(PanelTab::Tree),
            PanelAction::Cycle(1),
            PanelAction::Cycle(-1),
        ];
        for action in departures {
            let mut theme = Recorder::default();
            let state = PanelState {
                open: true,
                tab: PanelTab::Theme,
            };
            reduce_panel(state, action, Some(&mut theme));
            assert_eq!(theme.cancels, 1, "{action:?} did not revert the preview");
            assert_eq!(theme.commits, 0);
        }
        // Staying put never reverts — a cursor move is browsing, not leaving.
        let mut theme = Recorder::default();
        let state = PanelState {
            open: true,
            tab: PanelTab::Theme,
        };
        reduce_panel(state, PanelAction::Move(1), Some(&mut theme));
        assert_eq!(theme.cancels, 0);
        assert_eq!(theme.moves, vec![1]);
        // …and enter keeps it.
        let mut theme = Recorder::default();
        reduce_panel(state, PanelAction::Confirm, Some(&mut theme));
        assert_eq!(theme.commits, 1);
        assert_eq!(theme.cancels, 0);
        // Departing from any OTHER tab touches nothing.
        let mut theme = Recorder::default();
        reduce_panel(
            PanelState {
                open: true,
                tab: PanelTab::Tree,
            },
            PanelAction::Close,
            Some(&mut theme),
        );
        assert_eq!(theme.cancels, 0);
    }

    // ---- the strip ----------------------------------------------------------

    #[test]
    fn the_open_tab_is_marked_in_text_not_only_in_colour() {
        for id in [PanelTab::Tree, PanelTab::Changes, PanelTab::Theme] {
            let text: String = panel_tabs_line(id, None)
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect();
            assert!(
                text.contains(&format!("[{}]", id.id())),
                "tab {id:?} is not marked: {text}"
            );
            assert_eq!(text.matches('[').count(), 1, "{text}");
        }
        // Every tab is still listed, marked or not.
        let strip: String = panel_tabs_line(PanelTab::Mcp, None)
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        for t in TABS.iter() {
            assert!(strip.contains(t.title), "missing tab {}", t.title);
        }
    }

    #[test]
    fn a_narrow_strip_collapses_instead_of_dropping_the_tabs_at_the_end() {
        let text: String = panel_tabs_line(PanelTab::Theme, Some(40))
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, " [theme] 7/7 · ⇥ next · ^t close");
    }

    #[test]
    fn a_click_on_a_title_picks_that_tab_on_either_side_of_the_active_one() {
        // "  tree  changes …" — the strip opens with two spaces, so `tree`
        // occupies columns 2..5.
        assert_eq!(tab_at_column(PanelTab::Changes, 2), Some(PanelTab::Tree));
        assert_eq!(tab_at_column(PanelTab::Changes, 5), Some(PanelTab::Tree));
        assert_eq!(tab_at_column(PanelTab::Changes, 0), None);
        assert_eq!(tab_at_column(PanelTab::Tree, 9), Some(PanelTab::Changes));
    }

    #[test]
    fn the_active_tabs_brackets_shift_everything_after_it_by_one() {
        // " [tree]" is one column wider than "  tree", so a strip measured
        // against the inactive layout would drift right for every later tab.
        assert_eq!(tab_at_column(PanelTab::Tree, 8), None);
        assert_eq!(tab_at_column(PanelTab::Changes, 8), Some(PanelTab::Changes));
    }

    #[test]
    fn a_click_in_the_padding_between_titles_picks_nothing() {
        assert_eq!(tab_at_column(PanelTab::Changes, 1), None);
        assert_eq!(tab_at_column(PanelTab::Changes, 6), None);
    }

    #[test]
    fn every_tab_is_reachable_by_some_column() {
        for active in [PanelTab::Tree, PanelTab::Mcp] {
            let mut seen: std::collections::HashSet<PanelTab> = std::collections::HashSet::new();
            for c in 0..200 {
                if let Some(hit) = tab_at_column(active, c) {
                    seen.insert(hit);
                }
            }
            assert_eq!(
                seen.len(),
                TABS.len(),
                "active={active:?}: reached {seen:?}"
            );
        }
    }

    // ---- row arithmetic -----------------------------------------------------

    #[test]
    fn the_body_budget_floors_at_zero_and_spends_the_gap_row_first() {
        assert_eq!(gap_rows(4), 0);
        assert_eq!(gap_rows(5), 1);
        assert_eq!(panel_body_rows(0), 0);
        assert_eq!(panel_body_rows(1), 0);
        assert_eq!(panel_body_rows(4), 3);
        assert_eq!(panel_body_rows(12), 10);
    }

    #[test]
    fn window_around_keeps_the_cursor_centred_and_never_runs_past_an_edge() {
        assert_eq!(window_around(0, 3, 10), (0, 10), "a short list starts at 0");
        assert_eq!(window_around(0, 27, 5), (0, 5));
        assert_eq!(window_around(13, 27, 5), (11, 16));
        assert_eq!(window_around(26, 27, 5), (22, 27), "clamped at the bottom");
    }

    #[test]
    fn a_legend_drops_items_from_the_middle_and_always_keeps_the_way_out() {
        let items: Vec<String> = [
            "↑↓ move",
            "→ focus one file",
            "x revert this path",
            "esc back",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(legend_line(&items, None), items.join(" · "));
        let narrow = legend_line(&items, Some(30));
        assert!(narrow.ends_with("esc back"), "{narrow}");
        assert!(narrow.contains('…'), "{narrow}");
        assert!(width(&narrow) <= 30, "{narrow}");
        // Room for the way out and nothing else.
        assert_eq!(legend_line(&items, Some(9)), "esc back");
    }

    // ---- the row budget: the 100x12 panel corruption ------------------------

    /// 27 conversations, as the TS suite's `LIST`.
    fn many_sessions() -> Vec<crate::api::SessionRow> {
        (0..27)
            .map(|i| {
                let mut s = session_row(&format!("s{i}"), SessionKind::Root, 1_000 - i);
                s.session.title = format!("session number {i}");
                s.session.workspace = Some("/tmp/ws".into());
                s.last_turn_status = Some(bough_core::schema::parts::TurnStatus::Done);
                s
            })
            .collect()
    }

    #[test]
    fn no_tab_paints_past_its_row_budget_the_100x12_panel_corruption() {
        let sessions = many_sessions();
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            ..Default::default()
        });
        // Every height from "absurdly cramped" up to comfortable must hold the
        // property, because the corruption appeared at some heights and not others.
        // The theme tab windows its own list, so it is the one body that can
        // out-emit its budget without a caller passing a wrong height.
        let preview = crate::theme::ThemePreview::with_apply(None, Box::new(|_| {}));
        // Every tab's own data, at a size that overruns any budget it is given —
        // the tabs each had their own arithmetic and each got it wrong in its
        // own way.
        let wf_detail = workflows::fixtures::detail();
        let catalog = model::fixtures::catalog();
        let model_cfg = model::fixtures::cfg();
        let filters = model::ModelFilters::default();
        let entries = model::model_entries(&catalog, None, &filters);
        let mcp_servers: Vec<(String, bough_core::mcp::config::ServerConfig)> = (0..30)
            .map(|i| (format!("srv{i:02}"), mcp::fixtures::stdio("x")))
            .collect();
        let mcp_refs: Vec<(&str, bough_core::mcp::config::ServerConfig)> = mcp_servers
            .iter()
            .map(|(n, c)| (n.as_str(), c.clone()))
            .collect();
        let mcp_status = mcp::fixtures::status(&mcp_refs, &[], &[], vec![]);
        let skill_rows: Vec<skills::SkillRow> = (0..30)
            .map(|i| skills::SkillRow {
                name: format!("skill{i:02}"),
                description: "does a thing".into(),
                error: None,
                mcp: Vec::new(),
            })
            .collect();
        for h in [1usize, 2, 3, 4, 6, 8, 12, 20] {
            let bodies = [
                PanelBody::Tree(tree::TreeProps {
                    rows: &items,
                    selected: 0,
                    height: panel_body_rows(h),
                    ..Default::default()
                }),
                PanelBody::Changes(changes::ChangesProps {
                    rows: panel_body_rows(h),
                    ..Default::default()
                }),
                PanelBody::Workflows(workflows::WorkflowsProps {
                    level: 1,
                    detail: Some(&wf_detail),
                    rows: panel_body_rows(h),
                    cols: 96,
                    ..Default::default()
                }),
                PanelBody::Model(model::ModelPickerProps {
                    cols: 96,
                    cfg: &model_cfg,
                    entries: &entries,
                    selected: 0,
                    rows: panel_body_rows(h),
                    message: None,
                    filters: &filters,
                    focused: None,
                }),
                PanelBody::Mcp(mcp::McpTabProps {
                    status: Some(&mcp_status),
                    selected: 0,
                    rows: panel_body_rows(h),
                    cols: 96,
                    ..Default::default()
                }),
                PanelBody::Skills(skills::SkillsTabProps {
                    skills: Some(&skill_rows),
                    rows: panel_body_rows(h),
                    cols: 96,
                    ..Default::default()
                }),
                PanelBody::Theme(Some(&preview)),
            ];
            for (tab, body) in PANEL_TABS.iter().zip(bodies.iter()) {
                let painted = draw_panel(*tab, body, 100, h);
                // The panel is `rows + 2` tall. Nothing may be painted below it…
                for (i, row) in painted.iter().enumerate().skip(h + 2) {
                    assert_eq!(row, "", "{tab:?} @{h}: painted below the panel (row {i})");
                }
                // …and nothing may be painted ON its bottom border.
                let bottom = &painted[h + 1];
                assert!(
                    bottom.starts_with('╰')
                        && bottom.ends_with('╯')
                        && bottom[3..bottom.len() - 3].chars().all(|c| c == '─'),
                    "{tab:?} @{h}: the bottom border was painted over: {bottom}"
                );
            }
        }
    }

    #[test]
    fn the_theme_tab_paints_its_picker_through_the_panel_not_a_placeholder() {
        let preview = crate::theme::ThemePreview::with_apply(None, Box::new(|_| {}));
        let painted = draw_panel(PanelTab::Theme, &PanelBody::Theme(Some(&preview)), 100, 12);
        let screen = painted.join("\n");
        assert!(
            screen.contains("[theme]"),
            "the tab bar must mark the open tab:\n{screen}"
        );
        // The cursor is on the palette in force, and the rows are the presets.
        assert!(
            screen.contains("❯ Default"),
            "the picker's cursor row is missing:\n{screen}"
        );
        assert!(
            screen.contains("Fjord"),
            "the preset list is missing:\n{screen}"
        );
        // The legend is the LAST row of the body and says what leaving costs.
        assert!(
            screen.contains(
                "current: Default — ↑↓ preview live · ⏎ keep · esc back (leaving reverts)"
            ),
            "the legend is missing or reworded:\n{screen}"
        );
        assert!(
            !screen.contains("nothing to show here yet"),
            "a built picker must never fall through to the absent-surface placeholder"
        );
    }

    /// The body a `None` preview paints — the beat between opening the tab and
    /// `GET /theme` landing. An empty box there reads as a build with no presets.
    #[test]
    fn a_theme_tab_whose_fetch_has_not_landed_says_it_is_loading() {
        let painted = draw_panel(PanelTab::Theme, &PanelBody::Theme(None), 100, 6);
        assert!(painted.join("\n").contains("loading theme…"));
    }
}
