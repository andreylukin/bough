//! The skills tab: the `/name` instruction bundles this install can load
//! (port of `src/tui/components/Skills.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **an empty list, an absent source and a BROKEN
//! skill are three different screens.** `None` means nothing has answered yet or
//! the fetch failed — say why in `note`, and never render it as "no skills
//! installed", which is a claim about the user's `~/.bough/skills` that this
//! component has not read. An empty list IS that claim, and it only becomes safe
//! to make once `GET /skills` has answered; the `sources` it rides along with
//! are printed beside it, because "why is my skill not listed?" is almost always
//! answered by naming the directory that was walked.
//!
//! The third case is the one worth a component: a skill whose SKILL.md could not
//! be parsed is served WITH its `error` rather than omitted (`server/skills.rs`),
//! so it is rendered here in the error colour with the reason. A malformed skill
//! that simply vanished from the list would instead be discovered as a `/name`
//! that quietly did nothing, two turns and some money later.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use crate::api::SkillSourceRow;
use crate::components::panel::{legend_line, paint_rows};
use crate::components::{accent, error, info, warn};
use crate::store::selectors::clip;

/// One row of `GET /skills`, as this tab reads it. The composer's own
/// [`crate::api::SkillRow`] is the same wire object narrowed to name +
/// description; this is the full row, error included.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SkillRow {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Present when SKILL.md could not be parsed. Served rather than omitted.
    #[serde(default)]
    pub error: Option<String>,
    /// MCP servers this skill declares it needs.
    #[serde(default)]
    pub mcp: Vec<String>,
}

/// The visible slice, sized from what is left after the chrome.
///
/// `max(3, rows - 6)` claimed three list rows at any height, so a short panel
/// painted more rows than it had. Chrome here is the counter row, the
/// `read from …` line and the legend — the legend is the last row of the tab and
/// never gives up its place.
pub fn skills_window(
    count: usize,
    selected: usize,
    rows: usize,
    chrome: usize,
) -> (usize, usize, bool) {
    let avail = rows.saturating_sub(chrome + 1 /* legend */);
    // Content over indicators when it is tight: a lone `1/40` row above no
    // skills at all is a position report about a list nobody can see.
    let counter = count > avail && avail >= 2;
    let height = avail.saturating_sub(usize::from(counter));
    let at = selected.min(count.saturating_sub(1));
    let start = at
        .saturating_sub(height / 2)
        .min(count.saturating_sub(height));
    (start, height, counter)
}

pub struct SkillsTabProps<'a> {
    /// `None` = nothing has answered yet. Say why in `note`; never fake an empty
    /// list.
    pub skills: Option<&'a [SkillRow]>,
    pub rows: usize,
    /// Columns available. The description used to be clipped at a hardcoded 60
    /// characters, so at 200 columns a skill's description still cut off at
    /// column 80 with 120 blank columns beside it.
    pub cols: usize,
    /// Cursor row.
    pub selected: usize,
    /// Why the list is absent. Shown only when `skills` is `None`.
    pub note: Option<&'a str>,
    /// The directories that were walked. Printed so an empty list is
    /// diagnosable.
    pub sources: &'a [SkillSourceRow],
    /// The `/` filter buffer. Narrowing happens in the host; this only draws it.
    pub filter: &'a str,
    pub filtering: bool,
}

impl Default for SkillsTabProps<'_> {
    fn default() -> Self {
        SkillsTabProps {
            skills: None,
            rows: 10,
            cols: 80,
            selected: 0,
            note: None,
            sources: &[],
            filter: "",
            filtering: false,
        }
    }
}

/// The lines this tab paints, in order.
pub fn skills_lines(p: &SkillsTabProps) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let Some(skills) = p.skills else {
        return match p.note {
            Some(note) => vec![Line::from(Span::styled(
                note.to_string(),
                Style::default().fg(warn()),
            ))],
            None => vec![Line::from(Span::styled("loading…", dim))],
        };
    };
    let where_from: Option<String> = (!p.sources.is_empty()).then(|| {
        p.sources
            .iter()
            .map(|s| format!("{} {}", s.source, s.dir))
            .collect::<Vec<_>>()
            .join(" · ")
    });
    let legend = Line::from(Span::styled(
        if p.filtering {
            "type to narrow · ⌫ back · esc clear the filter · ↑↓ move".to_string()
        } else {
            legend_line(
                &[
                    "↑↓ move",
                    "pgup/pgdn page",
                    "1-9 pick",
                    "/ filter",
                    "name a skill to load it",
                    "esc back",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
                Some(p.cols),
            )
        },
        dim,
    ));

    let mut out: Vec<Line<'static>> = Vec::new();
    if skills.is_empty() {
        out.push(Line::from(Span::styled(
            if p.filter.is_empty() {
                "no skills installed"
            } else {
                "nothing matches that filter"
            },
            dim,
        )));
        if let Some(w) = &where_from {
            out.push(Line::from(Span::styled(format!("read from {w}"), dim)));
        }
        out.push(legend);
        return out;
    }

    let chrome =
        usize::from(p.filtering || !p.filter.is_empty()) + usize::from(where_from.is_some());
    // A WINDOW around the cursor, not the first N rows. The panel has always
    // moved `sel` for this tab, but the list drew from index 0 with no marker,
    // so ↑↓ and ⏎ were documented by the panel and inert here — a skill past the
    // fold could not be reached or read at all.
    let (start, height, counter) = skills_window(skills.len(), p.selected, p.rows, chrome);
    let at = p.selected.min(skills.len() - 1);

    if p.filtering {
        out.push(Line::from(vec![
            Span::styled("/ ", Style::default().fg(accent())),
            Span::raw(p.filter.to_string()),
            Span::styled(
                " ",
                Style::default()
                    .bg(accent())
                    .fg(ratatui::style::Color::Black),
            ),
        ]));
    } else if !p.filter.is_empty() {
        out.push(Line::from(Span::styled(format!("/ {}", p.filter), dim)));
    }
    for (i, s) in skills.iter().skip(start).take(height).enumerate() {
        let on = start + i == at;
        let mut spans = vec![
            Span::styled(
                if i < 9 {
                    format!("{} ", i + 1)
                } else {
                    "  ".into()
                },
                dim,
            ),
            // The `❯` carries the cursor on its own: INVERSE renders invisible
            // in some terminals, so a marked row was marked by a dim chevron and
            // nothing else.
            Span::styled(
                if on { "❯ " } else { "  " },
                if on {
                    Style::default().fg(accent()).add_modifier(Modifier::DIM)
                } else {
                    dim
                },
            ),
            Span::styled(
                format!("/{}", s.name),
                bold.fg(if s.error.is_some() { error() } else { accent() }),
            ),
            Span::styled(
                format!(
                    "  {}",
                    clip(
                        s.error.as_deref().unwrap_or(&s.description),
                        p.cols.saturating_sub(s.name.len() + 8).max(20)
                    )
                ),
                dim,
            ),
        ];
        if !s.mcp.is_empty() {
            spans.push(Span::styled(
                format!("  mcp: {}", s.mcp.join(", ")),
                Style::default().fg(info()),
            ));
        }
        out.push(Line::from(spans));
    }
    if counter {
        out.push(Line::from(Span::styled(
            format!("{}/{} · ↑↓ to see the rest", at + 1, skills.len()),
            dim,
        )));
    }
    if let Some(w) = &where_from {
        out.push(Line::from(Span::styled(format!("read from {w}"), dim)));
    }
    // Last row, like every other tab. `read from …` used to sit here, so the one
    // place a reader learns to look for keys held a directory listing instead.
    out.push(legend);
    out
}

pub fn render_skills(p: &SkillsTabProps, area: Rect, buf: &mut Buffer) {
    paint_rows(&skills_lines(p), area, buf);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn text(p: &SkillsTabProps) -> String {
        skills_lines(p)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn skill(name: &str, description: &str) -> SkillRow {
        SkillRow {
            name: name.into(),
            description: description.into(),
            error: None,
            mcp: Vec::new(),
        }
    }

    #[test]
    fn an_empty_list_and_an_absent_one_are_different_screens() {
        // Nothing has answered: never a claim about the user's skills directory.
        let loading = text(&SkillsTabProps::default());
        assert!(loading.contains("loading…"), "{loading}");
        assert!(!loading.contains("no skills installed"), "{loading}");
        // A failed fetch says why.
        let failed = text(&SkillsTabProps {
            note: Some("the server did not answer /skills"),
            ..Default::default()
        });
        assert!(
            failed.contains("the server did not answer /skills"),
            "{failed}"
        );
        assert!(!failed.contains("no skills installed"), "{failed}");
        // An empty ARRAY is the claim, and only then.
        let empty = text(&SkillsTabProps {
            skills: Some(&[]),
            ..Default::default()
        });
        assert!(empty.contains("no skills installed"), "{empty}");
    }

    #[test]
    fn a_skill_is_listed_by_its_slash_name_and_its_sentence() {
        let skills = vec![skill("history", "query the db")];
        let out = text(&SkillsTabProps {
            skills: Some(&skills),
            ..Default::default()
        });
        assert!(out.contains("/history"), "{out}");
        assert!(out.contains("query the db"), "{out}");
    }

    #[test]
    fn a_broken_skill_is_listed_with_its_reason_not_omitted() {
        // A malformed skill that simply vanished would be discovered as a
        // `/name` that quietly did nothing, two turns and some money later.
        let skills = vec![SkillRow {
            name: "broken".into(),
            description: "never read".into(),
            error: Some("SKILL.md has no front matter".into()),
            mcp: Vec::new(),
        }];
        let out = text(&SkillsTabProps {
            skills: Some(&skills),
            ..Default::default()
        });
        assert!(out.contains("/broken"), "{out}");
        assert!(out.contains("SKILL.md has no front matter"), "{out}");
        // The error REPLACES the description; it does not sit beside it.
        assert!(!out.contains("never read"), "{out}");
    }

    #[test]
    fn the_directories_that_were_walked_are_printed_so_an_empty_list_is_diagnosable() {
        let sources = vec![
            SkillSourceRow {
                source: "bundled".into(),
                dir: "/opt/bough/skills".into(),
            },
            SkillSourceRow {
                source: "user".into(),
                dir: "/home/u/.bough/skills".into(),
            },
        ];
        let out = text(&SkillsTabProps {
            skills: Some(&[]),
            sources: &sources,
            ..Default::default()
        });
        assert!(
            out.contains("read from bundled /opt/bough/skills · user /home/u/.bough/skills"),
            "{out}"
        );
    }

    #[test]
    fn the_legend_is_the_last_row_at_every_height_that_has_one() {
        let skills: Vec<SkillRow> = (0..30)
            .map(|i| skill(&format!("s{i:02}"), "a sentence"))
            .collect();
        for rows in [2usize, 3, 4, 6, 8, 12, 20] {
            let lines = skills_lines(&SkillsTabProps {
                skills: Some(&skills),
                rows,
                selected: 15,
                ..Default::default()
            });
            let last = lines
                .last()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.to_string())
                        .collect::<String>()
                })
                .unwrap_or_default();
            assert!(
                last.contains("esc back"),
                "@{rows} the last row is not the legend: {last}"
            );
            assert!(lines.len() <= rows, "@{rows}: painted {} rows", lines.len());
        }
    }

    #[test]
    fn the_window_follows_the_cursor_rather_than_starting_at_row_one() {
        let (start, height, counter) = skills_window(40, 30, 12, 0);
        assert!(counter);
        assert_eq!(height, 10);
        assert!(
            start > 0 && start + height > 30,
            "the cursor is off screen: {start}..{}",
            start + height
        );
        // A list shorter than the viewport starts at zero and shows no counter.
        let (start, _, counter) = skills_window(3, 0, 12, 0);
        assert_eq!(start, 0);
        assert!(!counter);
    }

    #[test]
    fn a_skill_that_needs_an_mcp_server_says_which_one() {
        let skills = vec![SkillRow {
            name: "todoist".into(),
            description: "manage tasks".into(),
            error: None,
            mcp: vec!["todoist".into()],
        }];
        let out = text(&SkillsTabProps {
            skills: Some(&skills),
            ..Default::default()
        });
        assert!(out.contains("mcp: todoist"), "{out}");
    }

    #[test]
    fn a_filter_that_matches_nothing_says_so_rather_than_claiming_none_are_installed() {
        let out = text(&SkillsTabProps {
            skills: Some(&[]),
            filter: "zzz",
            filtering: true,
            ..Default::default()
        });
        assert!(out.contains("nothing matches that filter"), "{out}");
        assert!(!out.contains("no skills installed"), "{out}");
    }
}
