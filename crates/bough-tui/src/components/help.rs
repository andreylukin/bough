//! The `?` overlay, rendered FROM THE KEYMAP so it can never drift out of
//! date (port of `App.tsx`'s `Help` + `clampHelpOffset`).
//!
//! There is exactly one description of what a key does, and it is the thing
//! that makes the key do it: every row below comes from `keys::BINDINGS` via
//! `keys::help_sections` / `keys::help_lines`. Nothing here is hand-listed,
//! and `keys::dead_bindings` proves no binding is unreachable — a documented
//! chord that cannot fire is a lie the overlay would tell forever.
//!
//! A WINDOW, not a page. The keymap is ~50 rows and a terminal is 24, so this
//! renders `body` rows starting at `offset` and says so in the footer. The
//! clamp is shared with the key handler: an unclamped offset run past the end
//! blanks the overlay, which is exactly the class of bug that shipped once.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::keys::{help_lines, help_sections, HelpLine, HelpLineKind, BINDINGS};

use super::{accent, info};

/// Rows `↑`/`↓` move the overlay (App.tsx::HELP_STEP).
pub const HELP_STEP: usize = 3;

/// What bough IS, for the reader who pressed `?` to find out.
pub const INTRO: &str = "type a task; the agent writes one program per round and runs it here";

/// The overlay's rows, keymap-derived. Recomputed rather than cached: the
/// table is static and the list is fifty rows.
pub fn overlay_lines() -> Vec<HelpLine> {
    help_lines(&help_sections(&BINDINGS))
}

/// The body height: two chrome rows, the header and the position footer.
fn body_rows(rows: usize) -> usize {
    rows.saturating_sub(2).max(1)
}

/// Clamp the overlay's scroll so the last page is the last page. The clamp and
/// the render MUST agree — the key handler calls this too.
pub fn clamp_help_offset(offset: usize, total: usize, rows: usize) -> usize {
    offset.min(total.saturating_sub(body_rows(rows)))
}

/// The overlay as painted lines, given the terminal's row count and the
/// current scroll offset.
pub fn help_view(rows: usize, offset: usize) -> Vec<Line<'static>> {
    let all = overlay_lines();
    let body = body_rows(rows);
    let start = clamp_help_offset(offset, all.len(), rows);
    let visible: &[HelpLine] = &all[start.min(all.len())..(start + body).min(all.len())];
    let more = all.len() - (start + visible.len());

    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut out: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(
            "keys · esc closes",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        // THE ONE SENTENCE THE OVERLAY WAS MISSING. Fifty rows of chords
        // answer "which key does what" for someone who already knows what the
        // program is; a first-time reader arrives here from the `? help` hint
        // without that, and every row after this one presumes it.
        Span::styled(format!("  ·  {INTRO}"), dim),
    ])];
    for l in visible {
        out.push(match l.kind {
            HelpLineKind::Blank => Line::from(Span::raw(" ")),
            HelpLineKind::Header => Line::from(Span::styled(
                l.desc.clone(),
                if l.muted {
                    dim
                } else {
                    Style::default().fg(accent())
                },
            )),
            HelpLineKind::Row => {
                let desc =
                    Span::styled(l.desc.clone(), if l.muted { dim } else { Style::default() });
                if l.prose {
                    Line::from(vec![Span::styled("  · ", dim), desc])
                } else {
                    Line::from(vec![
                        Span::styled(
                            format!("  {:<12}", l.chord),
                            if l.muted {
                                dim
                            } else {
                                Style::default().fg(info())
                            },
                        ),
                        desc,
                    ])
                }
            }
        });
    }
    // The legend names both, because the page keys are why the last section is
    // reachable at all — ↑↓ alone put it forty presses down.
    out.push(Line::from(Span::styled(
        if more > 0 {
            format!("↑↓ pgup/pgdn scroll · {more} more below")
        } else if start > 0 {
            "↑↓ pgup/pgdn scroll · end".to_string()
        } else {
            "↑↓ pgup/pgdn scroll".to_string()
        },
        dim,
    )));
    out
}

/// The full-screen overlay — the one surface that displaces everything.
pub fn render_help(rows: usize, offset: usize, area: Rect, buf: &mut Buffer) {
    for (i, line) in help_view(rows, offset)
        .iter()
        .take(area.height as usize)
        .enumerate()
    {
        buf.set_line(area.x, area.y + i as u16, line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{dead_bindings, TABS};

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    }

    /// THE GATE: a documented chord that cannot fire is a lie the overlay
    /// tells forever. Every binding must be reachable in some context.
    #[test]
    fn no_binding_is_unreachable_so_the_overlay_cannot_document_a_dead_key() {
        assert_eq!(dead_bindings(&BINDINGS), Vec::<String>::new());
    }

    #[test]
    fn the_overlay_is_generated_from_the_table_never_hand_listed() {
        let rendered = text(&help_view(200, 0)).join("\n");
        // Every tab's chord and its own description reach the overlay because
        // `TABS` generates both the binding and the row.
        for t in TABS.iter() {
            assert!(rendered.contains(t.desc), "missing tab row: {}", t.desc);
        }
        // …and the sections the table declares are all headed.
        for section in ["compose", "inside the panel", "the changes tab", "won't do"] {
            assert!(rendered.contains(section), "missing section: {section}");
        }
    }

    #[test]
    fn it_is_a_window_and_the_footer_says_how_much_is_below() {
        let total = overlay_lines().len();
        assert!(total > 24, "the keymap should outgrow one screen: {total}");
        let top = text(&help_view(24, 0));
        assert_eq!(top.len(), 24, "the overlay must fill exactly its rows");
        // The header says what the program IS, not only that esc closes.
        assert_eq!(top[0], format!("keys · esc closes  ·  {INTRO}"));
        assert!(
            top.last().unwrap().contains("more below"),
            "{:?}",
            top.last()
        );

        // The last page says `end`, and never blanks: an unclamped offset
        // scrolled the body off the screen entirely.
        let bottom = text(&help_view(24, 10_000));
        assert_eq!(bottom.len(), 24);
        assert_eq!(bottom.last().unwrap(), "↑↓ pgup/pgdn scroll · end");
        assert!(
            bottom[1..].iter().any(|r| r.trim() != ""),
            "the overlay went blank"
        );

        // A terminal tall enough for everything offers no scroll hint.
        let all = text(&help_view(total + 2, 0));
        assert_eq!(all.last().unwrap(), "↑↓ pgup/pgdn scroll");
    }

    #[test]
    fn the_clamp_is_shared_with_the_key_handler() {
        let total = overlay_lines().len();
        assert_eq!(clamp_help_offset(0, total, 24), 0);
        assert_eq!(clamp_help_offset(5, total, 24), 5);
        assert_eq!(clamp_help_offset(total + 99, total, 24), total - 22);
        // A terminal shorter than the chrome still keeps one body row.
        assert_eq!(clamp_help_offset(total + 99, total, 1), total - 1);
    }

    #[test]
    fn a_prose_row_carries_no_chord_column_and_a_muted_section_stays_dim() {
        // `won't do` rows are prose: they have nothing to press.
        let rendered = text(&help_view(400, 0));
        assert!(
            rendered
                .iter()
                .any(|r| r == "  · ^c ^c quits; subagents keep running"),
            "{rendered:?}"
        );
        // `not bound` rows keep their chord column, dimmed.
        assert!(
            rendered
                .iter()
                .any(|r| r.starts_with("  ^r          no reverse search")),
            "{rendered:?}"
        );
    }
}
