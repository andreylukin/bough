//! The recap tab: what actually happened in this conversation, in order.
//!
//! THE QUESTION THIS ANSWERS: "what is this session, and where did it get to."
//! Scrolling the transcript answers it eventually — after paging through every
//! tool output, every retry and every wall of prose. The recap is the same run
//! at one line per event, so a session that took three hours reads in fifteen
//! seconds.
//!
//! IT IS DERIVED, NOT WRITTEN. Every beat comes from the parts already in the
//! thread: a program's own summary line, a result's error flag, a message's
//! timestamp. Nothing here calls a model, so it is instant, free, and cannot
//! narrate work that did not happen — which is the failure mode of a generated
//! recap, and the reason a generated one cannot be trusted as a record.
//!
//! THE FAILURES ARE THE POINT. `✗` beats — a step that errored, an approach
//! that was retried — are the ones a summary drops first and the ones you most
//! need on a re-read: they are what stops the same wall being walked into
//! twice. They keep their own mark and their own colour, and the round footer
//! counts them.
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

/// One entry on the timeline. Deliberately a small closed set: a recap that can
/// say anything says nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum Beat {
    /// What you asked for. Opens a round.
    Ask { at: i64, text: String },
    /// One program the agent ran, headlined by what it DID.
    Step { at: i64, text: String, failed: bool },
    /// What the agent concluded, in its own first sentence.
    Said { at: i64, text: String },
    /// The round's settle: how long, how many steps, how many failed, and
    /// whether it GOT THERE.
    ///
    /// `arrived` is the last step's outcome, not `failed == 0`. A round that
    /// hit a wall, backed out and landed the fix did not fail — marking it `✗`
    /// because something went wrong on the way makes every interesting round
    /// look like a failure, and then the mark means nothing. The `failed` count
    /// is where the bumps are recorded.
    Round {
        at: i64,
        elapsed_ms: i64,
        steps: usize,
        failed: usize,
        arrived: bool,
    },
}

impl Beat {
    pub fn at(&self) -> i64 {
        match self {
            Beat::Ask { at, .. }
            | Beat::Step { at, .. }
            | Beat::Said { at, .. }
            | Beat::Round { at, .. } => *at,
        }
    }
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

/// What ONE program did, in a line.
///
/// `program_summary` is the tool-group header the transcript already paints
/// ("read rail.rs · wrote schedule.rs"), so a beat and the group it came from
/// cannot describe the same step differently. It gives nothing back for a
/// program that touched no files, and a bare first line of code beats an empty
/// row there.
fn step_text(input: &Value, width: usize) -> String {
    let code = input.get("code").and_then(Value::as_str).unwrap_or("");
    let summary = crate::lines::program_summary(code, width, false);
    if !summary.trim().is_empty() {
        return summary;
    }
    let gist = crate::lines::code_gist(input, width);
    if gist.trim().is_empty() {
        "ran a step".to_string()
    } else {
        gist
    }
}

/// The timeline, derived from the thread. Pure — the whole tab is a function
/// of this list, so every rule above is testable without a terminal.
pub fn beats(thread: &[Message], width: usize) -> Vec<Beat> {
    let Some(origin) = thread.first().map(|m| m.created_at) else {
        return Vec::new();
    };
    let mut out: Vec<Beat> = Vec::new();
    // The round in flight: when it opened, and what it has done.
    // started_at, steps, failed, and whether the LAST step landed.
    let mut open: Option<(i64, usize, usize, bool)> = None;

    // Close the round in flight, if any, at `at`.
    fn settle(
        out: &mut Vec<Beat>,
        open: &mut Option<(i64, usize, usize, bool)>,
        at: i64,
        origin: i64,
    ) {
        if let Some((started, steps, failed, arrived)) = open.take() {
            // A round with nothing in it is a message, not a round — an ask
            // still being answered, or one the agent replied to in prose alone.
            if steps > 0 {
                out.push(Beat::Round {
                    at: at - origin,
                    elapsed_ms: at - started,
                    steps,
                    failed,
                    arrived,
                });
            }
        }
    }

    for msg in thread {
        let at = msg.created_at - origin;
        if msg.role == Role::User {
            settle(&mut out, &mut open, msg.created_at, origin);
            let text = headline(&prose(msg));
            if !text.is_empty() {
                out.push(Beat::Ask {
                    at,
                    text: clip(&text, width),
                });
            }
            open = Some((msg.created_at, 0, 0, true));
            continue;
        }

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
            out.push(Beat::Step {
                at,
                text: step_text(input, width),
                failed,
            });
            if let Some((_, steps, failures, arrived)) = open.as_mut() {
                *steps += 1;
                *failures += usize::from(failed);
                *arrived = !failed;
            }
        }

        let said = headline(&prose(msg));
        if !said.is_empty() {
            out.push(Beat::Said {
                at,
                text: clip(&said, width),
            });
        }
    }
    // The last round is settled at the last thing that happened, not at "now":
    // a recap re-read tomorrow must not claim the round took a day.
    let last = thread.last().map(|m| m.created_at).unwrap_or(origin);
    settle(&mut out, &mut open, last, origin);
    out
}

/// The painted rows for a timeline. Split from `render` so the shape of the
/// tab is assertable as text.
pub fn recap_lines(thread: &[Message], cols: usize) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    // 7 columns of gutter: `m:ss` right-aligned, then the rail and its mark.
    let text_width = cols.saturating_sub(12).max(20);
    let beats = beats(thread, text_width);
    if beats.is_empty() {
        return vec![Line::from(Span::styled(
            "nothing has happened here yet — this fills in as the conversation runs".to_string(),
            dim,
        ))];
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    for beat in &beats {
        let time = match beat {
            // Only the round's ENDS carry a time. A stamp on every row is a
            // column of near-identical numbers that hides the two that matter.
            Beat::Ask { .. } | Beat::Round { .. } => format!("{:>6}", stamp(beat.at())),
            _ => " ".repeat(6),
        };
        let (rail, mark, text, style) = match beat {
            Beat::Ask { text, .. } => ("┌", "●", text.clone(), bold),
            Beat::Step { text, failed, .. } => (
                "│",
                if *failed { "✗" } else { "◆" },
                text.clone(),
                if *failed {
                    Style::default().fg(error())
                } else {
                    Style::default()
                },
            ),
            Beat::Said { text, .. } => ("│", "·", text.clone(), dim),
            Beat::Round {
                elapsed_ms,
                steps,
                failed,
                arrived,
                ..
            } => {
                let mut s = format!(
                    "{} · {steps} step{}",
                    crate::format::fmt_duration(*elapsed_ms),
                    if *steps == 1 { "" } else { "s" }
                );
                if *failed > 0 {
                    s.push_str(&format!(" · {failed} failed"));
                }
                (
                    "└",
                    if *arrived { "✓" } else { "✗" },
                    s,
                    if *arrived {
                        Style::default().fg(info())
                    } else {
                        Style::default().fg(error())
                    },
                )
            }
        };
        rows.push(Line::from(vec![
            Span::styled(time, dim),
            Span::styled(format!(" {rail}"), Style::default().fg(accent())),
            Span::styled(format!("{mark} "), style),
            Span::styled(text, style),
        ]));
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

    #[test]
    fn an_empty_thread_is_an_absence_not_a_timeline() {
        assert!(beats(&[], 60).is_empty());
        let rows = recap_lines(&[], 80);
        assert_eq!(rows.len(), 1, "one honest line, not an empty panel");
    }

    #[test]
    fn a_round_is_the_ask_its_steps_and_a_settle() {
        let thread = vec![
            user(1_000, "make the rail show scheduled runs"),
            msg(
                Role::Supervisor,
                4_000,
                vec![
                    call("c1", "const s = view('src/rail.rs')"),
                    result("c1", false),
                ],
            ),
            msg(
                Role::Supervisor,
                9_000,
                vec![Part::Text {
                    text: "the countdown renders from RailCtx now".into(),
                }],
            ),
        ];
        let beats = beats(&thread, 60);
        assert!(
            matches!(&beats[0], Beat::Ask { at: 0, text } if text.starts_with("make the rail"))
        );
        assert!(matches!(&beats[1], Beat::Step { failed: false, .. }));
        assert!(matches!(&beats[2], Beat::Said { .. }));
        // Settled at the LAST message, not at a wall clock — a recap re-read
        // tomorrow must not report that the round took a day.
        assert_eq!(
            beats[3],
            Beat::Round {
                at: 8_000,
                elapsed_ms: 8_000,
                steps: 1,
                failed: 0,
                arrived: true,
            }
        );
    }

    #[test]
    fn a_failed_step_keeps_its_mark_and_is_counted_in_the_settle() {
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
        let beats = beats(&thread, 60);
        let failed: Vec<&Beat> = beats
            .iter()
            .filter(|b| matches!(b, Beat::Step { failed: true, .. }))
            .collect();
        assert_eq!(failed.len(), 1, "the error flag survives into the timeline");
        assert_eq!(
            beats.last(),
            Some(&Beat::Round {
                at: 1_000,
                elapsed_ms: 1_000,
                steps: 2,
                failed: 1,
                // The failing step was not the LAST one — the round recovered,
                // and a round that recovered is not a round that failed.
                arrived: true,
            })
        );
        // The failure reaches the painted row, which is the whole point of
        // keeping it: this is the beat a generated summary drops first.
        let painted = recap_lines(&thread, 80)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(painted.iter().any(|r| r.contains('✗')), "{painted:?}");
        assert!(
            painted.iter().any(|r| r.contains("1 failed")),
            "{painted:?}"
        );
    }

    #[test]
    fn a_second_ask_settles_the_round_before_it() {
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
        let all = beats(&thread, 60);
        let settles: Vec<&Beat> = all
            .iter()
            .filter(|b| matches!(b, Beat::Round { .. }))
            .collect();
        assert_eq!(settles.len(), 2, "one settle per ask, not one at the end");
        assert!(
            matches!(
                settles[0],
                Beat::Round {
                    elapsed_ms: 5_000,
                    ..
                }
            ),
            "the first round is measured to the SECOND ask, not to the end"
        );
    }

    #[test]
    fn an_ask_still_being_answered_has_no_settle_to_report() {
        let thread = vec![user(0, "do the thing")];
        assert_eq!(
            beats(&thread, 60),
            vec![Beat::Ask {
                at: 0,
                text: "do the thing".into()
            }],
            "a round with no steps is a question, not a round"
        );
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
        assert!(matches!(
            beats(&recovered, 60).last(),
            Some(Beat::Round {
                arrived: true,
                failed: 1,
                ..
            })
        ));

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
        assert!(matches!(
            beats(&stuck, 60).last(),
            Some(Beat::Round {
                arrived: false,
                failed: 1,
                ..
            })
        ));
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
        let mut thread = vec![user(0, "start")];
        for i in 1..40 {
            thread.push(msg(
                Role::Supervisor,
                i * 1_000,
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
        let top: String = (0..80)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect::<String>();
        assert!(top.contains("earlier row"), "{top:?}");
    }
}
