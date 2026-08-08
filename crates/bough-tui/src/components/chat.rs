//! The chat view (port of `src/tui/components/Chat.tsx`, wave-1 subset): a
//! virtualized window over the pre-wrapped transcript, plus the facts that
//! must never scroll away — what the agent is doing right now.
//!
//! THE INVARIANT THIS HOLDS: **presentational only.** Every value shown is a
//! prop; the caller builds the transcript rows once and shares them with the
//! hit-test (spec: "the transcript is built once").
//!
//! SECOND INVARIANT — **the transcript hangs from the bottom.** A short
//! conversation is padded above, not below.
//!
//! The scroll-indicator row and the activity/busy strip are RESERVED
//! unconditionally — appearing/vanishing rows made the transcript jump a row
//! at turn start/end. Queued rows and the notice row are counted before the
//! transcript takes what is left (`chat_body_height`).
//!
//! THIRD INVARIANT — **the rows arrive STYLED and are parsed, never painted
//! raw.** `lines::build_lines` bakes SGR sequences into every row (that is how
//! the palette reaches the transcript at all); this component runs each one
//! through `ansi::line_from_ansi`, so escapes become ratatui spans instead of
//! printing as `[0m` litter. There is exactly ONE geometry: the window math
//! lives in `lines.rs` and both this renderer and the mouse hit-test call it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::lines::{chat_body_height, visible_slice, VLine};

use super::{accent, display_width, fmt_duration, fmt_tokens, info, pad_row, warn, SPINNER};

/// Default empty-transcript placeholder (Chat.tsx).
pub const CHAT_PLACEHOLDER: &str = "type to start · the agent writes one program per round";

/// The second line of the splash: the two keys that reach everything else. A
/// first screen that names no key leaves the whole surface undiscoverable.
pub const CHAT_HINT: &str = "? help · / commands · ^t panel";

pub struct ChatProps<'a> {
    /// The whole transcript, pre-wrapped and pre-STYLED: one entry per
    /// physical row, straight out of `lines::build_lines`.
    pub lines: &'a [VLine],
    pub width: u16,
    /// Rows this component may occupy IN TOTAL (body + reserved strips).
    pub height: u16,
    /// Lines up from the live tail. 0 = pinned to the bottom, following output.
    pub scroll_off: usize,
    /// The cheap-tier activity blurb. Absent is the normal case (wave 1: always None).
    pub activity: Option<&'a str>,
    /// The shell command the turn is blocked on right now. Independent of
    /// `activity`: either, both or neither may be present.
    pub activity_command: Option<&'a str>,
    /// A turn is in flight. Drives the spinner line.
    pub busy: bool,
    pub elapsed_ms: i64,
    /// Tokens accrued so far in the running turn; absent degrades the busy line.
    pub turn_tokens: Option<i64>,
    /// Spinner phase. The caller owns the clock so this stays render-pure.
    pub tick: u64,
    /// Messages typed while a turn ran, held locally until it drains.
    pub queued: &'a [String],
    /// A transient message (a copy, an error).
    pub notice: Option<&'a str>,
    /// Shown instead of the transcript when the session has no messages yet.
    pub placeholder: &'a str,
}

/// A styled transcript row as a ratatui line, padded to the viewport width so
/// it clears whatever the previous frame left in those cells. THE ESCAPES DIE
/// HERE — nothing downstream of this function has a `\x1b` in it.
fn styled_row(text: &str, width: usize) -> Line<'static> {
    let mut line = crate::ansi::line_from_ansi(text);
    let painted: usize = line
        .spans
        .iter()
        .map(|s| display_width(&s.content))
        .sum::<usize>();
    if painted < width {
        line.spans.push(Span::raw(" ".repeat(width - painted)));
    }
    line
}

/// The line shown while a turn is running: motion, elapsed time, and the way
/// out, always (format.ts::busyLine, verbatim wording).
pub(crate) fn busy_line(
    activity: Option<&str>,
    command: Option<&str>,
    elapsed_ms: i64,
    tick: u64,
    tokens: Option<i64>,
) -> String {
    let frame = SPINNER[(tick as usize) % SPINNER.len()];
    let trimmed = activity.map(str::trim).unwrap_or("");
    let command = command.map(str::trim).unwrap_or("");
    // The command is the more SPECIFIC fact, so when there is no blurb it
    // carries the line rather than sitting behind "working".
    let what = if !trimmed.is_empty() {
        trimmed
    } else if command.is_empty() {
        "working"
    } else {
        "running a command"
    };
    let mut bits: Vec<String> = vec![what.to_string()];
    if !command.is_empty() {
        bits.push(format!("$ {command}"));
    }
    bits.push(fmt_duration(elapsed_ms));
    if let Some(t) = tokens {
        if t > 0 {
            bits.push(format!("{} tok", fmt_tokens(t)));
        }
    }
    bits.push("esc interrupts".to_string());
    format!("{frame} {}", bits.join(" · "))
}

pub fn render_chat(p: &ChatProps, area: Rect, buf: &mut Buffer) {
    let width = p.width as usize;
    let dim = Style::default().add_modifier(Modifier::DIM);
    let body = chat_body_height(p.height as usize, p.queued.len(), p.notice.is_some());
    let slice = visible_slice(p.lines, body, p.scroll_off);
    let shown = slice.rows.len();
    // Pad above, never below: the newest line stays where the eye already is.
    let pad = body.saturating_sub(shown);

    let mut y = area.y;
    let put = |y: u16, line: Line, buf: &mut Buffer| {
        if y < area.y + area.height {
            buf.set_line(area.x, y, &line, area.width);
        }
    };

    // The empty session is the FIRST screen a user ever sees, so it carries the
    // mark, what the tool does and the keys that open the rest. It degrades to
    // the bare one-line placeholder when the body cannot hold that.
    let splash = if p.lines.is_empty() {
        super::splash::splash_block(width, body, p.placeholder, CHAT_HINT)
    } else {
        None
    };

    for i in 0..body {
        let line = if let Some(rows) = splash.as_ref() {
            rows[i].clone()
        } else if p.lines.is_empty() {
            // The empty-transcript hint sits on the last slot, where the first
            // reply will land.
            if i == body - 1 {
                Line::from(Span::styled(pad_row(p.placeholder, width), dim))
            } else {
                Line::from(Span::raw(pad_row(" ", width)))
            }
        } else if i >= pad {
            styled_row(&slice.rows[i - pad].text, width)
        } else {
            Line::from(Span::raw(pad_row(" ", width)))
        };
        put(y, line, buf);
        y += 1;
    }

    // Scroll indicator — reserved whether or not it has anything to say.
    if slice.more > 0 {
        let head = format!("↓ {}", slice.more);
        let tail = format!(
            " more line{} below · {}%",
            if slice.more == 1 { "" } else { "s" },
            slice.pct
        );
        put(
            y,
            Line::from(vec![
                Span::styled(head.clone(), Style::default().fg(info())),
                Span::styled(
                    pad_row(&tail, width.saturating_sub(display_width(&head))),
                    dim,
                ),
            ]),
            buf,
        );
    } else {
        put(y, Line::from(Span::raw(pad_row(" ", width))), buf);
    }
    y += 1;

    for q in p.queued {
        put(
            y,
            Line::from(Span::styled(pad_row(&format!("⧖ queued: {q}"), width), dim)),
            buf,
        );
        y += 1;
    }

    // The busy strip — reserved unconditionally so the transcript never jumps
    // a row at turn start/end.
    if p.busy {
        let text = busy_line(
            p.activity,
            p.activity_command,
            p.elapsed_ms,
            p.tick,
            p.turn_tokens,
        );
        let mut chars = text.chars();
        let head: String = chars.by_ref().take(2).collect();
        let rest: String = chars.collect();
        put(
            y,
            Line::from(vec![
                Span::styled(head, Style::default().fg(accent())),
                Span::styled(pad_row(&rest, width.saturating_sub(2)), dim),
            ]),
            buf,
        );
    } else if let Some(activity) = p.activity {
        put(
            y,
            Line::from(vec![
                Span::styled("⋯ ", Style::default().fg(accent())),
                Span::styled(pad_row(activity, width.saturating_sub(2)), dim),
            ]),
            buf,
        );
    } else {
        put(y, Line::from(Span::raw(pad_row(" ", width))), buf);
    }
    y += 1;

    if let Some(notice) = p.notice {
        put(
            y,
            Line::from(Span::styled(
                pad_row(notice, width),
                Style::default().fg(warn()),
            )),
            buf,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn draw(p: &ChatProps, cols: u16, rows: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render_chat(p, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rows(texts: &[&str]) -> Vec<VLine> {
        texts
            .iter()
            .map(|t| VLine {
                text: (*t).to_string(),
                ..Default::default()
            })
            .collect()
    }

    fn props<'a>(lines: &'a [VLine]) -> ChatProps<'a> {
        ChatProps {
            lines,
            width: 80,
            height: 20,
            scroll_off: 0,
            activity: None,
            activity_command: None,
            busy: false,
            elapsed_ms: 0,
            turn_tokens: None,
            tick: 0,
            queued: &[],
            notice: None,
            placeholder: CHAT_PLACEHOLDER,
        }
    }

    // Chat.test.tsx: "Chat with an empty thread shows the placeholder, not a blank screen"
    #[test]
    fn empty_thread_shows_the_placeholder_not_a_blank_screen() {
        let frame = draw(
            &ChatProps {
                height: 4,
                ..props(&[])
            },
            80,
            4,
        );
        assert!(frame.contains("one program per round"), "{frame}");
    }

    /// The first screen of a real-sized session carries the mark, the name and
    /// the keys — a blank rectangle with one grey sentence taught a new user
    /// nothing about what was reachable.
    #[test]
    fn a_roomy_empty_thread_shows_the_mark_the_name_and_the_keys() {
        let frame = draw(
            &ChatProps {
                height: 24,
                ..props(&[])
            },
            80,
            24,
        );
        assert!(frame.contains("bough"), "{frame}");
        assert!(frame.contains("one program per round"), "{frame}");
        assert!(frame.contains(CHAT_HINT), "{frame}");
        // The mark itself: the log's flank, centred.
        assert!(frame.contains("====="), "{frame}");
    }

    /// A narrow pane used to truncate the tagline mid-word. It says the same
    /// thing in fewer columns instead.
    #[test]
    fn a_narrow_empty_thread_shortens_the_tagline_instead_of_cutting_it() {
        let frame = draw(
            &ChatProps {
                width: 40,
                height: 10,
                ..props(&[])
            },
            40,
            10,
        );
        assert!(frame.contains("type to start · ? help"), "{frame}");
        assert!(!frame.contains("one program p"), "{frame}");
    }

    /// A short body drops the art rather than painting half a logo.
    #[test]
    fn a_short_empty_thread_drops_the_art_but_keeps_the_words() {
        let frame = draw(
            &ChatProps {
                height: 6,
                ..props(&[])
            },
            80,
            6,
        );
        assert!(!frame.contains("====="), "{frame}");
        assert!(frame.contains("one program per round"), "{frame}");
    }

    /// The splash is for the EMPTY session only — one message and the
    /// transcript owns the body again.
    #[test]
    fn the_splash_vanishes_once_the_transcript_has_a_row() {
        let lines = rows(&["hello"]);
        let frame = draw(
            &ChatProps {
                height: 24,
                ..props(&lines)
            },
            80,
            24,
        );
        assert!(frame.contains("hello"), "{frame}");
        assert!(!frame.contains("====="), "{frame}");
        assert!(!frame.contains(CHAT_HINT), "{frame}");
    }

    // Chat.test.tsx: "Chat renders a transcript and its scroll indicator from fixtures"
    // (this component's half: transcript + queued row + activity strip +
    // scroll indicator; the tool-fold assertions belong to lines.rs).
    #[test]
    fn renders_transcript_queued_and_scroll_indicator() {
        let owned: Vec<String> = (0..10).map(|i| format!("row {i}")).collect();
        let lines = rows(&owned.iter().map(String::as_str).collect::<Vec<_>>());
        let queued = vec!["and fix the lint".to_string()];
        let p = ChatProps {
            activity: Some("running the test suite"),
            activity_command: Some("cargo test"),
            queued: &queued,
            ..props(&lines)
        };
        let frame = draw(&p, 80, 20);
        assert!(frame.contains("running the test suite"), "{frame}");
        assert!(frame.contains("⧖ queued: and fix the lint"), "{frame}");
        assert!(frame.contains("row 9"), "{frame}");

        // Scrolled up, the window says how much is below and where the top sits.
        let scrolled = draw(
            &ChatProps {
                height: 4,
                scroll_off: 2,
                ..props(&lines)
            },
            80,
            4,
        );
        assert!(scrolled.contains("↓ 2 more lines below ·"), "{scrolled}");
    }

    #[test]
    fn busy_line_always_names_the_way_out() {
        assert_eq!(
            busy_line(None, None, 9_000, 0, None),
            "⠋ working · 9s · esc interrupts"
        );
        assert_eq!(
            busy_line(Some("reading files"), None, 64_000, 1, Some(3_200)),
            "⠙ reading files · 1m04s · 3.2k tok · esc interrupts"
        );
        // Zero tokens are omitted, not printed.
        assert_eq!(
            busy_line(None, None, 0, 0, Some(0)),
            "⠋ working · 0s · esc interrupts"
        );
    }

    #[test]
    fn the_busy_line_names_the_command_the_turn_is_blocked_on() {
        // The blurb says what the round is for; the command says what it is
        // waiting on right now. Both, in that order — a 90-second `cargo test`
        // under "running the test suite" is otherwise indistinguishable from a
        // hung turn.
        assert_eq!(
            busy_line(
                Some("running the test suite"),
                Some("cargo test"),
                12_000,
                0,
                None
            ),
            "⠋ running the test suite · $ cargo test · 12s · esc interrupts"
        );
        // No blurb (no cheap tier, or it has not answered yet): the command is
        // the more specific fact, so it carries the line instead of "working".
        assert_eq!(
            busy_line(None, Some("cargo test"), 3_000, 0, None),
            "⠋ running a command · $ cargo test · 3s · esc interrupts"
        );
        // Nothing running in a shell: unchanged from before the feature.
        assert_eq!(
            busy_line(Some("reading files"), None, 0, 0, None),
            "⠋ reading files · 0s · esc interrupts"
        );
        // A blank command is the same as none — never an empty `$ ` field.
        assert_eq!(
            busy_line(None, Some("  "), 0, 0, None),
            "⠋ working · 0s · esc interrupts"
        );
    }

    /// THE REGRESSION THIS COMPONENT EXISTS TO PREVENT: `build_lines` bakes SGR
    /// into every row, and a renderer that paints them raw prints `[0m` on
    /// screen. Nothing styled may survive to the buffer as text.
    #[test]
    fn styled_rows_are_parsed_not_painted_as_escape_litter() {
        let lines = rows(&[
            "\u{1b}[2m# rules: AGENTS.md\u{1b}[22m",
            "\u{1b}[1mbough\u{1b}[22m",
            "  \u{1b}[38;2;255;0;0mred\u{1b}[39m text",
        ]);
        let frame = draw(
            &ChatProps {
                width: 40,
                height: 8,
                ..props(&lines)
            },
            40,
            8,
        );
        assert!(!frame.contains("[0m"), "{frame}");
        assert!(!frame.contains("[2m"), "{frame}");
        assert!(!frame.contains('\u{1b}'), "{frame}");
        assert!(frame.contains("# rules: AGENTS.md"), "{frame}");
        assert!(frame.contains("red text"), "{frame}");
    }

    /// The styling is not merely stripped — it lands as ratatui style.
    #[test]
    fn a_bold_row_paints_bold_cells() {
        let mut term = Terminal::new(TestBackend::new(20, 4)).unwrap();
        let lines = rows(&["\u{1b}[1mbough\u{1b}[22m"]);
        let p = ChatProps {
            width: 20,
            height: 4,
            ..props(&lines)
        };
        term.draw(|f| render_chat(&p, f.area(), f.buffer_mut()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let y = chat_body_height(4, 0, false) as u16 - 1;
        assert_eq!(buf[(0, y)].symbol(), "b");
        assert!(buf[(0, y)].modifier.contains(Modifier::BOLD));
    }
}
