//! The recap tab: what actually happened in this conversation, in order.
//!
//! THE QUESTION THIS ANSWERS: "what is this session, and where did it get to."
//! Scrolling the transcript answers it eventually — after paging through every
//! tool output, every retry and every wall of prose.
//!
//! THE ROUND IS THE UNIT, and that is the whole design. One line per STEP was
//! the obvious first cut and it was wrong: a real session runs hundreds of
//! steps, so a step-per-line recap is a shorter transcript rather than a
//! summary, and it needs scrolling to read — which is the thing being fixed.
//! A round is what a person actually remembers ("I asked for the rail, it took
//! four minutes, it fought the timezone"), so a round is two rows: what you
//! asked, and what came of it.
//!
//! IT IS DERIVED, NOT WRITTEN. Every figure comes from parts already in the
//! thread: a program's own summary line, a result's error flag, a message's
//! timestamp. Nothing here calls a model, so it is instant, free, and cannot
//! narrate work that did not happen — which is the failure mode of a generated
//! recap, and the reason a generated one cannot be trusted as a record.
//!
//! FAILURES SURVIVE THE COLLAPSE. A step that errored is what a summary drops
//! first and what you most need on a re-read. Rolling steps up to a round must
//! not lose them, so the count rides the settle line and a round that ended
//! badly says what it was doing when it stopped.
//!
//! TIME IS RELATIVE TO THE FIRST MESSAGE, never a wall clock. A recap is read
//! for shape — how long a round took, where the session stalled — and an
//! elapsed column answers that without dragging a timezone into a renderer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use bough_core::schema::parts::{Message, Part, Role};

use crate::components::panel::paint_rows;
use crate::components::{accent, error, info};
use crate::store::selectors::clip;

/// What the tab needs. Borrowed, like every other tab's props.
pub struct RecapProps<'a> {
    pub thread: &'a [Message],
    /// Rows scrolled off the TOP. The tail is what a recap is opened for, so
    /// the default view is pinned to the end and this counts backwards from it.
    pub scroll: usize,
    pub height: usize,
    pub cols: usize,
}

/// One round: an ask and everything that answered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    /// Milliseconds from the first message in the thread.
    pub at: i64,
    /// What was asked, in its first line.
    pub ask: String,
    pub elapsed_ms: i64,
    pub steps: usize,
    pub failed: usize,
    /// Whether the round GOT THERE — the last step's outcome, not
    /// `failed == 0`. A round that hit a wall, backed out and landed the fix
    /// did not fail; marking it `✗` because something went wrong on the way
    /// makes every interesting round look broken, and then the mark means
    /// nothing. `failed` is where the bumps are recorded.
    pub arrived: bool,
    /// What the round touched, rolled up from its steps and deduped — the
    /// substance of the round in one phrase.
    pub gist: String,
}

/// `m:ss`, or `h:mm:ss` past an hour. Relative, so no timezone is involved.
fn stamp(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

/// The first non-empty line of some prose, which is the sentence a human wrote
/// or the agent led with. The rest is what the transcript is for.
fn headline(text: &str) -> String {
    text.trim()
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// All `Text` parts of a message, joined — the prose, without the tool traffic.
fn prose(msg: &Message) -> String {
    msg.parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What ONE program did, in a phrase.
///
/// `program_summary` is the tool-group header the transcript already paints
/// ("read rail.rs · wrote schedules.rs"), so a round and the groups it rolled
/// up cannot describe the same work differently. It gives nothing back for a
/// program that touched no files, and a bare first line of code beats nothing
/// there.
fn step_text(input: &Value, width: usize) -> String {
    let code = input.get("code").and_then(Value::as_str).unwrap_or("");
    let summary = crate::lines::program_summary(code, width, false);
    if !summary.trim().is_empty() {
        return summary;
    }
    let gist = crate::lines::code_gist(input, width);
    if gist.trim().is_empty() {
        String::new()
    } else {
        gist
    }
}

/// A round being accumulated.
struct Open {
    at: i64,
    started: i64,
    ask: String,
    steps: usize,
    failed: usize,
    arrived: bool,
    /// Step phrases in order of first appearance. Deduped, because a round
    /// that wrote the same file four times says "wrote rail.rs" once — the
    /// repetition is already carried by the step count.
    touched: Vec<String>,
    /// What the round was doing when its last step failed. Only rendered when
    /// the round did NOT arrive: on a round that recovered it is history, and
    /// on one that stopped it is the single most useful thing on the row.
    stopped_on: Option<String>,
}

/// The timeline, one entry per round. Pure — the whole tab is a function of
/// this list, so every rule above is testable without a terminal.
pub fn rounds(thread: &[Message], width: usize) -> Vec<Round> {
    let Some(origin) = thread.first().map(|m| m.created_at) else {
        return Vec::new();
    };
    let mut out: Vec<Round> = Vec::new();
    let mut open: Option<Open> = None;

    fn settle(out: &mut Vec<Round>, open: &mut Option<Open>, at: i64, width: usize) {
        let Some(o) = open.take() else { return };
        // A round with no steps is a question, not a round — one still being
        // answered, or one the agent replied to in prose alone.
        if o.steps == 0 {
            return;
        }
        let mut gist = o.touched.join(" · ");
        if !o.arrived {
            if let Some(stopped) = &o.stopped_on {
                gist = format!("stopped on {stopped}");
            }
        }
        out.push(Round {
            at: o.at,
            ask: o.ask,
            elapsed_ms: at - o.started,
            steps: o.steps,
            failed: o.failed,
            arrived: o.arrived,
            gist: clip(&gist, width),
        });
    }

    for msg in thread {
        if msg.role == Role::User {
            settle(&mut out, &mut open, msg.created_at, width);
            let ask = headline(&prose(msg));
            // An ask with no words — an image, a bare resume — still opens a
            // round; the round's own figures are the record either way.
            open = Some(Open {
                at: msg.created_at - origin,
                started: msg.created_at,
                ask: clip(&ask, width),
                steps: 0,
                failed: 0,
                arrived: true,
                touched: Vec::new(),
                stopped_on: None,
            });
            continue;
        }
        let Some(o) = open.as_mut() else { continue };

        // Which calls failed — a result names its call, so the two are joined
        // by id rather than by position.
        let refs: Vec<&Part> = msg.parts.iter().collect();
        let summary = crate::lines::tool_summary(&refs);
        for call in &summary.calls {
            let Part::ToolCall { id, input, .. } = call else {
                continue;
            };
            let failed = matches!(
                summary.results.get(id.as_str()),
                Some(Part::ToolResult { is_error: true, .. })
            );
            let text = step_text(input, width);
            o.steps += 1;
            o.failed += usize::from(failed);
            o.arrived = !failed;
            if failed {
                // The CODE, not the summary. `program_summary` describes a
                // step by the files it touched, which for a step that only ran
                // a command is the near-useless "ran 1 command" — and "stopped
                // on ran 1 command" is neither grammar nor information. The
                // first line of the program names the command that failed.
                let gist = crate::lines::code_gist(input, width);
                o.stopped_on = Some(if gist.trim().is_empty() {
                    text.clone()
                } else {
                    gist
                });
            }
            if !text.is_empty() && !o.touched.contains(&text) {
                o.touched.push(text);
            }
        }
    }
    // The last round settles at the last thing that happened, not at "now": a
    // recap re-read tomorrow must not claim the round took a day.
    let last = thread.last().map(|m| m.created_at).unwrap_or(origin);
    settle(&mut out, &mut open, last, width);
    out
}

/// The painted rows. Split from `render` so the shape of the tab is assertable
/// as text.
pub fn recap_lines(thread: &[Message], cols: usize) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    // 7 columns of gutter: the elapsed stamp, then the rail and its mark.
    let width = cols.saturating_sub(12).max(20);
    let rounds = rounds(thread, width);
    if rounds.is_empty() {
        return vec![Line::from(Span::styled(
            "nothing has happened here yet — this fills in as the conversation runs".to_string(),
            dim,
        ))];
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    for (i, r) in rounds.iter().enumerate() {
        if i > 0 {
            rows.push(Line::from(""));
        }
        let (mark, mark_style) = if r.arrived {
            ("●", Style::default().fg(accent()))
        } else {
            ("✗", Style::default().fg(error()))
        };
        rows.push(Line::from(vec![
            Span::styled(format!("{:>6}", stamp(r.at)), dim),
            Span::styled(format!(" {mark} "), mark_style),
            Span::styled(r.ask.clone(), bold),
        ]));

        let mut settle = format!(
            "{} · {} step{}",
            crate::format::fmt_duration(r.elapsed_ms),
            r.steps,
            if r.steps == 1 { "" } else { "s" }
        );
        if r.failed > 0 {
            settle.push_str(&format!(" · {} failed", r.failed));
        }
        let mut spans = vec![
            Span::styled(" ".repeat(6), dim),
            Span::styled(" ╰ ".to_string(), Style::default().fg(accent())),
            Span::styled(
                settle,
                if r.failed > 0 {
                    Style::default().fg(error())
                } else {
                    Style::default().fg(info())
                },
            ),
        ];
        if !r.gist.is_empty() {
            spans.push(Span::styled(format!(" · {}", r.gist), dim));
        }
        rows.push(Line::from(spans));
    }
    rows
}

/// Render the tab. Pinned to the TAIL — a recap is opened to see where the
/// session got to, so the newest round is on screen without scrolling, and
/// `scroll` walks backwards from there.
pub fn render(props: &RecapProps, area: Rect, buf: &mut Buffer) {
    let rows = recap_lines(props.thread, props.cols);
    let height = props.height.max(1);
    let end = rows.len().saturating_sub(props.scroll);
    let start = end.saturating_sub(height);
    let mut window: Vec<Line<'static>> = Vec::new();
    // Never silently truncate: a recap that quietly drops the first two hours
    // reads as a session that started two hours late.
    if start > 0 {
        window.push(Line::from(Span::styled(
            format!(
                "… {start} earlier row{} — scroll up",
                if start == 1 { "" } else { "s" }
            ),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    window.extend(rows[start..end].iter().cloned());
    paint_rows(&window, area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(role: Role, at: i64, parts: Vec<Part>) -> Message {
        Message {
            id: format!("m{at}"),
            session_id: "s1".into(),
            role,
            parts,
            pending: false,
            created_at: at,
        }
    }

    fn user(at: i64, text: &str) -> Message {
        msg(
            Role::User,
            at,
            vec![Part::Text {
                text: text.to_string(),
            }],
        )
    }

    fn call(id: &str, code: &str) -> Part {
        Part::ToolCall {
            id: id.into(),
            name: "run_steps".into(),
            input: json!({ "code": code }),
        }
    }

    fn result(id: &str, is_error: bool) -> Part {
        Part::ToolResult {
            call_id: id.into(),
            output: json!("out"),
            is_error,
            interrupted: None,
        }
    }

    fn painted(thread: &[Message], cols: usize) -> Vec<String> {
        recap_lines(thread, cols)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn an_empty_thread_is_an_absence_not_a_timeline() {
        assert!(rounds(&[], 60).is_empty());
        assert_eq!(
            recap_lines(&[], 80).len(),
            1,
            "one honest line, not an empty panel"
        );
    }

    #[test]
    fn a_round_is_two_rows_however_many_steps_it_ran() {
        // THE POINT OF THE TAB. Twenty steps must not become twenty rows, or
        // the recap is a shorter transcript rather than a summary.
        let mut thread = vec![user(0, "make the rail show scheduled runs")];
        for i in 1..=20 {
            thread.push(msg(
                Role::Supervisor,
                i * 1_000,
                vec![
                    call(&format!("c{i}"), "view('src/rail.rs')"),
                    result(&format!("c{i}"), false),
                ],
            ));
        }
        let rows = painted(&thread, 80);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert!(rows[0].contains("make the rail show scheduled runs"));
        assert!(rows[1].contains("20 steps"), "{:?}", rows[1]);
    }

    #[test]
    fn what_a_round_touched_is_deduped_because_the_count_carries_the_repetition() {
        let thread = vec![
            user(0, "fix the rail"),
            msg(
                Role::Supervisor,
                1_000,
                vec![
                    call("c1", "write('src/rail.rs', a)"),
                    result("c1", false),
                    call("c2", "write('src/rail.rs', b)"),
                    result("c2", false),
                    call("c3", "view('src/schedules.rs')"),
                    result("c3", false),
                ],
            ),
        ];
        let gist = &rounds(&thread, 200)[0].gist;
        assert_eq!(
            gist.matches("wrote rail.rs").count(),
            1,
            "written twice, said once: {gist:?}"
        );
        assert!(gist.contains("schedules.rs"), "{gist:?}");
    }

    #[test]
    fn failures_survive_the_collapse_to_one_round() {
        // The rollup must not lose the beats a summary drops first.
        let thread = vec![
            user(0, "fix the flake"),
            msg(
                Role::Supervisor,
                1_000,
                vec![
                    call("c1", "await bash('cargo test', ['test'])"),
                    result("c1", true),
                    call("c2", "write('src/rail.rs', body)"),
                    result("c2", false),
                ],
            ),
        ];
        let r = &rounds(&thread, 200)[0];
        assert_eq!((r.steps, r.failed), (2, 1));
        assert!(painted(&thread, 80)[1].contains("1 failed"));
    }

    #[test]
    fn a_round_that_recovered_is_not_a_round_that_failed() {
        // The mark answers "did this get there", the count answers "was it
        // bumpy". Conflating them makes every interesting round read as broken.
        let recovered = vec![
            user(0, "fix it"),
            msg(
                Role::Supervisor,
                1_000,
                vec![
                    call("c1", "await bash('cargo test', ['test'])"),
                    result("c1", true),
                    call("c2", "write('src/a.rs', body)"),
                    result("c2", false),
                ],
            ),
        ];
        let r = &rounds(&recovered, 200)[0];
        assert!(r.arrived && r.failed == 1);
        assert!(
            !r.gist.contains("stopped on"),
            "a recovered round reports what it touched, not where it tripped"
        );

        let stuck = vec![
            user(0, "fix it"),
            msg(
                Role::Supervisor,
                1_000,
                vec![
                    call("c1", "write('src/a.rs', body)"),
                    result("c1", false),
                    call("c2", "await bash('cargo test', ['test'])"),
                    result("c2", true),
                ],
            ),
        ];
        let r = &rounds(&stuck, 200)[0];
        assert!(!r.arrived);
        assert!(
            r.gist.starts_with("stopped on") && r.gist.contains("cargo test"),
            "a round that stopped names the COMMAND that failed, not \"ran 1 \
             command\": {:?}",
            r.gist
        );
        assert!(painted(&stuck, 80)[0].contains('✗'));
    }

    #[test]
    fn each_ask_settles_the_round_before_it() {
        let thread = vec![
            user(0, "first ask"),
            msg(
                Role::Supervisor,
                1_000,
                vec![call("c1", "view('a.rs')"), result("c1", false)],
            ),
            user(5_000, "second ask"),
            msg(
                Role::Supervisor,
                6_000,
                vec![call("c2", "view('b.rs')"), result("c2", false)],
            ),
        ];
        let rs = rounds(&thread, 200);
        assert_eq!(rs.len(), 2);
        assert_eq!(
            rs[0].elapsed_ms, 5_000,
            "measured to the SECOND ask, not to the end of the thread"
        );
        // The last round settles at the last message, so a recap re-read
        // tomorrow does not report that the round took a day.
        assert_eq!(rs[1].elapsed_ms, 1_000);
    }

    #[test]
    fn an_ask_still_being_answered_is_not_yet_a_round() {
        assert!(rounds(&[user(0, "do the thing")], 60).is_empty());
    }

    #[test]
    fn the_stamp_is_relative_and_grows_an_hours_column_when_it_needs_one() {
        assert_eq!(stamp(0), "0:00");
        assert_eq!(stamp(9_000), "0:09");
        assert_eq!(stamp(252_000), "4:12");
        assert_eq!(stamp(3_723_000), "1:02:03");
    }

    #[test]
    fn scrolled_past_rows_are_announced_rather_than_dropped() {
        let mut thread = Vec::new();
        for i in 0..20 {
            thread.push(user(i * 10_000, &format!("ask {i}")));
            thread.push(msg(
                Role::Supervisor,
                i * 10_000 + 1_000,
                vec![
                    call(&format!("c{i}"), "view('a.rs')"),
                    result(&format!("c{i}"), false),
                ],
            ));
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 6));
        render(
            &RecapProps {
                thread: &thread,
                scroll: 0,
                height: 6,
                cols: 80,
            },
            Rect::new(0, 0, 80, 6),
            &mut buf,
        );
        let top: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(top.contains("earlier row"), "{top:?}");
    }
}
