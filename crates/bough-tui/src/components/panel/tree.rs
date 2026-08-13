//! The one tree, painted (port of `src/tui/components/Tree.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **visibility is derived from lineage, never
//! stored.** There is no archive, deprecate, hide or purge state anywhere in
//! the model, so there is no affordance here for one.
//!
//! PURE CORE ELSEWHERE. The rows come from `forest.rs`; this file windows the
//! list around the cursor and paints it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use bough_core::schema::parts::{Role, SessionKind, TurnStatus};

use crate::api::SessionRow;
use crate::components::info;
use crate::components::panel::{legend_line, paint_rows, window_around};
use crate::forest::{is_delegated, ForestRow};
use crate::store::selectors::{clip, fmt_usd};

/// A root that CAME FROM another conversation — a handoff, an extract — gets
/// its own mark. `kind` cannot say this: both are `root`, and `title_of`
/// strips the `handoff · ` prefix, so the two rendered identically.
const DERIVED_ROOT: &str = "↦";

fn kind_glyph_for(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Root => "●",
        SessionKind::Fork => "⑂",
        SessionKind::Compaction => "≣",
        SessionKind::Subagent => "◆",
        // NOTHING CREATES THIS KIND — a workflow agent's session is kind
        // `subagent`. Kept because the schema still has the value and this map
        // must be total over it; the help no longer advertises the glyph.
        SessionKind::WorkflowAgent => "◈",
        // A firing of a schedule. The clock reads as "this ran on its own".
        SessionKind::ScheduleRun => "◷",
        // The conversation `!` runs in. Marked as the user's own, because it is.
        SessionKind::Shell => "●",
    }
}

/// The row's kind mark.
pub fn kind_glyph(s: &SessionRow) -> &'static str {
    if s.session.kind == SessionKind::Root && s.session.origin_id.is_some() {
        DERIVED_ROOT
    } else {
        kind_glyph_for(s.session.kind)
    }
}

/// Outcome marker. `None` for a session that has never run a turn — an
/// absence, not a state, and rendering a glyph for it would invent one.
///
/// `outcome_ok == Some(false)` is checked ahead of the turn status because it
/// is the DELEGATION outcome: a subagent whose turn ended `done` but whose
/// work failed is exactly the branch the tree exists to make findable.
pub fn status_mark(s: &SessionRow, busy_below: usize) -> Option<(&'static str, Color)> {
    if s.busy {
        return Some(("⋯", Color::Cyan));
    }
    // Work running UNDER this conversation counts as this conversation
    // running. Without it a root sitting on five live subagents rendered `✓`.
    if busy_below > 0 {
        return Some(("⋯", Color::Cyan));
    }
    if s.session.outcome_ok == Some(false) {
        return Some(("✗", Color::Red));
    }
    s.last_turn_status.map(turn_mark)
}

/// The mark for one turn outcome, and the ONE place the mapping lives — the
/// terminal tab reads a live `turn.finished` while the tree reads a row's
/// `last_turn_status`, and the two must not drift into different alphabets.
pub fn turn_mark(status: TurnStatus) -> (&'static str, Color) {
    match status {
        TurnStatus::Running => ("⋯", Color::Cyan),
        TurnStatus::Orphaned | TurnStatus::Interrupted => ("◼", Color::Yellow),
        TurnStatus::Error => ("✗", Color::Red),
        TurnStatus::Done => ("✓", Color::Green),
    }
}

/// The row's words: the server's kind prefix stripped, then the workspace's
/// directory name, then `(untitled)`.
pub fn title_of(s: &SessionRow) -> String {
    let title = s.session.title.as_str();
    let base = ["fork", "compacted", "handoff", "subagent", "workflow"]
        .iter()
        .find_map(|p| title.strip_prefix(&format!("{p} · ")))
        .unwrap_or(title)
        .trim();
    if !base.is_empty() {
        return base.to_string();
    }
    // THE WORKSPACE, before "(untitled)". The tree is the switcher and the one
    // surface where every row has to be recognisable; a directory name is a
    // much better title than nothing.
    let dir = s
        .session
        .workspace
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/')
        .split('/')
        .rfind(|p| !p.is_empty())
        .unwrap_or("");
    if dir.is_empty() {
        "(untitled)".to_string()
    } else {
        dir.to_string()
    }
}

/// `supervisor` is the agent — the transcript calls it "bough" and so does this.
fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "you",
        Role::Supervisor => "bough",
        Role::System => "system",
    }
}

fn role_color(role: Role) -> Color {
    match role {
        Role::User => Color::White,
        Role::Supervisor => Color::Green,
        Role::System => Color::Yellow,
    }
}

/// The rows on screen, and where the window starts.
///
/// Exported because the panel host resolves `1`–`9` against the SAME window
/// this paints: two calculations of "which rows are visible" is how a digit
/// comes to select a row nobody can see.
///
/// TWO rows of chrome, and they go LAST: the mark legend, then the key legend.
/// The count is FIXED rather than conditional on what is on screen, because a
/// reservation that depends on the rows being windowed is a reservation the
/// renderer and the digit resolver can disagree about.
pub fn forest_window(count: usize, selected: usize, rows: usize, chrome: usize) -> (usize, usize) {
    let height = rows.saturating_sub(2 + chrome);
    let (start, end) = window_around(selected, count, height);
    (start, end.saturating_sub(start))
}

/// What the marks ON SCREEN mean, in the order the eye meets them.
///
/// Only marks PRESENT in the given rows are described: a static legend would
/// spend scarce columns on `≣ compaction` for the many users who have never
/// made one, and `legend_line` would then drop the entries actually on screen.
pub fn mark_legend(rows: &[ForestRow]) -> Vec<String> {
    const KINDS: [(&str, &str); 6] = [
        ("●", "yours"),
        (DERIVED_ROOT, "handoff"),
        ("⑂", "fork"),
        ("≣", "compaction"),
        ("◆", "subagent"),
        ("◷", "scheduled run"),
    ];
    const STATUSES: [(&str, &str); 4] = [
        ("⋯", "running"),
        ("✓", "done"),
        ("✗", "failed"),
        ("◼", "stopped"),
    ];
    let mut seen_kind: Vec<&str> = Vec::new();
    let mut seen_status: Vec<&str> = Vec::new();
    // The SHAPE marks — the ones this layout is made of. They belong to turn
    // and branch rows, which carry no kind glyph at all.
    let mut shape: Vec<&str> = Vec::new();
    for r in rows {
        match r {
            ForestRow::Message { branches, leaf, .. } => {
                if *branches > 0 && !shape.contains(&"⑂ branch point") {
                    shape.push("⑂ branch point");
                }
                if *leaf && !shape.contains(&"◀ leaf") {
                    shape.push("◀ leaf");
                }
                continue;
            }
            ForestRow::Tool { .. } => {
                if !shape.contains(&"✦ tool") {
                    shape.push("✦ tool");
                }
                continue;
            }
            // A branch row's kind is always a branching one, and saying "⑂
            // fork" beside a row already drawn as a fan is a wasted column.
            ForestRow::Branch { active, .. } => {
                if *active && !shape.contains(&"● active branch") {
                    shape.push("● active branch");
                }
                continue;
            }
            _ => {}
        }
        // Only `session` rows carry the kind marks — a caption does not.
        let ForestRow::Session {
            session,
            busy_below,
            ..
        } = r
        else {
            continue;
        };
        let g = kind_glyph(session);
        if !seen_kind.contains(&g) {
            seen_kind.push(g);
        }
        if let Some((glyph, _)) = status_mark(session, *busy_below) {
            if !seen_status.contains(&glyph) {
                seen_status.push(glyph);
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (g, label) in KINDS {
        if seen_kind.contains(&g) {
            out.push(format!("{g} {label}"));
        }
    }
    for (g, label) in STATUSES {
        if seen_status.contains(&g) {
            out.push(format!("{g} {label}"));
        }
    }
    out.extend(shape.into_iter().map(str::to_string));
    out
}

/// `2h`, `58m`, `4d` — the right-hand column. Coarse on purpose: the tree is
/// scanned, not read, and "how long ago" at one significant figure is the whole
/// question a row answers.
pub fn age(now: i64, then: i64) -> String {
    let s = ((now - then) / 1000).max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 172_800 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// The line, with `right` pushed against the last column.
///
/// The age column is what makes the tree readable as a HISTORY rather than a
/// list, and it only works if it lines up. A row too wide to hold both keeps
/// its content and drops the age: a truncated `5` that used to be `58m` is a
/// worse lie than no timestamp.
fn with_age(
    mut spans: Vec<Span<'static>>,
    right: Option<String>,
    cols: Option<usize>,
) -> Line<'static> {
    let (Some(right), Some(cols)) = (right, cols) else {
        return Line::from(spans);
    };
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let want = right.chars().count();
    if used + want + 2 > cols {
        return Line::from(spans);
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    spans.push(Span::raw(" ".repeat(cols - used - want)));
    spans.push(Span::styled(right, dim));
    Line::from(spans)
}

#[derive(Default)]
pub struct TreeProps<'a> {
    pub rows: &'a [ForestRow],
    pub selected: usize,
    /// The tab body's total row budget, legend and filter row included.
    pub height: usize,
    /// The `/` buffer, echoed so a narrowed list says what narrowed it.
    pub filter: Option<&'a str>,
    pub filtering: bool,
    /// The open conversation's workspace. A top-level row in a DIFFERENT one
    /// says so; labelling every row with the directory you are already in is
    /// noise.
    pub workspace: Option<&'a str>,
    /// Columns available, so the legend degrades instead of being cut mid-word.
    pub cols: Option<usize>,
    /// A refusal or a result, from the panel host's `message` state.
    pub message: Option<&'a str>,
    /// Now, in epoch milliseconds, for the age column. `0` = no clock was
    /// passed and no ages are drawn — a test's rows must not print "56y".
    pub now: i64,
    /// The re-rooted view's lineage, outermost first. Empty = the whole forest.
    pub crumbs: &'a [String],
}

/// The lines this tab paints, in order. Split out from the render so the row
/// budget is testable without a terminal.
pub fn tree_lines(p: &TreeProps) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut out: Vec<Line<'static>> = Vec::new();

    // The message takes a row from the list, like the filter echo above it —
    // otherwise it is drawn on top of one, or past the tab's budget.
    let chrome = usize::from(p.filtering || p.filter.is_some())
        + usize::from(p.message.is_some())
        + usize::from(!p.crumbs.is_empty());
    let (start, shown) = forest_window(p.rows.len(), p.selected, p.height, chrome);
    let window: &[ForestRow] = &p.rows[start.min(p.rows.len())..(start + shown).min(p.rows.len())];

    // THE CRUMB LINE IS WHERE DEPTH LIVES. Everything below it is drawn at one
    // level, because this row already said how far in the reader has walked.
    if !p.crumbs.is_empty() {
        let mut crumb: Vec<Span<'static>> = vec![Span::styled("⌂", dim)];
        for (i, c) in p.crumbs.iter().enumerate() {
            crumb.push(Span::styled(" ▸ ", dim));
            crumb.push(Span::styled(
                clip(c, 28),
                if i + 1 == p.crumbs.len() {
                    Style::default()
                } else {
                    dim
                },
            ));
        }
        out.push(with_age(crumb, Some("esc up one level".into()), p.cols));
    }
    if p.filtering || p.filter.is_some() {
        out.push(Line::from(Span::styled(
            format!(
                "/ {}{}",
                p.filter.unwrap_or(""),
                if p.filtering { "▌" } else { "" }
            ),
            dim,
        )));
    }
    if p.rows.is_empty() {
        out.push(Line::from(Span::styled(
            match p.filter {
                // Says WHAT was searched: `/` covers titles, workspaces and
                // every message in every transcript, and a bare "nothing
                // matches" leaves the user wondering whether it looked inside.
                Some(q) if !q.is_empty() => {
                    format!("nothing matches \"{q}\" — titles, paths or messages")
                }
                _ => "no conversations yet".to_string(),
            },
            dim,
        )));
    }

    let warn = Style::default().fg(crate::components::warn());
    for (i, item) in window.iter().enumerate() {
        let idx = start + i;
        let sel = idx == p.selected;
        let cursor = if sel { "❯ " } else { "  " };
        let cursor_style = if sel {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let indent = "  ".repeat(item.depth());
        let mut spans: Vec<Span<'static>> = vec![Span::styled(cursor, cursor_style)];
        let mut right: Option<String> = None;
        match item {
            ForestRow::Collapsed { count, depth, .. } => {
                spans.push(Span::styled(
                    format!("{}⋯ {count} spawned · → drill in", "  ".repeat(*depth)),
                    dim,
                ));
            }
            ForestRow::Tool {
                depth,
                verb,
                detail,
                ..
            } => {
                // Evidence, not a node: it is indented past the trunk guide and
                // it is the one row here that no key acts on.
                spans.push(Span::styled(format!("{}    ", "  ".repeat(*depth)), dim));
                spans.push(Span::styled(
                    format!("✦ {verb} "),
                    Style::default().fg(info()),
                ));
                spans.push(Span::styled(clip(detail, 46), dim));
            }
            ForestRow::Branch {
                session,
                depth,
                active,
                last,
                entries,
                forks,
                busy_below,
                ..
            } => {
                // ONE ROW, always. A sibling branch is a door with a label on
                // it — how many turns are behind it, and whether it forks again
                // — and never a subtree drawn in place.
                spans.push(Span::styled(
                    format!(
                        "{}{} ",
                        "  ".repeat(depth.saturating_sub(1)),
                        if *last { "└" } else { "├" }
                    ),
                    if *active {
                        Style::default().fg(Color::Green)
                    } else {
                        dim
                    },
                ));
                spans.push(Span::styled(
                    if *active { "● " } else { "▸ " }.to_string(),
                    if *active {
                        Style::default().fg(Color::Green)
                    } else {
                        dim
                    },
                ));
                spans.push(Span::styled(
                    clip(
                        &title_of(session),
                        12.max(44usize.saturating_sub(depth * 2)),
                    ),
                    if sel || *active {
                        bold
                    } else {
                        Style::default()
                    },
                ));
                if *active {
                    spans.push(Span::styled("  active", Style::default().fg(Color::Green)));
                } else if let Some(n) = entries.filter(|n| *n > 0) {
                    spans.push(Span::styled(
                        format!(" · {n} entr{}", if n == 1 { "y" } else { "ies" }),
                        dim,
                    ));
                }
                if *forks > 0 {
                    spans.push(Span::styled(format!("  ⑂ {forks}"), warn));
                }
                // One row is all a branch gets. If it is hiding running work,
                // this is the only place that can be said.
                if let Some((glyph, color)) = status_mark(session, *busy_below) {
                    spans.push(Span::styled(
                        format!("  {glyph}"),
                        Style::default().fg(color),
                    ));
                }
                if *busy_below > 0 {
                    spans.push(Span::styled(
                        format!(" {busy_below} running"),
                        Style::default().fg(Color::Cyan),
                    ));
                }
                right = (p.now > 0).then(|| age(p.now, session.session.created_at));
            }
            ForestRow::Message {
                depth,
                role,
                gist,
                matched,
                active,
                created_at,
                tools,
                tools_open,
                branches,
                on_path,
                leaf,
                ..
            } => {
                // A turn rides the TRUNK: one guide character, at the same
                // column as every other turn in the conversation. A branch
                // point trades that guide for the fork mark, which is the only
                // place the eye has to slow down.
                spans.push(Span::styled(
                    format!("{indent}{} ", if *branches > 0 { "⑂" } else { "│" }),
                    if *branches > 0 {
                        warn
                    } else if *on_path {
                        Style::default().fg(Color::Green)
                    } else {
                        dim
                    },
                ));
                spans.push(Span::styled(
                    format!("{:<6}", role_label(*role)),
                    Style::default()
                        .fg(role_color(*role))
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    clip(gist, 12.max(50usize.saturating_sub(depth * 2))),
                    if sel { bold } else { Style::default() },
                ));
                if *tools > 0 {
                    spans.push(Span::styled(
                        format!(
                            "  {} {tools} tool{}",
                            if *tools_open { "▾" } else { "▸" },
                            if *tools == 1 { "" } else { "s" }
                        ),
                        dim,
                    ));
                }
                if *branches > 0 {
                    spans.push(Span::styled(
                        format!(
                            "  {branches} branch{}",
                            if *branches == 1 { "" } else { "es" }
                        ),
                        warn,
                    ));
                }
                if *matched {
                    // The row that actually said the word.
                    spans.push(Span::styled("  ◂ match", Style::default().fg(info())));
                }
                if *leaf {
                    spans.push(Span::styled("  ◀ leaf", warn.add_modifier(Modifier::BOLD)));
                } else if *active && *branches == 0 {
                    // NOT on a branch point: the trunk carries on below it, so
                    // "the next turn lands here" would be a lie about a row the
                    // conversation has already moved past.
                    spans.push(Span::styled("  ← active", dim));
                }
                right = (p.now > 0 && *created_at > 0).then(|| age(p.now, *created_at));
            }
            ForestRow::Section { depth, label, .. } => {
                // A CAPTION, not a row you act on.
                spans = vec![Span::styled(
                    format!(
                        "{cursor}{indent}── {}",
                        clip(label, 12.max(56usize.saturating_sub(depth * 2)))
                    ),
                    dim,
                )];
            }
            ForestRow::Session {
                session,
                depth,
                open,
                delegated,
                current,
                busy_below,
                expandable,
                ..
            } => {
                spans.push(Span::raw(indent.clone()));
                // The disclosure comes FIRST and is present on every
                // conversation with anything under it: it is the one mark
                // saying this row is a door.
                spans.push(Span::styled(
                    if *expandable {
                        if *open {
                            "▾ "
                        } else {
                            "▸ "
                        }
                    } else {
                        "  "
                    },
                    dim,
                ));
                spans.push(Span::styled(
                    kind_glyph(session),
                    if is_delegated(session.session.kind) {
                        Style::default()
                    } else {
                        dim
                    },
                ));
                match status_mark(session, *busy_below) {
                    Some((glyph, color)) => spans.push(Span::styled(
                        format!(" {glyph}"),
                        Style::default().fg(color),
                    )),
                    None => spans.push(Span::raw("  ")),
                }
                let mut title_style = if sel || *current {
                    bold
                } else {
                    Style::default()
                };
                if *current {
                    title_style = title_style.fg(Color::Green);
                }
                spans.push(Span::styled(
                    format!(
                        " {}",
                        clip(
                            &title_of(session),
                            12.max(46usize.saturating_sub(depth * 2))
                        )
                    ),
                    title_style,
                ));
                if *delegated > 0 {
                    spans.push(Span::styled(format!("  ⋯{delegated}"), dim));
                }
                // Named, not just glyphed: `⋯` says something is live, this
                // says how much — the difference between "look inside" and
                // "leave it alone".
                if *busy_below > 0 {
                    spans.push(Span::styled(
                        format!("  {busy_below} running"),
                        Style::default().fg(Color::Cyan),
                    ));
                }
                if *depth == 0 {
                    if let (Some(ws), Some(open_ws)) = (&session.session.workspace, p.workspace) {
                        if ws != open_ws {
                            let tail: Vec<&str> = ws.split('/').collect();
                            let short = tail[tail.len().saturating_sub(2)..].join("/");
                            spans.push(Span::styled(format!("  {short}"), dim));
                        }
                    }
                }
                if let Some(cost) = session.cost_usd.filter(|c| *c != 0.0) {
                    spans.push(Span::styled(format!("  {}", fmt_usd(cost)), dim));
                }
                right = (p.now > 0).then(|| age(p.now, session.session.created_at));
            }
        }
        out.push(with_age(spans, right, p.cols));
    }

    // The marks first, then the keys: the glyphs are what the reader is
    // looking at, and the keys are what they do next.
    out.push(Line::from(Span::styled(
        legend_line(&mark_legend(window), p.cols),
        dim,
    )));
    let mut keys: Vec<String> = Vec::new();
    if p.rows.len() > shown {
        keys.push(format!("{}/{}", p.selected + 1, p.rows.len()));
    }
    keys.extend(
        [
            "↑↓ move",
            "→← turns",
            "→ on a turn shows its tools",
            "⏎ open",
            "⏎ on a branch re-roots",
            "⏎ on a turn forks",
            "e splits",
            "m brings here",
            "/ find",
            if p.crumbs.is_empty() {
                "esc back"
            } else {
                "esc up one level"
            },
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    out.push(Line::from(Span::styled(legend_line(&keys, p.cols), dim)));
    if let Some(message) = p.message {
        out.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(crate::components::warn()),
        )));
    }
    out
}

pub fn render_tree(p: &TreeProps, area: Rect, buf: &mut Buffer) {
    paint_rows(&tree_lines(p), area, buf);
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/Tree.test.ts and Panel.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::panel::test_render::draw_panel;
    use crate::components::panel::{panel_body_rows, PanelBody};
    use crate::forest::fixtures::{msg, session_row, with_origin, with_status};
    use crate::forest::{forest_rows, ForestInput};
    use crate::keys::PanelTab;
    use std::collections::{HashMap, HashSet};

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn rendered(p: &TreeProps) -> String {
        tree_lines(p)
            .iter()
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- outcome markers --------------------------------------------------

    #[test]
    fn a_session_that_never_ran_a_turn_gets_no_marker() {
        assert_eq!(
            status_mark(&session_row("x", SessionKind::Root, 1), 0),
            None
        );
    }

    #[test]
    fn a_failed_delegation_is_marked_even_when_its_turn_ended_cleanly() {
        let mut failed = with_status(session_row("x", SessionKind::Subagent, 1), TurnStatus::Done);
        failed.session.outcome_ok = Some(false);
        assert_eq!(status_mark(&failed, 0), Some(("✗", Color::Red)));
        let mut ok = with_status(session_row("y", SessionKind::Subagent, 2), TurnStatus::Done);
        ok.session.outcome_ok = Some(true);
        assert_eq!(status_mark(&ok, 0).unwrap().0, "✓");
    }

    #[test]
    fn a_restart_orphaned_branch_is_distinguishable_from_a_failed_one() {
        let orphaned = with_status(session_row("x", SessionKind::Fork, 1), TurnStatus::Orphaned);
        assert_eq!(status_mark(&orphaned, 0).unwrap().0, "◼");
        let errored = with_status(session_row("y", SessionKind::Fork, 2), TurnStatus::Error);
        assert_eq!(status_mark(&errored, 0).unwrap().0, "✗");
        let mut busy = session_row("z", SessionKind::Fork, 3);
        busy.busy = true;
        assert_eq!(status_mark(&busy, 0).unwrap().0, "⋯");
    }

    #[test]
    fn titles_drop_the_kind_prefix_the_server_stamped_on_them() {
        let mut s = session_row("x", SessionKind::Subagent, 1);
        s.session.title = "subagent · review app.ts".into();
        assert_eq!(title_of(&s), "review app.ts");

        // The WORKSPACE before "(untitled)".
        let mut y = session_row("y", SessionKind::Root, 2);
        y.session.title = String::new();
        y.session.workspace = Some("/Users/a/repos/bough".into());
        assert_eq!(title_of(&y), "bough");
        y.session.workspace = Some("/tmp/proj/".into());
        assert_eq!(title_of(&y), "proj");
        y.session.workspace = None;
        assert_eq!(title_of(&y), "(untitled)");

        // A handoff of a still-untitled conversation is titled `handoff · `
        // server-side — the row rendered as a prefix with nothing after it.
        let mut z = session_row("z", SessionKind::Root, 3);
        z.session.title = "handoff · ".into();
        assert_eq!(title_of(&z), "(untitled)");
        z.session.title = "handoff · fix the parser".into();
        assert_eq!(title_of(&z), "fix the parser");
    }

    #[test]
    fn live_work_under_a_collapsed_conversation_is_glyphed_not_hidden() {
        let idle = with_status(session_row("x", SessionKind::Root, 1), TurnStatus::Done);
        assert_eq!(status_mark(&idle, 0).unwrap().0, "✓");
        assert_eq!(status_mark(&idle, 5), Some(("⋯", Color::Cyan)));
        let mut busy = session_row("y", SessionKind::Root, 2);
        busy.busy = true;
        assert_eq!(status_mark(&busy, 0).unwrap().0, "⋯");
    }

    #[test]
    fn a_root_that_came_from_another_conversation_is_marked_as_derived() {
        assert_eq!(kind_glyph(&session_row("a", SessionKind::Root, 1)), "●");
        assert_eq!(
            kind_glyph(&with_origin(session_row("b", SessionKind::Root, 2), "a")),
            "↦"
        );
        // The mark distinguishes; it does not replace the kinds that have one.
        assert_eq!(
            kind_glyph(&with_origin(session_row("c", SessionKind::Fork, 3), "a")),
            "⑂"
        );
        assert_eq!(
            kind_glyph(&with_origin(
                session_row("d", SessionKind::Compaction, 4),
                "a"
            )),
            "≣"
        );
        assert_eq!(
            kind_glyph(&with_origin(
                session_row("e", SessionKind::Subagent, 5),
                "a"
            )),
            "◆"
        );
    }

    // ---- the mark legend ---------------------------------------------------

    fn forest_session(s: crate::api::SessionRow, busy_below: usize) -> ForestRow {
        ForestRow::Session {
            id: s.session.id.clone(),
            session: s,
            depth: 0,
            open: false,
            delegated: 0,
            current: false,
            busy_below,
            expandable: false,
        }
    }

    #[test]
    fn the_mark_legend_explains_what_is_on_screen_and_only_that() {
        let legend = mark_legend(&[
            forest_session(
                with_status(session_row("a", SessionKind::Root, 1), TurnStatus::Done),
                0,
            ),
            forest_session(
                with_status(session_row("b", SessionKind::Fork, 2), TurnStatus::Error),
                0,
            ),
        ]);
        assert_eq!(legend, vec!["● yours", "⑂ fork", "✓ done", "✗ failed"]);
        assert!(!legend.iter().any(|l| l.contains("compaction")));
        assert!(!legend.iter().any(|l| l.contains("subagent")));
    }

    #[test]
    fn a_derived_root_reads_as_a_handoff_not_as_one_you_started() {
        let legend = mark_legend(&[forest_session(
            with_origin(session_row("h", SessionKind::Root, 1), "x"),
            0,
        )]);
        assert_eq!(legend, vec!["↦ handoff"]);
    }

    #[test]
    fn rows_that_carry_no_mark_contribute_nothing_to_the_legend() {
        let legend = mark_legend(&[ForestRow::Section {
            id: "s".into(),
            session_id: "a".into(),
            depth: 0,
            label: "topic".into(),
        }]);
        assert!(legend.is_empty());
    }

    #[test]
    fn work_running_below_a_collapsed_row_is_legended_as_running() {
        let legend = mark_legend(&[forest_session(
            with_status(session_row("r", SessionKind::Root, 1), TurnStatus::Done),
            3,
        )]);
        assert_eq!(legend, vec!["● yours", "⋯ running"]);
    }

    // ---- the painted rows ---------------------------------------------------

    #[test]
    fn the_tree_tab_lists_conversations_newest_first_delegated_work_collapsed() {
        let mut a = session_row("a", SessionKind::Root, 1_000);
        a.session.title = "wire the panel".into();
        let mut b = session_row("b", SessionKind::Root, 3_000);
        b.session.title = "nightly bench".into();
        let mut c = with_origin(session_row("c", SessionKind::Subagent, 4_000), "a");
        c.session.title = "subagent · review".into();
        let sessions = vec![a, b];
        let children = HashMap::from([("a".to_string(), vec![c])]);
        let expanded: HashSet<String> = HashSet::from(["a".to_string()]);
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            children_by_origin: &children,
            expanded: &expanded,
            current_id: Some("a"),
            ..Default::default()
        });
        let body = PanelBody::Tree(TreeProps {
            rows: &items,
            selected: 0,
            height: panel_body_rows(12),
            ..Default::default()
        });
        let frame = draw_panel(PanelTab::Tree, &body, 100, 12).join("\n");
        assert!(frame.contains("nightly bench"), "{frame}");
        assert!(frame.contains("wire the panel"), "{frame}");
        // The subagent is a COUNT, not a row of its own, until it is drilled into.
        assert!(!frame.contains("review"), "{frame}");
        assert!(frame.contains("1 spawned"), "{frame}");
    }

    #[test]
    fn the_tree_legend_is_the_last_row_at_every_height_that_has_one() {
        let sessions: Vec<crate::api::SessionRow> = (0..27)
            .map(|i| {
                let mut s = session_row(&format!("s{i}"), SessionKind::Root, 1_000 - i);
                s.session.title = format!("session number {i}");
                s
            })
            .collect();
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            ..Default::default()
        });
        for h in [4usize, 6, 8, 12, 20] {
            let body = PanelBody::Tree(TreeProps {
                rows: &items,
                selected: 0,
                height: panel_body_rows(h),
                cols: Some(96),
                ..Default::default()
            });
            let painted = draw_panel(PanelTab::Tree, &body, 100, h);
            let last = painted
                .iter()
                .rfind(|r| r.contains('│'))
                .cloned()
                .unwrap_or_default();
            assert!(
                last.contains("↑↓ move"),
                "@{h}: the last row is not the legend: {last}"
            );
        }
    }

    /// The 2a screen, end to end: a trunk, a branch point, one sibling
    /// expanded inline and the others collapsed to a row each.
    #[test]
    fn a_branch_point_fans_out_once_and_the_trunk_picks_straight_back_up() {
        let root = session_row("root", SessionKind::Root, 1);
        let fork = |id: &str, title: &str, at: i64| {
            let mut f = with_origin(session_row(id, SessionKind::Fork, at), "root");
            f.session.origin_message_id = Some("m2".into());
            f.session.title = title.into();
            f
        };
        let sessions = vec![
            root,
            fork("a", "smaller blast radius", 2),
            fork("b", "keep offsets, add an index", 3),
            fork("c", "try a cursor-based approach", 4),
        ];
        let threads = HashMap::from([
            (
                "root".to_string(),
                vec![
                    msg("m1", Role::User, "add a subtract function"),
                    msg("m2", Role::User, "refactor the offsets"),
                ],
            ),
            (
                "a".to_string(),
                vec![msg("ma", Role::Supervisor, "patched the writer")],
            ),
            (
                "b".to_string(),
                vec![msg("mb", Role::Supervisor, "indexing alongside")],
            ),
        ]);
        let expanded: HashSet<String> = ids(&["root", "a", "b", "c"]);
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            current_id: Some("b"),
            ..Default::default()
        });
        let frame = rendered(&TreeProps {
            rows: &items,
            height: 14,
            cols: Some(96),
            ..Default::default()
        });
        // The branch point trades its guide for the fork mark and says how
        // many ways it went.
        assert!(
            frame.contains("⑂ you   refactor the offsets  3 branches"),
            "{frame}"
        );
        // …and it does NOT claim to be where the next turn lands. The trunk
        // carries on below it, through the branch that was taken.
        assert!(
            !frame.contains("3 branches  ← active"),
            "a branch point is not the leaf: {frame}"
        );
        // The sibling nobody is on: ONE row, with what is behind it.
        assert!(
            frame.contains("├ ▸ smaller blast radius · 1 entry"),
            "{frame}"
        );
        // The one carrying the trunk, and its turn back at the trunk column.
        assert!(
            frame.contains("├ ● keep offsets, add an index  active"),
            "{frame}"
        );
        assert!(frame.contains("│ bough indexing alongside"), "{frame}");
        // Its turn is the leaf: the open conversation is where the next turn
        // appends, and that is what makes "go back to here" concrete.
        assert!(frame.contains("◀ leaf"), "{frame}");
        assert!(frame.contains("└ ▸ try a cursor-based approach"), "{frame}");
        // AND THE INDENT NEVER GREW. Every turn starts at the same column.
        let turn_cols: Vec<usize> = frame
            .lines()
            .filter(|l| l.contains("│ you") || l.contains("│ bough"))
            .map(|l| l.find('│').unwrap())
            .collect();
        assert!(turn_cols.windows(2).all(|w| w[0] == w[1]), "{frame}");
    }

    #[test]
    fn a_turns_tools_unfold_under_it_and_the_chip_says_how_many() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let mut m = msg("m1", Role::Supervisor, "reading it first");
        m.parts.push(bough_core::schema::parts::Part::ToolCall {
            id: "c1".into(),
            name: "run_steps".into(),
            input: serde_json::json!({"code": "await read(\"util.ts\")"}),
        });
        let threads = HashMap::from([("root".to_string(), vec![m])]);
        let expanded = ids(&["root"]);
        let base = ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        };
        let folded = rendered(&TreeProps {
            rows: &forest_rows(&base),
            height: 10,
            cols: Some(96),
            ..Default::default()
        });
        assert!(folded.contains("▸ 1 tool"), "{folded}");
        assert!(!folded.contains("✦"), "{folded}");
        let open = ids(&["m1"]);
        let unfolded = rendered(&TreeProps {
            rows: &forest_rows(&ForestInput {
                tools_open: &open,
                ..base
            }),
            height: 10,
            cols: Some(96),
            ..Default::default()
        });
        assert!(unfolded.contains("▾ 1 tool"), "{unfolded}");
        assert!(unfolded.contains("✦ run_steps"), "{unfolded}");
        assert!(unfolded.contains("util.ts"), "{unfolded}");
    }

    #[test]
    fn the_age_column_is_right_aligned_and_dropped_rather_than_truncated() {
        let mut s = session_row("root", SessionKind::Root, 0);
        s.session.title = "wire the panel".into();
        let items = forest_rows(&ForestInput {
            sessions: std::slice::from_ref(&s),
            ..Default::default()
        });
        let now = 2 * 3600 * 1000;
        let wide = tree_lines(&TreeProps {
            rows: &items,
            height: 8,
            cols: Some(60),
            now,
            ..Default::default()
        });
        let row = text_of(&wide[0]);
        assert!(row.ends_with("2h"), "{row}");
        assert_eq!(row.chars().count(), 60, "the age sits in the last column");
        // Too narrow to hold both: the row keeps its words. A `2` that used to
        // be `2h` is a worse lie than no timestamp at all.
        let narrow = tree_lines(&TreeProps {
            rows: &items,
            height: 8,
            cols: Some(22),
            now,
            ..Default::default()
        });
        assert!(
            !text_of(&narrow[0]).ends_with("2h"),
            "{:?}",
            text_of(&narrow[0])
        );
    }

    #[test]
    fn a_re_rooted_view_says_where_it_is_instead_of_indenting_to_show_it() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let threads = HashMap::from([(
            "root".to_string(),
            vec![msg("m1", Role::User, "what about resuming mid-page?")],
        )]);
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            root_id: Some("root"),
            ..Default::default()
        });
        let crumbs = vec![
            "refactor the offsets".to_string(),
            "try a cursor-based approach".to_string(),
        ];
        let frame = rendered(&TreeProps {
            rows: &items,
            height: 10,
            cols: Some(96),
            crumbs: &crumbs,
            ..Default::default()
        });
        assert!(
            frame.contains("⌂ ▸ refactor the offsets ▸ try a cursor-based approach"),
            "{frame}"
        );
        assert!(frame.contains("esc up one level"), "{frame}");
        // Two levels in, and the turn is still at column zero.
        assert!(frame.contains("│ you   what about resuming"), "{frame}");
        assert_eq!(items[0].depth(), 0, "two levels in, still at column zero");
    }

    #[test]
    fn an_empty_filtered_list_says_what_was_searched() {
        let filtered = rendered(&TreeProps {
            rows: &[],
            height: 6,
            filter: Some("compound"),
            ..Default::default()
        });
        assert!(
            filtered.contains("nothing matches \"compound\" — titles, paths or messages"),
            "{filtered}"
        );
        let empty = rendered(&TreeProps {
            rows: &[],
            height: 6,
            ..Default::default()
        });
        assert!(empty.contains("no conversations yet"), "{empty}");
    }

    #[test]
    fn the_message_and_the_filter_echo_each_take_a_row_from_the_list() {
        let sessions: Vec<crate::api::SessionRow> = (0..20)
            .map(|i| session_row(&format!("s{i}"), SessionKind::Root, i))
            .collect();
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            ..Default::default()
        });
        let plain = tree_lines(&TreeProps {
            rows: &items,
            height: 10,
            ..Default::default()
        });
        assert_eq!(plain.len(), 10);
        let with_both = tree_lines(&TreeProps {
            rows: &items,
            height: 10,
            filtering: true,
            message: Some("e splits a conversation at a TURN — move onto one first"),
            ..Default::default()
        });
        // Same budget: the filter echo and the message came out of the LIST.
        assert_eq!(with_both.len(), 10);
        assert!(text_of(&with_both[0]).starts_with("/ "));
        assert!(text_of(with_both.last().unwrap()).contains("e splits a conversation"));
    }

    #[test]
    fn a_turn_row_names_who_said_it_and_marks_the_active_leaf() {
        let sessions = vec![session_row("root", SessionKind::Root, 1)];
        let threads = HashMap::from([(
            "root".to_string(),
            vec![
                msg("m1", Role::User, "add a discount"),
                msg("m2", Role::Supervisor, "done"),
            ],
        )]);
        let expanded: HashSet<String> = HashSet::from(["root".to_string()]);
        let items = forest_rows(&ForestInput {
            sessions: &sessions,
            threads: &threads,
            expanded: &expanded,
            ..Default::default()
        });
        let frame = rendered(&TreeProps {
            rows: &items,
            height: 8,
            ..Default::default()
        });
        // THE TRUNK: both turns at the same column, one guide character each.
        assert!(frame.contains("│ you   add a discount"), "{frame}");
        assert!(frame.contains("│ bough done  ← active"), "{frame}");
    }
}
