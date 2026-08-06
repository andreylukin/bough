//! The ask card (port of `AskCard` + `askPromptLines` in
//! `src/tui/components/App.tsx`, PORT_PLAN row 2.21).
//!
//! A held `ask()` replaces the composer and owns the keyboard. The card is
//! presentational — props in, cells out; the I/O half (answer/decline, the
//! settled race) lives in `store::lifecycle`.
//!
//! THE INVARIANT THIS HOLDS: **one row per line, and the height the App lays
//! out is the height this draws.** The prompt used to be one no-wrap row,
//! which is right for exactly the shape `ask()` shipped with ("Deploy to prod
//! or staging?") and silently wrong for any other: an embedded newline opens
//! no row, so a multi-line question and the numbered options below it painted
//! into the SAME cells — the workflow approval card rendered as
//! `21noneitew files — One agent per f*.js file`.
//!
//! SECOND — **and wrapped to the card**, which the first fix left out. The
//! approval card's last line is the sentence that says how to stop a run, and
//! at 120 columns it read `…`x` in the workflows t` — clipped mid-word, on the
//! card whose whole job is to be read before something bills. Rows are cheap
//! here (the card is sized from [`ask_prompt_lines`]'s own output); a sentence
//! the user cannot finish is not.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::ansi::wrap_line;

use super::{ACCENT, PANEL_INSET, WARN};

/// The prompt, split on `\n` AND wrapped to `width - 4` (2 border columns +
/// 2 paddingX, matching the card's own box), capped at a third of the screen.
///
/// At most a third, because a question is asked INSTEAD of the composer, so a
/// long one squeezes the transcript it is asking about; past the cap the card
/// SAYS it clipped rather than quietly dropping the tail of a spend
/// confirmation.
pub fn ask_prompt_lines(prompt: &str, rows: u16, width: u16) -> Vec<String> {
    let cap = ((rows as usize) / 3).max(1);
    let inner = (width as usize).saturating_sub(4);
    let mut lines: Vec<String> = Vec::new();
    for logical in prompt.split('\n') {
        if logical.is_empty() {
            // A blank line is a row too — it is what spaces the card.
            lines.push(String::new());
        } else {
            lines.extend(wrap_line(logical, inner));
        }
    }
    if lines.len() <= cap {
        return lines;
    }
    let more = lines.len() - cap + 1;
    let mut out: Vec<String> = lines[..cap - 1].to_vec();
    out.push(format!("… {more} more lines"));
    out
}

/// Rows the card draws IN TOTAL: 2 border + prompt + options + the typed row +
/// the legend. **The App sizes `input_h` from this** — a card that draws more
/// rows than the frame reserved paints over the status line (App.tsx:
/// `inputH = 4 + askLines.length + options.length`).
pub fn ask_card_height(prompt_lines: usize, options: usize) -> usize {
    4 + prompt_lines + options
}

pub struct AskCardProps<'a> {
    /// Already through [`ask_prompt_lines`] — the card never re-wraps, so the
    /// height and the render read the same rows.
    pub lines: &'a [String],
    pub options: &'a [String],
    /// What the user has typed so far into the free-text answer.
    pub typed: &'a str,
}

pub fn render_ask_card(p: &AskCardProps, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        // Warn, because the turn is parked until this is answered.
        .border_style(Style::default().fg(WARN))
        .style(Style::default().bg(PANEL_INSET));
    let inner = block.inner(area);
    block.render(area, buf);
    // paddingX = 1 inside the border.
    let body = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };

    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut rows: Vec<Line> = Vec::new();
    for line in p.lines {
        rows.push(Line::from(Span::raw(line.clone())));
    }
    for (i, option) in p.options.iter().enumerate() {
        rows.push(Line::from(vec![
            Span::styled(format!(" {} ", i + 1), Style::default().fg(ACCENT)),
            Span::raw(option.clone()),
        ]));
    }
    rows.push(Line::from(vec![
        Span::styled("› ", dim),
        Span::raw(p.typed.to_string()),
    ]));
    rows.push(Line::from(Span::styled(
        format!(
            "{}type an answer · ⏎ send · esc decline",
            if p.options.is_empty() {
                ""
            } else {
                "1-9 pick · "
            }
        ),
        dim,
    )));

    for (i, line) in rows.into_iter().enumerate() {
        // A card must never emit more rows than its budget (spec §4: row
        // budgets are claims).
        if i as u16 >= body.height {
            break;
        }
        buf.set_line(body.x, body.y + i as u16, &line, body.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Draws the card into a region of exactly `card_rows`, inside a taller
    /// frame — so a test can assert that nothing spills past the budget.
    fn draw_in(p: &AskCardProps, cols: u16, card_rows: u16, frame_rows: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(cols, frame_rows)).unwrap();
        term.draw(|f| {
            let area = Rect {
                height: card_rows,
                ..f.area()
            };
            render_ask_card(p, area, f.buffer_mut());
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

    fn draw(p: &AskCardProps, cols: u16, rows: u16) -> String {
        draw_in(p, cols, rows, rows)
    }

    /// Chat.test.tsx: "a multi-line ask reports every line, and clips instead
    /// of overflowing".
    #[test]
    fn a_multi_line_ask_reports_every_line_and_clips_instead_of_overflowing() {
        assert_eq!(
            ask_prompt_lines("one line?", 46, 80),
            vec!["one line?".to_string()]
        );

        let three = ask_prompt_lines("Run it?\n\n  1. describe\n  2. summarize", 46, 80);
        assert_eq!(
            three.len(),
            4,
            "blank lines are rows too — they are what spaces the card"
        );

        // A question taller than a third of the screen is clipped, and says so:
        // silently dropping the tail of a spend confirmation is the one thing
        // it may not do.
        let long: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let clipped = ask_prompt_lines(&long.join("\n"), 30, 80);
        assert_eq!(clipped.len(), 10, "capped at rows/3");
        assert_eq!(clipped.last().unwrap(), "… 31 more lines");
    }

    /// Chat.test.tsx: "a long question wraps to the card instead of being
    /// clipped mid-word" — the line that says how to STOP a run was the one
    /// being cut off.
    #[test]
    fn a_long_question_wraps_to_the_card_instead_of_being_clipped_mid_word() {
        let sentence = "It runs detached and fans out subagents in parallel, so it can spend \
            a lot of tokens quickly. `x` in the workflows tab (^w) stops a run at any point.";
        let wrapped = ask_prompt_lines(sentence, 60, 60);
        assert!(wrapped.len() > 1, "one logical line becomes several rows");
        assert!(
            wrapped.iter().all(|l| super::super::display_width(l) <= 56),
            "every row fits inside border + padding: {wrapped:?}"
        );
        // The tail survives: the whole point is that the escape hatch stays readable.
        assert!(wrapped.join(" ").contains("stops a run at any point."));
    }

    /// Chat.test.tsx: "a question narrower than the width is left alone".
    #[test]
    fn a_question_narrower_than_the_width_is_left_alone() {
        assert_eq!(
            ask_prompt_lines("prod or staging?", 46, 120),
            vec!["prod or staging?".to_string()]
        );
    }

    #[test]
    fn the_height_the_app_reserves_is_the_height_the_card_draws() {
        let lines = ask_prompt_lines("Run it?\nreally?", 46, 60);
        let options = vec!["yes".to_string(), "no".to_string()];
        let height = ask_card_height(lines.len(), options.len());
        assert_eq!(height, 2 + 2 + 2 + 1 + 1);

        // Every row the card claims is painted, and nothing spills past it: the
        // last body row is the legend, and the row under the card is untouched.
        let frame = draw_in(
            &AskCardProps {
                lines: &lines,
                options: &options,
                typed: "",
            },
            70,
            height as u16,
            height as u16 + 1,
        );
        let rows: Vec<&str> = frame.lines().collect();
        assert!(
            rows[height - 2].contains("1-9 pick · type an answer · ⏎ send · esc decline"),
            "{frame}"
        );
        assert!(
            rows[height - 1].contains("╰"),
            "the last row of the budget is the border: {frame}"
        );
        assert!(
            rows[height].trim().is_empty(),
            "the card must not paint below its budget"
        );
    }

    #[test]
    fn the_card_numbers_the_options_and_shows_what_was_typed() {
        let lines = ask_prompt_lines("prod or staging?", 46, 60);
        let options = vec!["prod".to_string(), "staging".to_string()];
        let frame = draw(
            &AskCardProps {
                lines: &lines,
                options: &options,
                typed: "neither, wait",
            },
            60,
            ask_card_height(lines.len(), options.len()) as u16,
        );
        assert!(frame.contains("prod or staging?"), "{frame}");
        assert!(frame.contains(" 1 prod"), "{frame}");
        assert!(frame.contains(" 2 staging"), "{frame}");
        assert!(frame.contains("› neither, wait"), "{frame}");
    }

    #[test]
    fn free_text_only_holds_drop_the_pick_half_of_the_legend() {
        let lines = ask_prompt_lines("what should it be called?", 46, 60);
        let frame = draw(
            &AskCardProps {
                lines: &lines,
                options: &[],
                typed: "",
            },
            60,
            ask_card_height(lines.len(), 0) as u16,
        );
        assert!(
            frame.contains("type an answer · ⏎ send · esc decline"),
            "{frame}"
        );
        assert!(
            !frame.contains("1-9 pick"),
            "no options, no pick legend: {frame}"
        );
    }
}
