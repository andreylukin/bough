//! The live-work rail: everything running *right now* on this session's
//! behalf (port of `src/tui/components/SubagentRail.tsx` + `format.ts`'s
//! `unitLine`).
//!
//! THE INVARIANT THIS HOLDS: **the rail pins LIVE work only.** A finished
//! branch belongs in the tree and in the transcript's report note, both of
//! which outlive the run; a rail that keeps everything it ever saw grows past
//! the terminal on any real fan-out and pushes the composer off screen — which
//! is how the two agents actually working become the part you cannot see.
//!
//! SECOND — **every row is exactly ONE screen row.** The rows are truncated
//! and then padded to the full width: an unpadded short row leaves the tail of
//! the longer one that was there before it, and the rail redraws every second,
//! which is exactly the surface that bug shows on.
//!
//! SCHEDULES ARE THE DELIBERATE EXCEPTION to "live only": a schedule will fire
//! whether or not you are watching. Enabled ones sit at the BOTTOM, count down
//! instead of up, and `x` disables rather than kills.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::ansi::{line_from_ansi, truncate_ansi, width};
use crate::api::SessionRow;
use crate::components::pad_row_ansi;
use crate::forest::is_delegated;
use crate::format::{accent, bold, dim, info, warn};
use crate::store::selectors::{clip, fmt_duration, fmt_tokens, fmt_usd, LiveUnit, LiveUnitKind};

/// The delegated children of the open session with a turn in flight.
///
/// Still the lineage rule the rail is built on: a busy FORK is a sibling
/// conversation, not delegated work, and it belongs in the tree rather than
/// pinned under the composer.
pub fn live_subagents(children: &[SessionRow]) -> Vec<SessionRow> {
    let mut out: Vec<SessionRow> = children
        .iter()
        .filter(|s| is_delegated(s.session.kind) && s.busy)
        .cloned()
        .collect();
    out.sort_by_key(|s| s.session.created_at);
    out
}

/// The one-line hint shown when the composer, not the rail, has the cursor.
///
/// It counts by KIND, because "3 running" does not tell you whether to worry:
/// three shells is a build, three agents is a fan-out, and one of each is a
/// turn that has spread out.
pub fn rail_hint(units: &[LiveUnit]) -> String {
    let count = |kind: LiveUnitKind, one: &str, many: &str| -> String {
        let n = units.iter().filter(|u| u.kind == kind).count();
        if n == 0 {
            String::new()
        } else {
            format!("{n} {}", if n == 1 { one } else { many })
        }
    };
    let live: Vec<String> = [
        count(LiveUnitKind::Shell, "shell", "shells"),
        count(LiveUnitKind::Subagent, "agent", "agents"),
        count(LiveUnitKind::Workflow, "run", "runs"),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    // Schedules are counted apart because "running" would be a lie about them
    // — a schedule row is a countdown, not work in flight.
    let scheduled = count(LiveUnitKind::Schedule, "scheduled", "scheduled");
    let bits: Vec<String> = [
        if live.is_empty() { String::new() } else { format!("{} running", live.join(" · ")) },
        scheduled,
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    format!("↓ {}", bits.join(" · "))
}

const BAR_CELLS: usize = 8;

fn progress_bar(fraction: f64) -> String {
    let filled = ((fraction * BAR_CELLS as f64).round()).clamp(0.0, BAR_CELLS as f64) as usize;
    format!(
        "{}{} {}%",
        "█".repeat(filled),
        "░".repeat(BAR_CELLS - filled),
        (fraction.clamp(0.0, 1.0) * 100.0).round() as i64
    )
}

/// One row of the live-work rail: what is running, for how long, and what it
/// costs. Nothing is re-derived here — `LiveUnit` already carries elapsed,
/// tokens, spend and progress, because the numbers must be the same ones a
/// stop acts on. The DETAIL is last and is the only thing that clips.
pub fn unit_line(u: &LiveUnit, cols: usize) -> String {
    let glyph = match u.kind {
        LiveUnitKind::Shell => "⚙",
        LiveUnitKind::Subagent => "◆",
        LiveUnitKind::Schedule => "⏱",
        LiveUnitKind::Workflow => "⧉",
    };
    // A schedule's glyph is DIM where the live kinds get a colour: the rail is
    // a "what is happening" surface and a schedule is a thing that will happen.
    let hue: fn(&str) -> String = match u.kind {
        LiveUnitKind::Shell => warn,
        LiveUnitKind::Subagent => info,
        LiveUnitKind::Schedule => dim,
        LiveUnitKind::Workflow => accent,
    };
    // A schedule counts DOWN, and once due it says so: the ticker fires within
    // ~30s, and a negative countdown rendered as elapsed time would read as a
    // schedule that has been running.
    let mut bits: Vec<String> = vec![if u.kind == LiveUnitKind::Schedule {
        if u.elapsed_ms < 1000 {
            "due".to_string()
        } else {
            format!("in {}", fmt_duration(u.elapsed_ms))
        }
    } else {
        fmt_duration(u.elapsed_ms)
    }];
    if let Some(tokens) = u.tokens.filter(|t| *t > 0) {
        bits.push(format!("{} tok", fmt_tokens(tokens)));
    }
    if let Some(cost) = u.cost_usd.filter(|c| *c > 0.0) {
        bits.push(fmt_usd(cost));
    }
    if let Some(progress) = u.progress {
        bits.push(progress_bar(progress));
    }
    let name = clip(&u.title, 28);
    let tail = bits.join(" · ");
    // Two spaces separate the name from the numbers; the detail takes whatever
    // is left and is dropped entirely rather than rendered as an ellipsis.
    let room = cols as isize - width(&format!("{glyph} {name}")) as isize - width(&tail) as isize - 6;
    // A DETAIL THAT REPEATS THE NAME IS NOT CONTEXT: a background shell's title
    // and detail are both its command line, so the row read `⚙ sleep 120  5s ·
    // sleep 120`. Compared against the UNCLIPPED title, since `name` may carry
    // a trailing ellipsis that no detail starts with.
    let repeats = u
        .detail
        .as_ref()
        .is_some_and(|d| *d == u.title || d.starts_with(&u.title));
    let detail = match &u.detail {
        Some(d) if room >= 8 && !repeats => dim(&format!(" · {}", clip(d, room as usize))),
        _ => String::new(),
    };
    format!("{} {name}  {}{detail}", hue(glyph), dim(&tail))
}

/// The rail's rows, ANSI-styled and each exactly `width` columns.
///
/// `sel = None` is the composer having focus: the rail still renders, it
/// simply carries no selection and the last row becomes the counting hint.
/// That is what makes ↓-from-an-empty-composer a reversible move rather than a
/// mode switch. Empty units render NOTHING AT ALL, not an empty box.
pub fn rail_rows(
    units: &[LiveUnit],
    sel: Option<usize>,
    width_cols: usize,
    armed_id: Option<&str>,
) -> Vec<String> {
    if units.is_empty() {
        return Vec::new();
    }
    let w = width_cols.max(1);
    let mut rows: Vec<String> = Vec::new();
    for (i, u) in units.iter().enumerate() {
        let on = sel == Some(i);
        // The armed row says what the next press destroys, in the row's own
        // space — consent is never inferred, and the scope is said out loud.
        // A schedule's verbs differ: there is no output to open and `x`
        // disables rather than kills.
        let hint = if armed_id == Some(u.id.as_str()) {
            dim(if u.kind == LiveUnitKind::Schedule {
                "  x again disables it · esc cancels"
            } else {
                "  x again stops it · esc cancels"
            })
        } else if on {
            dim(if u.kind == LiveUnitKind::Schedule {
                "  ⏎ details · x disable · esc composer"
            } else {
                "  ⏎ open · x stop · esc composer"
            })
        } else {
            String::new()
        };
        let head = if on { bold(&info("❯")) } else { " ".to_string() };
        let text = format!("{head} {}{hint}", unit_line(u, w.saturating_sub(2)));
        rows.push(pad_row_ansi(&truncate_ansi(&text, w, "…"), w));
    }
    if sel.is_none() {
        rows.push(pad_row_ansi(&dim(&format!("  {}", rail_hint(units))), w));
    }
    rows
}

pub fn render_rail(
    units: &[LiveUnit],
    sel: Option<usize>,
    armed_id: Option<&str>,
    area: Rect,
    buf: &mut Buffer,
) {
    let rows = rail_rows(units, sel, area.width as usize, armed_id);
    for (i, row) in rows.iter().take(area.height as usize).enumerate() {
        let line: Line = line_from_ansi(row);
        buf.set_line(area.x, area.y + i as u16, &line, area.width);
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/SubagentRail.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::strip_ansi;
    use crate::forest::fixtures::{session_row, with_status};
    use bough_core::schema::parts::{SessionKind, TurnStatus};

    fn busy(id: &str, kind: SessionKind, created_at: i64) -> SessionRow {
        let mut s = session_row(id, kind, created_at);
        s.busy = true;
        s
    }

    #[test]
    fn only_running_delegated_sessions_reach_the_rail() {
        let children = vec![
            busy("busy-sub", SessionKind::Subagent, 1),
            with_status(session_row("done-sub", SessionKind::Subagent, 2), TurnStatus::Done),
            busy("busy-wf", SessionKind::WorkflowAgent, 3),
            with_status(session_row("failed-sub", SessionKind::Subagent, 4), TurnStatus::Error),
            // A fork can be busy too, and it is not delegated work — it is a
            // sibling conversation, and it belongs in the tree.
            busy("busy-fork", SessionKind::Fork, 5),
        ];
        let ids: Vec<String> =
            live_subagents(&children).iter().map(|s| s.session.id.clone()).collect();
        assert_eq!(ids, vec!["busy-sub", "busy-wf"]);
    }

    #[test]
    fn a_finished_agent_leaves_the_rail_with_no_cleanup_pass() {
        let running = busy("a", SessionKind::Subagent, 1);
        assert_eq!(live_subagents(std::slice::from_ref(&running)).len(), 1);
        let mut done = running.clone();
        done.busy = false;
        done.last_turn_status = Some(TurnStatus::Done);
        assert_eq!(live_subagents(&[done]).len(), 0);
    }

    #[test]
    fn rail_order_is_start_order_so_enter_opens_what_the_cursor_is_on() {
        let later = busy("later", SessionKind::Subagent, 2000);
        let earlier = busy("earlier", SessionKind::Subagent, 1000);
        let ids: Vec<String> = live_subagents(&[later, earlier])
            .iter()
            .map(|s| s.session.id.clone())
            .collect();
        assert_eq!(ids, vec!["earlier", "later"]);
    }

    fn unit(kind: LiveUnitKind, id: &str) -> LiveUnit {
        LiveUnit {
            kind,
            id: id.to_string(),
            session_id: id.to_string(),
            title: id.to_string(),
            elapsed_ms: 1000,
            tokens: None,
            cost_usd: None,
            progress: None,
            detail: None,
        }
    }

    #[test]
    fn an_empty_rail_is_nothing_at_all_not_an_empty_box() {
        assert!(rail_rows(&[], None, 80, None).is_empty());
    }

    #[test]
    fn the_hint_counts_by_kind_three_shells_and_three_agents_are_different_news() {
        assert_eq!(rail_hint(&[unit(LiveUnitKind::Shell, "bg_1")]), "↓ 1 shell running");
        assert_eq!(
            rail_hint(&[
                unit(LiveUnitKind::Shell, "bg_1"),
                unit(LiveUnitKind::Shell, "bg_2"),
                unit(LiveUnitKind::Subagent, "a"),
            ]),
            "↓ 2 shells · 1 agent running"
        );
        assert_eq!(rail_hint(&[unit(LiveUnitKind::Workflow, "run")]), "↓ 1 run running");
    }

    #[test]
    fn schedules_are_counted_apart_running_would_be_a_lie_about_a_countdown() {
        assert_eq!(
            rail_hint(&[unit(LiveUnitKind::Schedule, "s1"), unit(LiveUnitKind::Schedule, "s2")]),
            "↓ 2 scheduled"
        );
        assert_eq!(
            rail_hint(&[unit(LiveUnitKind::Shell, "bg_1"), unit(LiveUnitKind::Schedule, "s1")]),
            "↓ 1 shell running · 1 scheduled"
        );
    }

    #[test]
    fn every_rail_row_is_exactly_one_screen_row() {
        let mut shell = unit(LiveUnitKind::Shell, "bg_1");
        shell.title = "build".into();
        // A multi-line command must not become two rows.
        shell.detail = Some("for f in *; do\n  echo $f\ndone".into());
        let mut agent = unit(LiveUnitKind::Subagent, "a1");
        agent.title = "review the parser".into();
        agent.tokens = Some(12_400);
        agent.cost_usd = Some(0.42);
        let rows = rail_rows(&[shell, agent], Some(1), 80, None);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let plain = strip_ansi(row);
            assert!(!plain.contains('\n'), "a rail row painted two rows: {plain:?}");
            assert_eq!(width(row), 80, "row not padded to the full width: {plain:?}");
        }
    }

    #[test]
    fn the_selected_row_carries_the_verbs_and_the_armed_row_says_what_x_destroys() {
        let mut shell = unit(LiveUnitKind::Shell, "bg_1");
        shell.title = "build".into();
        let selected = strip_ansi(&rail_rows(std::slice::from_ref(&shell), Some(0), 90, None)[0]);
        assert!(selected.starts_with("❯ "), "{selected}");
        assert!(selected.contains("⏎ open · x stop · esc composer"), "{selected}");
        let armed =
            strip_ansi(&rail_rows(std::slice::from_ref(&shell), Some(0), 90, Some("bg_1"))[0]);
        assert!(armed.contains("x again stops it · esc cancels"), "{armed}");

        // A schedule cannot be opened and is not killed — the hint must not
        // promise what ⏎ cannot do.
        let mut schedule = unit(LiveUnitKind::Schedule, "s1");
        schedule.title = "nightly bench".into();
        let row = strip_ansi(&rail_rows(std::slice::from_ref(&schedule), Some(0), 90, None)[0]);
        assert!(row.contains("⏎ details · x disable · esc composer"), "{row}");
        let armed =
            strip_ansi(&rail_rows(std::slice::from_ref(&schedule), Some(0), 90, Some("s1"))[0]);
        assert!(armed.contains("x again disables it · esc cancels"), "{armed}");
    }

    #[test]
    fn with_the_composer_focused_the_last_row_is_the_counting_hint() {
        let units = [unit(LiveUnitKind::Shell, "bg_1"), unit(LiveUnitKind::Subagent, "a")];
        let rows = rail_rows(&units, None, 80, None);
        assert_eq!(rows.len(), 3);
        assert!(strip_ansi(&rows[2]).trim_end().ends_with("↓ 1 shell · 1 agent running"));
        // …and no row carries a cursor.
        assert!(!rows.iter().any(|r| strip_ansi(r).starts_with('❯')));
    }

    #[test]
    fn a_schedule_counts_down_and_says_due_rather_than_reading_as_running() {
        let mut s = unit(LiveUnitKind::Schedule, "s1");
        s.title = "nightly bench".into();
        s.elapsed_ms = 65_000;
        assert!(strip_ansi(&unit_line(&s, 80)).contains("in 1m05s"));
        s.elapsed_ms = -4_000;
        assert!(strip_ansi(&unit_line(&s, 80)).contains("due"));
    }

    #[test]
    fn a_detail_that_repeats_the_name_is_dropped_rather_than_printed_twice() {
        let mut shell = unit(LiveUnitKind::Shell, "bg_1");
        shell.title = "sleep 120".into();
        shell.detail = Some("sleep 120".into());
        let row = strip_ansi(&unit_line(&shell, 80));
        assert_eq!(row.matches("sleep 120").count(), 1, "{row}");
    }
}
