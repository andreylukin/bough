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

/// Column 0 of the focused lane's head line (visual audit): the transcript's row-focus glyph.
pub const FOCUS_MARKER: char = '\u{258c}';
/// After the leader's name (visual audit: "make it obvious who the leader is").
pub const LEADER_TAG: &str = " \u{2726} leader";
/// Before the intent half, in place of the 23-column label (visual audit F8).
pub const INTENT_MARK: &str = "\u{2192} ";

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
    /// The agent the `leader` set is mounted in (visual audit: "make it obvious who the leader
    /// is"). Folded from the `leader` key when the row is mounted; with no leader row nobody is.
    pub leader: bool,
    /// A dim role word after the name (`unattended`, `memory`) from `StripConfig::roles`: the
    /// rail answers "what does this lane do" without headers. The leader tag outranks it.
    pub role: Option<String>,
    /// Its last message was a question to Andrey and nothing from him followed (round 10).
    pub question: bool,
    /// When the CURRENT status began (the TUI brief, D4: "how long" was the runner-up rail
    /// fact). `None` until the first status change this life; the row then shows no clock
    /// rather than a guessed one.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// The elapsed text the pane stamped for THIS frame (`clock_text`), empty when unknown.
    /// Stamped by the pane, not computed in `row_lines`, so the pure renderer needs no clock.
    pub clock: String,
    /// Messages in the lane's inbox that no wake has claimed. Read from the live handle whenever
    /// a mail step lands (visual audit follow-up): which lane needs attention is the rail's job.
    pub waiting: usize,
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
    // Dormant is the state that decides whether mail is answered, so it is the LAST thing the
    // rail may render faintly (visual audit F6): the warn role, not dim.
    if dormant {
        return ('\u{25CC}', "warn");
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
        "interactive" => theme.interactive,
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

/// PURE: how long a status has held, in the shortest honest unit — `41s`, `2m`, `1h05`. Empty
/// for an unknown start (no clock is better than a wrong one).
pub fn clock_text(
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let Some(since) = since else {
        return String::new();
    };
    let secs = (now - since).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{:02}", secs / 3600, (secs % 3600) / 60)
    }
}

/// PURE: the lane Up/Down from the rail moves to — the previous/next row in rail order (the
/// leader first, as drawn), wrapping. `None` with no rows.
pub fn step_focus(rows: &[RailRow], focused: Option<&AgentId>, down: bool) -> Option<FocusRequest> {
    if rows.is_empty() {
        return None;
    }
    let mut order: Vec<&RailRow> = rows.iter().collect();
    order.sort_by_key(|r| !r.leader);
    let at = focused.and_then(|f| order.iter().position(|r| &r.agent == f));
    let next = match (at, down) {
        (None, _) => 0,
        (Some(i), true) => (i + 1) % order.len(),
        (Some(i), false) => (i + order.len() - 1) % order.len(),
    };
    Some(FocusRequest {
        agent: Some(order[next].agent.clone()),
        ..Default::default()
    })
}

/// PURE: whether the lane's last word to Andrey was a question he has not answered: the newest
/// `thought/text` ends with `?` and no `mail/delivered` from Andrey is newer than it.
pub fn pending_question(steps: &[bough_plugin_ledger::Step]) -> bool {
    let last_text = steps
        .iter()
        .rposition(|s| s.kind.as_str() == "thought/text");
    let Some(i) = last_text else {
        return false;
    };
    let asks = steps[i]
        .body
        .get("text")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t.trim_end().ends_with('?'));
    let answered = steps[i + 1..].iter().any(|s| {
        s.kind.as_str() == "mail/delivered"
            && s.body.get("from").and_then(|v| v.as_str()) == Some("andrey")
    });
    asks && !answered
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
pub fn row_height(row: &RailRow, show_about: bool, about_lines: u16, width: u16) -> u16 {
    1 + about_extent(row, show_about, about_lines, width)
}

/// The two halves of an about-line as the lines they take at `width` (the TUI brief, D4): each
/// half WRAPS to at most `about_lines` lines instead of being cut at the rail's edge, because
/// "what is this lane doing" was the one rail fact Andrey asked for, and a clipped fragment
/// (`. Added doc comment "/// Prin…`) does not answer it. Past the cap the last line is elided,
/// so a cut is still visible as a cut.
fn about_halves(row: &RailRow, about_lines: u16, width: u16) -> (Vec<String>, Vec<String>) {
    let Some(v) = row.about.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let body_room = width.saturating_sub(2);
    let label_room = body_room.saturating_sub(INTENT_MARK.chars().count() as u16);
    let cap = about_lines as usize;
    let fold = |text: &str, room: u16| -> Vec<String> {
        let text = text.trim();
        if text.is_empty() || room == 0 {
            return Vec::new();
        }
        let mut lines = bough_plugin_tui_render::wrap(text, room);
        if lines.len() > cap {
            lines.truncate(cap);
            if let Some(last) = lines.last_mut() {
                *last = elide(&format!("{last} \u{2026}"), room as usize);
                if !last.ends_with('\u{2026}') {
                    last.push('\u{2026}');
                }
            }
        }
        lines
    };
    (fold(&v.state, body_room), fold(&v.intent, label_room))
}

fn about_extent(row: &RailRow, show_about: bool, about_lines: u16, width: u16) -> u16 {
    if !show_about || about_lines == 0 {
        return 0;
    }
    let (state, intent) = about_halves(row, about_lines, width);
    (state.len() + intent.len()) as u16
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
    // A lane with no work yet (round 10) is dim — name and state — until its first turn; the
    // leader never dims. Two full-weight rows for a lane that did nothing all session took the
    // rail's space from the lane that did.
    let quiet =
        row.about.is_none() && !row.leader && !row.disposed && row.status != Status::Running;
    let mut head_style = Style::default().fg(if quiet {
        theme.dim
    } else {
        role_color(theme, role)
    });
    if focused {
        head_style = head_style.add_modifier(Modifier::BOLD);
    }
    // The state word, and how long it has held (D4): `running 2m`, `idle 41s`.
    let word = if row.clock.is_empty() {
        status_word(row).to_string()
    } else {
        format!("{} {}", status_word(row), row.clock)
    };
    let word_w = word.chars().count() as u16;
    // Column 0 is the FOCUS MARKER (visual audit: the focused lane was bold and nothing else,
    // which no persona noticed). Same glyph the transcript's row focus uses, so one mark means
    // "the keyboard's conversation" everywhere.
    let marker = if focused { FOCUS_MARKER } else { ' ' };
    let leader_tag = if row.leader { LEADER_TAG } else { "" };
    // Unread mail, right before the state word: `✉ 3 idle` says "asleep with three waiting"
    // at a glance. Nothing when the inbox is empty — the rail stays quiet when nothing is owed.
    let mail = if row.waiting > 0 {
        format!("\u{2709} {} ", row.waiting)
    } else {
        String::new()
    };
    // What this lane is waiting on from Andrey (round 10): `?` a question — right after the
    // name, in the warn colour, so the rail answers "do I owe anything" on its own.
    let mut owed = String::new();
    if row.question {
        owed.push_str(" ?");
    }
    let role_tag = match (&row.role, row.leader) {
        (Some(role), false) => format!(" {role}"),
        _ => String::new(),
    };
    let name_room = width.saturating_sub(
        3 + leader_tag.chars().count() as u16
            + role_tag.chars().count() as u16
            + owed.chars().count() as u16
            + mail.chars().count() as u16
            + word_w
            + 1,
    ) as usize;
    let name = elide(&row.name, name_room);
    let mut head = vec![
        Span::styled(marker.to_string(), Style::default().fg(theme.accent)),
        Span::styled(format!("{g} "), head_style),
        Span::styled(
            name.clone(),
            Style::default()
                .fg(if quiet { theme.dim } else { theme.fg })
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ];
    if row.leader {
        head.push(Span::styled(
            leader_tag.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !role_tag.is_empty() {
        head.push(Span::styled(
            role_tag.clone(),
            Style::default().fg(theme.dim),
        ));
    }
    if !owed.is_empty() {
        head.push(Span::styled(
            owed.clone(),
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        ));
    }
    let used = 3
        + name.chars().count()
        + leader_tag.chars().count()
        + role_tag.chars().count()
        + owed.chars().count();
    let pad = (width as usize).saturating_sub(used + mail.chars().count() + word_w as usize);
    head.push(Span::raw(" ".repeat(pad)));
    if !mail.is_empty() {
        head.push(Span::styled(mail, Style::default().fg(theme.evidence)));
    }
    head.push(Span::styled(
        word,
        Style::default().fg(if quiet {
            theme.dim
        } else {
            role_color(theme, role)
        }),
    ));
    let mut out = vec![Line::from(head)];

    if !show_about || about_lines == 0 {
        return out;
    }
    let (state, intent) = about_halves(row, about_lines, width);
    // The STATE half: evidence, and only ever reached here through `about_from_step`, which
    // reads a cited step (`crate::invariant`). Wrapped, never cut, up to the cap (D4).
    for l in state {
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(l, Style::default().fg(theme.evidence)),
        ]));
    }
    // The INTENT half, ALWAYS marked as such. §2: self-declared, never truth. The mark is the
    // arrow plus the `thought` colour and italics — the words "intent (self-declared)" used to
    // spend 23 of the rail's 34 columns (visual audit F8); the full label stays in `/help`. The
    // mark sits on the first line only; continuation lines indent under it, so the whole half
    // reads as one qualified claim.
    for (i, l) in intent.into_iter().enumerate() {
        let mark = if i == 0 {
            INTENT_MARK.to_string()
        } else {
            " ".repeat(INTENT_MARK.chars().count())
        };
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                mark,
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                l,
                Style::default()
                    .fg(theme.thought)
                    .add_modifier(Modifier::ITALIC),
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
/// The spelling that makes an agent a WORKER of a lane: `<lane>/worker-<uuid>`.
pub const WORKER_SEP: &str = "/worker-";

/// The lane a worker belongs to, from its name; `None` for a lane.
pub fn worker_parent(name: &str) -> Option<&str> {
    name.split_once(WORKER_SEP).map(|(parent, _)| parent)
}

/// A worker's display name when its group is expanded: indented, uuid cut to eight chars —
/// `nas-event-log/worker-01a058fd-…` reads as `  …01a058fd` under its parent.
pub fn short_worker(name: &str) -> String {
    let id = name.split_once(WORKER_SEP).map(|(_, id)| id).unwrap_or(name);
    let id: String = id.chars().take(8).collect();
    format!("  \u{2026}{id}")
}

/// What one display row IS, aligned with [`display`]'s returned rows.
#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    Lane,
    Worker,
    /// A collapsed worker group under the named lane. Clicking it expands, never focuses.
    Badge(String),
}

/// PURE: the rows the rail DRAWS, from the rows that exist (the persona brainstorm, 2026-09-01:
/// "collapsed by default, and the badge must roll up attention or it hides stuck workers").
///
/// Lanes stay a flat list, leader first. A lane's workers collapse into ONE badge row —
/// `▸ 2 workers`, Running if any child runs, `?` if any child holds a question, mail summed —
/// unless the lane's name is in `expanded`, where they render individually, indented and
/// shortened. Collapsed DISPOSED workers vanish entirely (a worker's receipt is noise); an
/// orphan worker whose lane is gone stays a plain row at the end. `roles` decorates lanes.
pub fn display(
    rows: &[RailRow],
    expanded: &std::collections::BTreeSet<String>,
    roles: &std::collections::BTreeMap<String, String>,
) -> (Vec<RailRow>, Vec<RowKind>) {
    let mut lanes: Vec<&RailRow> = rows
        .iter()
        .filter(|r| worker_parent(&r.name).is_none())
        .collect();
    lanes.sort_by_key(|r| !r.leader);
    let workers: Vec<&RailRow> = rows
        .iter()
        .filter(|r| worker_parent(&r.name).is_some())
        .collect();
    let mut out: Vec<RailRow> = Vec::new();
    let mut kinds: Vec<RowKind> = Vec::new();

    for lane in &lanes {
        let mut row = (*lane).clone();
        row.role = roles.get(&row.name).cloned();
        out.push(row);
        kinds.push(RowKind::Lane);

        let kids: Vec<&RailRow> = workers
            .iter()
            .filter(|w| worker_parent(&w.name) == Some(lane.name.as_str()))
            .copied()
            .collect();
        if kids.is_empty() {
            continue;
        }
        if expanded.contains(&lane.name) {
            for k in kids {
                let mut row = k.clone();
                row.name = short_worker(&row.name);
                out.push(row);
                kinds.push(RowKind::Worker);
            }
        } else {
            let live: Vec<&RailRow> = kids.iter().filter(|k| !k.disposed).copied().collect();
            if live.is_empty() {
                continue;
            }
            let mut badge = kids[0].clone();
            badge.name = format!(
                "\u{25b8} {} worker{}",
                live.len(),
                if live.len() == 1 { "" } else { "s" }
            );
            badge.status = if live.iter().any(|k| k.status == Status::Running) {
                Status::Running
            } else {
                Status::Idle
            };
            badge.question = live.iter().any(|k| k.question);
            badge.waiting = live.iter().map(|k| k.waiting).sum();
            badge.wake_pending = live.iter().any(|k| k.wake_pending);
            badge.disposed = false;
            badge.dormant = false;
            badge.leader = false;
            badge.about = None;
            badge.clock = String::new();
            out.push(badge);
            kinds.push(RowKind::Badge(lane.name.clone()));
        }
    }
    // A worker whose lane is gone: shown plainly, never silently dropped.
    for w in &workers {
        if !lanes
            .iter()
            .any(|l| worker_parent(&w.name) == Some(l.name.as_str()))
        {
            out.push((*w).clone());
            kinds.push(RowKind::Worker);
        }
    }
    (out, kinds)
}

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

// ---------------------------------------------------------------------------
// phase ux1 §2.5: the rail's width, and a clip that cannot overflow
// ---------------------------------------------------------------------------

/// PURE: the rail's column count at a terminal width. `0` below `collapse_cols` (M13).
pub fn rail_width(total: u16, cfg: &crate::StripConfig) -> u16 {
    bough_plugin_tui_shell::responsive_width(
        total,
        cfg.collapse_cols,
        cfg.width,
        cfg.min_width,
        cfg.max_width,
    )
}

/// PURE: hard-clip one rail line to `width`, with a `…` when it clipped.
///
/// The audit's `idlePlease` and `running──` were two text runs sharing one baseline. A clip that
/// CANNOT overflow is what makes that impossible rather than unlikely (M9).
pub fn clip(line: Line<'static>, width: u16) -> Line<'static> {
    let width = width as usize;
    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width {
        return line;
    }
    // The cut costs a cell, so the visible text is one shorter than the room.
    let room = width - 1;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans.into_iter() {
        if used >= room {
            break;
        }
        let n = span.content.chars().count();
        if used + n <= room {
            used += n;
            out.push(span);
        } else {
            let keep = room - used;
            let text: String = span.content.chars().take(keep).collect();
            used = room;
            out.push(Span::styled(text, span.style));
        }
    }
    // The ellipsis carries the style of the run it cut, so a clipped status word does not change
    // colour halfway through.
    let style = out.last().map(|s| s.style).unwrap_or_default();
    out.push(Span::styled("\u{2026}".to_string(), style));
    Line::from(out)
}
