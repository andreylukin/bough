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
//! WAVE-1 NOTE: `chat_body_height`/`visible_slice` belong to `tui/lines.rs`
//! (row 1.37); until that port lands the math lives here, ported verbatim
//! from `src/tui/lines.ts`, so the renderer and the (future) mouse hit-test
//! share one copy inside the crate.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{display_width, fmt_duration, fmt_tokens, pad_row, ACCENT, INFO, SPINNER, WARN};

/// Default empty-transcript placeholder (Chat.tsx).
pub const CHAT_PLACEHOLDER: &str = "type to start · the agent writes one program per round";

pub struct ChatProps<'a> {
    /// The whole transcript, pre-wrapped: one entry per physical row.
    pub lines: &'a [String],
    pub width: u16,
    /// Rows this component may occupy IN TOTAL (body + reserved strips).
    pub height: u16,
    /// Lines up from the live tail. 0 = pinned to the bottom, following output.
    pub scroll_off: usize,
    /// The cheap-tier activity blurb. Absent is the normal case (wave 1: always None).
    pub activity: Option<&'a str>,
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

/// lines.ts::chatBodyHeight — the transcript body after the reserved strips.
pub fn chat_body_height(height: u16, queued: usize, has_notice: bool) -> usize {
    (height as isize - (queued as isize + 2 + isize::from(has_notice))).max(1) as usize
}

pub struct VisibleSlice {
    pub start: usize,
    pub rows: std::ops::Range<usize>,
    pub more: usize,
    pub pct: usize,
}

/// lines.ts::visibleSlice — `scroll_off` counts up from the live tail; `pct`
/// is the viewport TOP's position (fully scrolled up reads 0%).
pub fn visible_slice(len: usize, height: usize, scroll_off: usize) -> VisibleSlice {
    let h = height.max(1);
    let max_off = len.saturating_sub(h);
    let off = scroll_off.min(max_off);
    let start = len.saturating_sub(h + off);
    let end = (start + h).min(len);
    let pct = if max_off == 0 {
        100
    } else {
        ((start as f64 / max_off as f64) * 100.0).round() as usize
    };
    VisibleSlice { start, rows: start..end, more: off, pct }
}

/// The line shown while a turn is running: motion, elapsed time, and the way
/// out, always (format.ts::busyLine, verbatim wording).
pub(crate) fn busy_line(activity: Option<&str>, elapsed_ms: i64, tick: u64, tokens: Option<i64>) -> String {
    let frame = SPINNER[(tick as usize) % SPINNER.len()];
    let trimmed = activity.map(str::trim).unwrap_or("");
    let what = if trimmed.is_empty() { "working" } else { trimmed };
    let mut bits: Vec<String> = vec![what.to_string(), fmt_duration(elapsed_ms)];
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
    let body = chat_body_height(p.height, p.queued.len(), p.notice.is_some());
    let slice = visible_slice(p.lines.len(), body, p.scroll_off);
    let shown = slice.rows.len();
    // Pad above, never below: the newest line stays where the eye already is.
    let pad = body.saturating_sub(shown);

    let mut y = area.y;
    let put = |y: u16, line: Line, buf: &mut Buffer| {
        if y < area.y + area.height {
            buf.set_line(area.x, y, &line, area.width);
        }
    };

    for i in 0..body {
        let line = if p.lines.is_empty() {
            // The empty-transcript hint sits on the last slot, where the first
            // reply will land.
            if i == body - 1 {
                Line::from(Span::styled(pad_row(p.placeholder, width), dim))
            } else {
                Line::from(Span::raw(pad_row(" ", width)))
            }
        } else if i >= pad {
            let text = &p.lines[slice.start + (i - pad)];
            Line::from(Span::raw(pad_row(text, width)))
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
                Span::styled(head.clone(), Style::default().fg(INFO)),
                Span::styled(pad_row(&tail, width.saturating_sub(display_width(&head))), dim),
            ]),
            buf,
        );
    } else {
        put(y, Line::from(Span::raw(pad_row(" ", width))), buf);
    }
    y += 1;

    for q in p.queued {
        put(y, Line::from(Span::styled(pad_row(&format!("⧖ queued: {q}"), width), dim)), buf);
        y += 1;
    }

    // The busy strip — reserved unconditionally so the transcript never jumps
    // a row at turn start/end.
    if p.busy {
        let text = busy_line(p.activity, p.elapsed_ms, p.tick, p.turn_tokens);
        let mut chars = text.chars();
        let head: String = chars.by_ref().take(2).collect();
        let rest: String = chars.collect();
        put(
            y,
            Line::from(vec![
                Span::styled(head, Style::default().fg(ACCENT)),
                Span::styled(pad_row(&rest, width.saturating_sub(2)), dim),
            ]),
            buf,
        );
    } else if let Some(activity) = p.activity {
        put(
            y,
            Line::from(vec![
                Span::styled("⋯ ", Style::default().fg(ACCENT)),
                Span::styled(pad_row(activity, width.saturating_sub(2)), dim),
            ]),
            buf,
        );
    } else {
        put(y, Line::from(Span::raw(pad_row(" ", width))), buf);
    }
    y += 1;

    if let Some(notice) = p.notice {
        put(y, Line::from(Span::styled(pad_row(notice, width), Style::default().fg(WARN))), buf);
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

    fn props<'a>(lines: &'a [String]) -> ChatProps<'a> {
        ChatProps {
            lines,
            width: 80,
            height: 20,
            scroll_off: 0,
            activity: None,
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
        let frame = draw(&ChatProps { height: 4, ..props(&[]) }, 80, 4);
        assert!(frame.contains("one program per round"), "{frame}");
    }

    // Chat.test.tsx: "Chat renders a transcript and its scroll indicator from fixtures"
    // (wave-1 subset: transcript + queued row + activity strip + scroll indicator;
    // the tool-fold assertions belong to lines.rs, row 1.37).
    #[test]
    fn renders_transcript_queued_and_scroll_indicator() {
        let lines: Vec<String> = (0..10).map(|i| format!("row {i}")).collect();
        let queued = vec!["and fix the lint".to_string()];
        let p = ChatProps {
            activity: Some("running the test suite"),
            queued: &queued,
            ..props(&lines)
        };
        let frame = draw(&p, 80, 20);
        assert!(frame.contains("running the test suite"), "{frame}");
        assert!(frame.contains("⧖ queued: and fix the lint"), "{frame}");
        assert!(frame.contains("row 9"), "{frame}");

        // Scrolled up, the window says how much is below and where the top sits.
        let scrolled = draw(&ChatProps { height: 4, scroll_off: 2, ..props(&lines) }, 80, 4);
        assert!(scrolled.contains("↓ 2 more lines below ·"), "{scrolled}");
    }

    #[test]
    fn busy_line_always_names_the_way_out() {
        assert_eq!(busy_line(None, 9_000, 0, None), "⠋ working · 9s · esc interrupts");
        assert_eq!(
            busy_line(Some("reading files"), 64_000, 1, Some(3_200)),
            "⠙ reading files · 1m04s · 3.2k tok · esc interrupts"
        );
        // Zero tokens are omitted, not printed.
        assert_eq!(busy_line(None, 0, 0, Some(0)), "⠋ working · 0s · esc interrupts");
    }

    #[test]
    fn visible_slice_bottom_hang_math() {
        // Pinned to the tail.
        let s = visible_slice(10, 4, 0);
        assert_eq!((s.start, s.more, s.pct), (6, 0, 100));
        // Scrolled up two.
        let s = visible_slice(10, 4, 2);
        assert_eq!((s.start, s.more), (4, 2));
        // Clamped at the top; pct reads 0.
        let s = visible_slice(10, 4, 99);
        assert_eq!((s.start, s.more, s.pct), (0, 6, 0));
        // Short list: no clamping, 100%.
        let s = visible_slice(2, 4, 0);
        assert_eq!((s.start, s.rows.len(), s.pct), (0, 2, 100));
    }

    #[test]
    fn chat_body_height_reserves_the_strips() {
        assert_eq!(chat_body_height(20, 0, false), 18);
        assert_eq!(chat_body_height(20, 2, true), 15);
        assert_eq!(chat_body_height(3, 5, true), 1); // floor is 1, never 0
    }
}
