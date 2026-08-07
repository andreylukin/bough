//! The input box (port of `src/tui/components/Composer.tsx`, wave-1 subset).
//!
//! THE INVARIANT THIS HOLDS: **the cursor is exactly where the box says it is.**
//! The text is wrapped here, into fixed-width chunks, rather than by the
//! renderer — so the character→row mapping is computed, not inferred from a
//! layout pass.
//!
//! SECOND INVARIANT — **the box never grows past its cap.** A large paste is
//! windowed to `max_rows` around the cursor with a counter row saying what is
//! above and below; the text itself is untouched.
//!
//! THIRD — **this component is presentational.** Props in, cells out.
//!
//! Wave-1 stubs kept honest: ghost text is plumbed but always empty (cheap
//! tier is `None`), the completion popup and attachments are deferred with the
//! rest of their plumbing (spec §8 v1 scope cut) — `attachments` stays in the
//! height contract so the frame arithmetic does not change shape when they land.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use super::{accent, bg, muted, panel_inset, warn};
use crate::format::{Completion, TriggerKind};

pub struct ComposerProps<'a> {
    pub input: &'a str,
    /// Char index into `input` (keys.ts: the cursor is a char index, never bytes).
    pub cursor: usize,
    /// A turn is running: Enter interjects into it rather than starting a new one.
    pub busy: bool,
    pub width: u16,
    pub max_rows: usize,
    /// Dim autocomplete preview appended after the input. Wave 1: always `""`.
    pub ghost: &'a str,
    /// Image attachment names.
    pub attachments: &'a [String],
    /// Which attachment row is selected, if any. ↑/↓ move it on an empty
    /// draft and Backspace deletes it — so the row has to SAY which one is
    /// about to go (App.tsx::delete.back).
    pub attachment_sel: Option<usize>,
    /// The surface that has the keyboard INSTEAD of this one, e.g. `"the tree"`.
    /// None means the composer is focused.
    pub keyboard_owner: Option<&'a str>,
}

/// Rows `render_completion_popup` will draw, for the same reason as
/// [`composer_height`] (Composer.tsx::completionPopupHeight).
pub fn completion_popup_height(items: usize, more: usize) -> usize {
    2 /* border */ + items.max(1) + usize::from(more > 0) + 1 /* legend */
}

/// The `@`/`/` menu (Composer.tsx::CompletionPopup).
pub struct CompletionPopupProps<'a> {
    pub kind: TriggerKind,
    pub items: &'a [Completion],
    /// -1 = browsing; Enter then keeps the typed text rather than a listed row.
    /// Rust carries that as `None` — an index cannot be negative.
    pub sel: Option<usize>,
    pub more: usize,
}

/// A filter that matches nothing still shows the box, saying so: silently
/// hiding it reads as "the picker is broken" rather than "no such file".
pub fn render_completion_popup(p: &CompletionPopupProps, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(muted()));
    let body = block.inner(area);
    block.render(area, buf);
    let body = Rect {
        x: body.x + 1,
        width: body.width.saturating_sub(2),
        ..body
    }; // paddingX
    let dim = Style::default().add_modifier(Modifier::DIM);

    let mut lines: Vec<Line> = Vec::new();
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled(
            match p.kind {
                TriggerKind::File => "no matching files",
                TriggerKind::Skill => "no matching commands or skills",
            },
            dim,
        )));
    } else {
        for (i, it) in p.items.iter().enumerate() {
            let selected = p.sel == Some(i);
            // File rows dim the directory prefix so basenames stand out.
            let dim_to = match p.kind {
                TriggerKind::File => it
                    .label
                    .chars()
                    .enumerate()
                    .filter(|(_, c)| *c == '/')
                    .map(|(i, _)| i + 1)
                    .last()
                    .unwrap_or(0),
                TriggerKind::Skill => 0,
            };
            // A `❯` and an accent, not a reverse-video bar: reverse renders
            // white-on-white here, so the row Enter was about to act on was
            // marked with nothing at all. This is the same cursor glyph every
            // other list in the TUI uses.
            let mut spans = vec![Span::styled(
                if selected { "❯ " } else { "  " },
                Style::default().fg(accent()),
            )];
            spans.extend(popup_label(&it.label, &it.hl, dim_to));
            if !it.detail.is_empty() {
                spans.push(Span::styled(format!("  {}", it.detail), dim));
            }
            lines.push(Line::from(spans));
        }
    }
    if p.more > 0 {
        // Keeps the row cap honest: without this a first-run user reads the
        // menu as the whole catalogue and never types to narrow it.
        lines.push(Line::from(vec![
            Span::styled(format!("↓ {}", p.more), Style::default().fg(super::info())),
            Span::styled(" more — keep typing to narrow", dim),
        ]));
    }
    // ⏎ is named FIRST because it is the commit key here too. "runs or inserts"
    // on the `/` list because a built-in command row RUNS and a skill row
    // inserts — the legend says both rather than promising the one behaviour
    // that would be wrong for whichever row is highlighted.
    lines.push(Line::from(Span::styled(
        match p.kind {
            TriggerKind::File => "files & dirs — ↑↓ select · ⏎ or ⇥ inserts · esc closes",
            TriggerKind::Skill => "commands & skills — ↑↓ select · ⏎ runs or inserts · esc closes",
        },
        dim,
    )));

    for (i, line) in lines.into_iter().enumerate() {
        if i as u16 >= body.height {
            break;
        }
        buf.set_line(body.x, body.y + i as u16, &line, body.width);
    }
}

/// A label with the fuzzy-matched characters emphasized (Composer.tsx::PopupLabel).
fn popup_label<'a>(label: &'a str, hl: &[usize], dim_to: usize) -> Vec<Span<'a>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    if hl.is_empty() {
        if dim_to > 0 {
            let head: String = label.chars().take(dim_to).collect();
            let tail: String = label.chars().skip(dim_to).collect();
            return vec![Span::styled(head, dim), Span::raw(tail)];
        }
        return vec![Span::raw(label)];
    }
    label
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            if hl.contains(&i) {
                Span::styled(
                    ch.to_string(),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                )
            } else if i < dim_to {
                Span::styled(ch.to_string(), dim)
            } else {
                Span::raw(ch.to_string())
            }
        })
        .collect()
}

/// Rows the box will draw, so the container can SIZE the region above it
/// instead of guessing. **Must mirror the render exactly** — the two are
/// edited together (Composer.tsx::composerHeight).
pub fn composer_height(
    input: &str,
    ghost: &str,
    busy: bool,
    width: u16,
    max_rows: usize,
    attachments: usize,
) -> usize {
    let inner_w = inner_width(width);
    let full = full_text(input, ghost);
    let mut n = 0usize;
    for line in full.split('\n') {
        let len = line.chars().count();
        n += 1.max(len.div_ceil(inner_w));
    }
    let cap = max_rows.max(2);
    let clipped = n > cap;
    let hint = usize::from((busy && !input.is_empty()) || input.starts_with('!'));
    2 + (if clipped { cap - 1 } else { n }) + usize::from(clipped) + hint + attachments
}

fn inner_width(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(4) // border + paddingX
}

fn full_text(input: &str, ghost: &str) -> String {
    let ghost_hint = if ghost.is_empty() { "" } else { "  ⇥ tab" };
    format!("› {input}{ghost}{ghost_hint}")
}

/// One wrapped row: its char offset into the full text, and its chars.
struct WrapRow {
    start: usize,
    text: Vec<char>,
}

/// Fixed-width chunks over the full text, char-based, exactly as the TS wraps.
fn wrap_rows(full: &str, inner_w: usize) -> Vec<WrapRow> {
    let mut rows = Vec::new();
    let mut off = 0usize;
    for line in full.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        loop {
            let end = (i + inner_w).min(chars.len());
            rows.push(WrapRow {
                start: off + i,
                text: chars[i..end].to_vec(),
            });
            if i + inner_w >= chars.len() {
                break;
            }
            i += inner_w;
        }
        off += chars.len() + 1;
    }
    rows
}

/// The cursor's row: within `[start, start+len)`, or sitting at the row's end
/// when nothing continues it there.
fn cursor_row(rows: &[WrapRow], cur: usize) -> usize {
    rows.iter()
        .enumerate()
        .position(|(i, r)| {
            let end = r.start + r.text.len();
            let next_start = rows.get(i + 1).map(|n| n.start).unwrap_or(usize::MAX);
            cur >= r.start && (cur < end || (cur == end && next_start > end))
        })
        .unwrap_or(0)
}

pub fn render_composer(p: &ComposerProps, area: Rect, buf: &mut Buffer) {
    let border_color = if p.keyboard_owner.is_some() {
        muted()
    } else if p.busy {
        warn()
    } else {
        accent()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(panel_inset()));
    let body = block.inner(area);
    block.render(area, buf);
    // paddingX = 1 inside the border.
    let body = Rect {
        x: body.x + 1,
        width: body.width.saturating_sub(2),
        ..body
    };

    let inner_w = inner_width(p.width);
    // An empty composer states the first action; a ghost suppresses it (the two
    // would paint into the same cells). When another surface has the keyboard
    // the placeholder names it instead.
    let placeholder = match p.keyboard_owner {
        Some(owner) => {
            if p.input.is_empty() {
                format!("{owner} has the keyboard · esc returns here")
            } else {
                String::new()
            }
        }
        None => {
            if p.input.is_empty() && p.ghost.is_empty() {
                "type a message · enter sends".to_string()
            } else {
                String::new()
            }
        }
    };
    let full = full_text(p.input, p.ghost);
    let ghost_start = 2 + p.input.chars().count();
    let cur = p.cursor + 2;
    let rows = wrap_rows(&full, inner_w);
    let cur_row = cursor_row(&rows, cur);
    let cap = p.max_rows.max(2);
    let clipped = rows.len() > cap;
    let shown_count = if clipped { cap - 1 } else { rows.len() }; // one row for the … counter
    let top = if clipped {
        (cur_row.saturating_sub(shown_count >> 1)).min(rows.len() - shown_count)
    } else {
        0
    };

    let dim = Style::default().add_modifier(Modifier::DIM);
    let caret = Style::default().fg(bg()).bg(accent());
    let mut lines: Vec<Line> = Vec::new();
    for (i, r) in rows.iter().enumerate().skip(top).take(shown_count) {
        // No caret when the keyboard is elsewhere: a block cursor is the single
        // strongest claim a terminal UI can make about where typing goes.
        let has_cursor = p.keyboard_owner.is_none() && i == cur_row;
        let prefix = if r.start == 0 { 2 } else { 0 };
        let seg = |from: usize, to: usize| -> String {
            r.text[from.min(r.text.len())..to.min(r.text.len())]
                .iter()
                .collect()
        };
        // Where this row crosses into ghost text — everything from there is dim.
        let gcol = (ghost_start.saturating_sub(r.start)).clamp(prefix, r.text.len());
        let mut spans: Vec<Span> = Vec::new();
        if prefix > 0 {
            spans.push(Span::styled("› ", Style::default().fg(accent())));
        }
        if has_cursor {
            let col = cur - r.start;
            let at: Option<char> = r.text.get(col).copied();
            spans.push(Span::raw(seg(prefix, col)));
            spans.push(Span::styled(
                at.map(String::from).unwrap_or_else(|| " ".into()),
                caret,
            ));
            if !placeholder.is_empty() {
                spans.push(Span::styled(placeholder.clone(), dim));
            }
            if at.is_some() {
                let rest = seg(col + 1, r.text.len());
                let rest_style = if col + 1 >= gcol {
                    dim
                } else {
                    Style::default()
                };
                spans.push(Span::styled(rest, rest_style));
            }
        } else if r.text.len() <= prefix {
            // An empty first row with no caret: the placeholder lives here instead.
            if !placeholder.is_empty() {
                spans.push(Span::styled(placeholder.clone(), dim));
            } else {
                spans.push(Span::raw(" "));
            }
        } else {
            spans.push(Span::raw(seg(prefix, gcol)));
            if gcol < r.text.len() {
                spans.push(Span::styled(seg(gcol, r.text.len()), dim));
            }
        }
        lines.push(Line::from(spans));
    }
    // Images only — a held paste has no row of its own: its mark sits in the draft.
    for (i, name) in p.attachments.iter().enumerate() {
        let selected = p.attachment_sel == Some(i);
        // The cursor is the whole affordance: without it, Backspace deletes
        // something the screen never said was chosen.
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "❯ " } else { "  " },
                Style::default().fg(super::accent()),
            ),
            Span::styled(
                format!("[image: {name}]"),
                if selected {
                    Style::default()
                        .fg(super::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(super::info())
                },
            ),
            Span::styled(
                if selected { "  ⌫ removes" } else { "" },
                Style::default().fg(super::muted()),
            ),
        ]));
    }
    if clipped {
        let above = top;
        let below = rows.len() - top - shown_count;
        lines.push(Line::from(Span::styled(
            format!(
                "… {above} line{} above · {below} below",
                if above == 1 { "" } else { "s" }
            ),
            dim,
        )));
    }
    // A context hint under the box: a plain Enter mid-turn steers the running
    // turn rather than starting a new one; a `!` line goes to your shell.
    let hint = if p.busy && !p.input.is_empty() {
        "enter interjects this turn"
    } else if p.input.starts_with('!') {
        "runs in your shell · not a message · output lands in the rail"
    } else {
        ""
    };
    if !hint.is_empty() {
        lines.push(Line::from(Span::styled(hint, dim)));
    }

    for (i, line) in lines.into_iter().enumerate() {
        if i as u16 >= body.height {
            break; // a tab body must never emit more rows than its budget
        }
        buf.set_line(body.x, body.y + i as u16, &line, body.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn draw(p: &ComposerProps, cols: u16, rows: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render_composer(p, area, f.buffer_mut());
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

    fn props<'a>(input: &'a str, cursor: usize, busy: bool) -> ComposerProps<'a> {
        ComposerProps {
            input,
            cursor,
            busy,
            width: 60,
            max_rows: 6,
            ghost: "",
            attachments: &[],
            attachment_sel: None,
            keyboard_owner: None,
        }
    }

    // Chat.test.tsx: "Composer shows the prompt, the placeholder and the mid-turn hint"
    #[test]
    fn shows_prompt_placeholder_and_mid_turn_hint() {
        let empty = draw(&props("", 0, false), 60, 8);
        assert!(empty.contains("type a message · enter sends"), "{empty}");

        let busy = draw(&props("also this", 9, true), 60, 8);
        assert!(busy.contains("enter interjects this turn"), "{busy}");
        assert!(busy.contains("also this"), "{busy}");
    }

    // Chat.test.tsx: "Composer caps its height on a large paste and says what is off-screen"
    #[test]
    fn caps_height_on_a_large_paste_and_says_what_is_off_screen() {
        let input: String = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = ComposerProps {
            max_rows: 5,
            width: 60,
            ..props(&input, input.chars().count(), false)
        };
        let frame = draw(&p, 60, 10);
        assert!(frame.contains("lines above ·"), "{frame}");
        assert!(
            !frame.contains("line 0 "),
            "windowed to the cursor: {frame}"
        );
        assert!(frame.contains("line 29"), "{frame}");
    }

    // Chat.test.tsx: "Composer height reserves a row for each attachment"
    #[test]
    fn height_reserves_a_row_for_each_attachment() {
        assert_eq!(
            composer_height("", "", false, 60, 6, 1),
            composer_height("", "", false, 60, 6, 0) + 1,
        );
    }

    #[test]
    fn height_mirrors_the_render_contract() {
        // 2 border + 1 text row, empty idle composer.
        assert_eq!(composer_height("", "", false, 60, 6, 0), 3);
        // busy + non-empty adds the hint row.
        assert_eq!(composer_height("hi", "", true, 60, 6, 0), 4);
        // `!` line always carries its hint row.
        assert_eq!(composer_height("!ls", "", false, 60, 6, 0), 4);
        // clipped: cap-1 shown rows + counter row.
        let long: String = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(composer_height(&long, "", false, 60, 5, 0), 2 + 4 + 1);
        // a ghost widens the text ("  ⇥ tab" tail) but adds no chrome rows.
        assert_eq!(composer_height("", "do it", false, 60, 6, 0), 3);
    }

    fn draw_popup(p: &CompletionPopupProps, cols: u16, rows: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render_completion_popup(p, area, f.buffer_mut());
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

    // Chat.test.tsx: "Composer renders the @ popup for the trigger under the cursor"
    #[test]
    fn renders_the_at_popup_for_the_trigger_under_the_cursor() {
        let text = "look at @app";
        let trigger = crate::format::active_trigger(text, text.chars().count()).unwrap();
        let candidates: Vec<crate::format::Candidate> = ["server/app.ts", "app.tsx", "docs/app.md"]
            .iter()
            .map(|n| crate::format::Candidate::file(*n))
            .collect();
        let ranked = crate::format::rank_completions(&candidates, &trigger, 2);
        let frame = draw_popup(
            &CompletionPopupProps {
                kind: trigger.kind,
                items: &ranked.items,
                sel: Some(0),
                more: ranked.total - ranked.items.len(),
            },
            60,
            completion_popup_height(ranked.items.len(), ranked.total - ranked.items.len()) as u16,
        );
        assert!(frame.contains("@app.tsx"), "{frame}");
        assert!(frame.contains("files & dirs"), "{frame}");
        assert!(frame.contains("↓ 1"), "{frame}");
        assert!(frame.contains("❯ "), "the row ⏎ acts on is marked: {frame}");
    }

    // Chat.test.tsx: "Composer's / popup says so when nothing matches, rather than vanishing"
    #[test]
    fn the_slash_popup_says_so_when_nothing_matches_rather_than_vanishing() {
        let frame = draw_popup(
            &CompletionPopupProps {
                kind: TriggerKind::Skill,
                items: &[],
                sel: Some(0),
                more: 0,
            },
            60,
            completion_popup_height(0, 0) as u16,
        );
        assert!(frame.contains("no matching commands or skills"), "{frame}");
        assert!(frame.contains("commands & skills"), "{frame}");
    }

    #[test]
    fn popup_height_mirrors_the_render_contract() {
        // border + one row (the empty-state line) + legend.
        assert_eq!(completion_popup_height(0, 0), 4);
        // three rows + the "↓ N more" counter.
        assert_eq!(completion_popup_height(3, 2), 2 + 3 + 1 + 1);
    }

    #[test]
    fn keyboard_owner_names_the_owner_and_drops_the_caret() {
        let p = ComposerProps {
            keyboard_owner: Some("the tree"),
            ..props("", 0, false)
        };
        let frame = draw(&p, 60, 6);
        assert!(
            frame.contains("the tree has the keyboard · esc returns here"),
            "{frame}"
        );
    }
}
