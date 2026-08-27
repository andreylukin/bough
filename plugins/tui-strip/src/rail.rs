//! Invariant: every rail row is derived from state the pane ALREADY HOLDS, and the two halves of
//! an about-line are never confused. The STATE half is rendered as truth and therefore only ever
//! from a cited `about/line` step (§16, and `crate::invariant`); the INTENT half is always drawn
//! under [`INTENT_LABEL`], never as truth (§2).
//!
//! Everything here is PURE: `(rows, width, theme) -> Vec<Line>`. The pane's listeners keep the
//! `RailRow` list current; this module only draws it.

use bough_plugin_agents::{AgentId, Status};
use bough_plugin_tui_render::AboutView;
use bough_plugin_tui_shell::pane::{HitId, PaneOutcome};
use bough_plugin_tui_shell::{FocusRequest, Theme};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub use bough_plugin_tui_render::about::INTENT_LABEL;

/// The prefix a rail row's clickable region is minted under. The shell's hit map is keyed by
/// string, so the convention is spelled once, here, and parsed back by [`focus_for_hit`].
pub const HIT_PREFIX: &str = "rail:";

/// One agent, as the rail knows it. The listeners maintain this; `render` never queries.
#[derive(Clone, Debug, PartialEq)]
pub struct RailRow {
    pub agent: AgentId,
    /// The agent's trajectory, which is the key an `about/line` step arrives under. `None` for an
    /// agent whose session the rail has not seen.
    pub traj: Option<bough_plugin_ledger::TrajId>,
    pub name: String,
    pub status: Status,
    pub wake_pending: bool,
    pub disposed: bool,
    /// Folded from `agent/dormancy` steps BY NAME (P3-D11): the rail gains no dependency on the
    /// `dormancy` row, and with that row disabled no agent is ever dormant.
    pub dormant: bool,
    /// `None` when the agent has never written an `about/line` — with the `about-line` row
    /// disabled that is every agent, and the rail still renders (P3-D11).
    pub about: Option<AboutView>,
}

/// PURE, unit-tested: status + pending wake ⇒ glyph and style ROLE.
///
/// The role is a [`Theme`] field name, never a colour: call sites name roles (`tui-shell`'s theme
/// invariant), and `shell-use cells` assertions then have a name to test against.
pub fn glyph(
    status: Status,
    wake_pending: bool,
    disposed: bool,
    dormant: bool,
) -> (char, &'static str) {
    // Disposed first: a disposed agent can still carry a stale status, and what it IS is gone.
    if disposed {
        return ('×', "dim");
    }
    // Dormant next, ahead of the status arms: a dormant agent keeps whatever status it had when
    // it went to sleep (§1 — dormancy is not a status), and drawing `idle` for it would say the
    // one thing that is not true, that a wake would run.
    if dormant {
        return ('\u{25CC}', "dim");
    }
    match (status, wake_pending) {
        (Status::Running, _) => ('●', "accent"),
        // Idle with a wake already handed to the driver: about to run, not running.
        (Status::Idle, true) => ('◐', "warn"),
        (Status::Idle, false) => ('○', "fg"),
    }
}

/// The colour a style role names.
pub fn role_color(theme: &Theme, role: &str) -> ratatui::style::Color {
    match role {
        "dim" => theme.dim,
        "accent" => theme.accent,
        "warn" => theme.warn,
        "error" => theme.error,
        "evidence" => theme.evidence,
        "hint" => theme.hint,
        _ => theme.fg,
    }
}

/// The word under the glyph.
pub fn status_word(row: &RailRow) -> &'static str {
    if row.disposed {
        "disposed"
    } else if row.dormant {
        "dormant"
    } else {
        match (row.status, row.wake_pending) {
            (Status::Running, _) => "running",
            (Status::Idle, true) => "waking",
            (Status::Idle, false) => "idle",
        }
    }
}

/// The clickable region id for one rail row.
pub fn hit_for_agent(agent: &AgentId) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{agent}"))
}

/// PURE: a clicked region ⇒ what the pane asks the shell to do. `None` for a hit this pane did
/// not mint, so a click that landed on somebody else's region is `Ignored` rather than a focus
/// change to a nonexistent agent.
pub fn focus_for_hit(hit: &HitId) -> Option<FocusRequest> {
    let rest = hit.as_str().strip_prefix(HIT_PREFIX)?;
    if rest.is_empty() {
        return None;
    }
    Some(FocusRequest {
        agent: Some(AgentId::new(rest)),
        ..Default::default()
    })
}

/// PURE: the rail's whole interaction. A click on a row asks the SHELL to focus that agent; the
/// pane never moves focus itself (§2.1's `PaneOutcome`). Split out of `Pane::handle` so it is
/// testable without a live shell — `PaneCx` carries a `TuiHandle`, which only a mounted row has.
pub fn on_click(hit: Option<&HitId>) -> PaneOutcome {
    match hit.and_then(focus_for_hit) {
        Some(req) => PaneOutcome::Focus(req),
        None => PaneOutcome::Ignored,
    }
}

/// How many terminal rows one rail row occupies under this config. The pane's hit map needs this
/// before it draws, so it is a function rather than a side effect of drawing.
pub fn row_height(row: &RailRow, show_about: bool, about_lines: u16) -> u16 {
    1 + about_extent(row, show_about, about_lines)
}

fn about_extent(row: &RailRow, show_about: bool, about_lines: u16) -> u16 {
    if !show_about || about_lines == 0 {
        return 0;
    }
    match &row.about {
        None => 0,
        Some(v) => {
            // The state half is one line; the intent half is one more, under its label.
            let wanted = 1 + u16::from(!v.intent.trim().is_empty());
            wanted.min(about_lines)
        }
    }
}

/// PURE: one rail row ⇒ the lines it draws, at `width`.
///
/// The rail is narrow by construction, so long halves are ELIDED with `…` rather than wrapped:
/// a wrapped state half would push the intent label off the rail, and the label is the thing that
/// keeps the second half from reading as truth.
pub fn row_lines(
    row: &RailRow,
    focused: bool,
    show_about: bool,
    about_lines: u16,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (g, role) = glyph(row.status, row.wake_pending, row.disposed, row.dormant);
    let mut head_style = Style::default().fg(role_color(theme, role));
    if focused {
        head_style = head_style.add_modifier(Modifier::BOLD);
    }
    let word = status_word(row);
    let name_room = width.saturating_sub(2 + word.len() as u16 + 1) as usize;
    let mut head = vec![
        Span::styled(format!("{g} "), head_style),
        Span::styled(
            elide(&row.name, name_room),
            Style::default().fg(theme.fg).add_modifier(if focused {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
    ];
    let used = 2 + elide(&row.name, name_room).chars().count();
    let pad = (width as usize).saturating_sub(used + word.len());
    head.push(Span::raw(" ".repeat(pad)));
    head.push(Span::styled(
        word.to_string(),
        Style::default().fg(role_color(theme, role)),
    ));
    let mut out = vec![Line::from(head)];

    let extent = about_extent(row, show_about, about_lines);
    if extent == 0 {
        return out;
    }
    // NOT an `expect`: this is the RENDER path, and a panic inside `Pane::render` unwinds the
    // draw loop and takes the process down (V8, which `tui-probe` exists to demonstrate).
    let Some(v) = row.about.as_ref() else {
        return out;
    };
    let body_room = width.saturating_sub(2) as usize;
    // The STATE half: evidence, and only ever reached here through `about_from_step`, which
    // reads a cited step (`crate::invariant`).
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            elide(v.state.trim(), body_room),
            Style::default().fg(theme.evidence),
        ),
    ]));
    if extent > 1 {
        // The INTENT half, ALWAYS under its label. §2: self-declared, never truth.
        let label_room = body_room.saturating_sub(INTENT_LABEL.len() + 2);
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{INTENT_LABEL}: "),
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                elide(v.intent.trim(), label_room),
                Style::default().fg(theme.thought),
            ),
        ]));
    }
    out
}

/// Truncate to `room` display cells, marking the cut with `…`. Grapheme-naive on purpose: the
/// general wrapper lives in `tui-render` and the rail needs a cut, not a wrap.
fn elide(text: &str, room: usize) -> String {
    let one_line: String = text.lines().next().unwrap_or("").to_string();
    if room == 0 {
        return String::new();
    }
    if one_line.chars().count() <= room {
        return one_line;
    }
    let keep = room.saturating_sub(1);
    let mut s: String = one_line.chars().take(keep).collect();
    s.push('…');
    s
}

/// PURE: the whole rail ⇒ its lines plus, for each row, the line span it occupies. The pane turns
/// the second half into `RenderCx::hit` calls; keeping them together is what makes click-to-focus
/// testable without a terminal.
pub fn rail(
    rows: &[RailRow],
    focused: Option<&AgentId>,
    show_about: bool,
    about_lines: u16,
    width: u16,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<(AgentId, u16, u16)>) {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    for row in rows {
        let top = lines.len() as u16;
        let is_focused = focused == Some(&row.agent);
        lines.extend(row_lines(
            row,
            is_focused,
            show_about,
            about_lines,
            width,
            theme,
        ));
        spans.push((row.agent.clone(), top, lines.len() as u16 - top));
        // One blank line between agents, so two rails rows never read as one block.
        lines.push(Line::from(""));
    }
    (lines, spans)
}
