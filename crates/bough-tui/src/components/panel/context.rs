//! The context tab: what the last turn actually put in the window.
//!
//! THE QUESTION THIS ANSWERS, and the reason it is a tab rather than a notice:
//! "what is in my context, and what is it costing me". The harness has always
//! known — every turn assembles a prompt that reports its own sections with a
//! length each — and the user was shown one percentage in the status bar. A
//! rules file that quietly doubled, a skill catalog that grew with every skill
//! installed, a directory picked up mid-session: all of it was invisible, and
//! all of it is paid for on every uncached turn.
//!
//! THE TIERS ARE THE POINT. A stable section is byte-identical across sessions
//! and shared in the provider's prompt cache; a volatile one belongs to this
//! session and is re-sent whenever the cache misses. Showing one flat list of
//! sizes would send a reader to shorten the wrong thing — the big stable
//! sections are the cheap ones. So the two tiers are separate blocks with
//! separate totals, and the volatile block leads, because that is the half the
//! user can actually do something about.
//!
//! `None` IS NOT EMPTY, the same rule the skills and hooks tabs hold. A server
//! that has not run a turn for this session since it started knows nothing
//! about its prompt, and saying "0 sections" would be a claim rather than an
//! absence.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::api::{ProjectRuleSummary, PromptView};
use crate::components::panel::{legend_line, paint_rows};
use crate::components::{accent, error, info, warn};

/// What the tab needs. Borrowed, like every other tab's props.
pub struct ContextProps<'a> {
    pub view: Option<&'a PromptView>,
    /// Why there is nothing to show, when the fetch itself failed.
    pub note: Option<&'a str>,
    pub height: usize,
    pub cols: usize,
}

/// A byte count as a token estimate. Four characters to a token is the usual
/// English approximation and is stated as approximate everywhere it is shown —
/// the honest alternative is a tokenizer per provider in a panel renderer.
fn tokens(bytes: usize) -> String {
    let t = bytes / 4;
    if t >= 1000 {
        format!("{:.1}k", t as f64 / 1000.0)
    } else {
        format!("{t}")
    }
}

/// The section ids the user reads as one line item, in the order they are
/// worth reading. Anything not named here is folded into the stable total —
/// eighteen prose sections listed one per row is a wall, not an answer.
fn label_for(id: &str) -> Option<&'static str> {
    Some(match id {
        "notes" => "notes (rules · workspace · tags)",
        "skill-catalog" => "skills available",
        "skills" => "skill bodies (invoked)",
        "mcp-tools" => "mcp tools",
        "extensions" => "extensions",
        _ => return None,
    })
}

/// Render the tab. Never emits more rows than `height` — the row budget rule
/// stated in `panel/mod.rs`.
pub fn render(props: &ContextProps, area: Rect, buf: &mut Buffer) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut rows: Vec<Line<'static>> = Vec::new();

    let Some(view) = props.view else {
        rows.push(Line::from(Span::styled(
            props
                .note
                .unwrap_or("reading what the last turn sent…")
                .to_string(),
            if props.note.is_some() {
                Style::default().fg(error())
            } else {
                dim
            },
        )));
        paint_rows(&rows, area, buf);
        return;
    };

    // ---- the meter ---------------------------------------------------------
    if let Some(used) = view.context_tokens.filter(|t| *t > 0) {
        let mut spans = vec![Span::styled(
            format!("{} in the window", crate::format::fmt_tokens(used)),
            bold,
        )];
        if let Some(limit) = view.context_limit.filter(|l| *l > 0) {
            spans.push(Span::styled(
                format!(" / {}", crate::format::fmt_tokens(limit)),
                dim,
            ));
        }
        if let Some(cached) = view.cached_tokens.filter(|t| *t > 0) {
            spans.push(Span::styled(
                format!("  ⚡ {} cached", crate::format::fmt_tokens(cached)),
                Style::default().fg(info()),
            ));
        }
        rows.push(Line::from(spans));
        rows.push(Line::from(""));
    }

    let Some(shape) = view.shape.as_ref() else {
        // The honest empty state: a turn has not run HERE, which is different
        // from a prompt with nothing in it.
        rows.push(Line::from(Span::styled(
            "no turn has run in this server process yet — send a message and this fills in"
                .to_string(),
            dim,
        )));
        paint_rows(&rows, area, buf);
        return;
    };

    // ---- volatile first: the half the user can change ----------------------
    rows.push(Line::from(vec![
        Span::styled("THIS SESSION ".to_string(), bold),
        Span::styled(
            format!("~{} tok", tokens(shape.volatile_bytes)),
            Style::default().fg(accent()),
        ),
        Span::styled("  re-sent whenever the cache misses".to_string(), dim),
    ]));
    for section in shape.sections.iter().filter(|s| label_for(&s.id).is_some()) {
        let label = label_for(&section.id).unwrap_or(&section.id);
        rows.push(Line::from(vec![
            Span::styled(format!("  {label:<34}"), Style::default()),
            Span::styled(format!("~{:>6} tok", tokens(section.bytes)), dim),
        ]));
    }

    // The rules block, named. A size with no filename beside it is not
    // something anybody can act on, and the rules note is almost always the
    // largest line item above.
    for rule in &view.project_rules {
        rows.push(rule_row(rule, dim));
    }
    if !view.worked_in.is_empty() {
        rows.push(Line::from(Span::styled(
            format!(
                "    picked up from {} worked in this session",
                crate::store::selectors::plural(view.worked_in.len() as i64, "directory")
            ),
            dim,
        )));
    }
    // A merged block names its sources on screen. This is the whole answer to
    // "the model is following rules I never wrote": it did not — it is
    // following every line of both files, and here are the two files.
    for rule in view
        .project_rules
        .iter()
        .filter(|r| r.merged_from.len() > 1)
    {
        rows.push(Line::from(Span::styled(
            format!(
                "  ⧉ merged: {} — one copy of what they share, both of what they don't",
                rule.merged_from
                    .iter()
                    .map(|p| short(p))
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
            Style::default().fg(warn()),
        )));
    }

    // ---- stable: named, totalled, not enumerated ---------------------------
    let stable = shape.sections.iter().filter(|s| label_for(&s.id).is_none());
    let count = stable.clone().count();
    rows.push(Line::from(""));
    rows.push(Line::from(vec![
        Span::styled("SHARED ".to_string(), bold),
        Span::styled(format!("~{} tok", tokens(shape.stable_bytes)), dim),
        Span::styled(
            format!("  {count} sections · cached across every session"),
            dim,
        ),
    ]));
    let names: Vec<String> = stable.map(|s| s.id.clone()).collect();
    for line in wrap(&names.join(" · "), props.cols.saturating_sub(4).max(20)) {
        rows.push(Line::from(Span::styled(format!("  {line}"), dim)));
    }

    rows.push(Line::from(Span::styled(
        legend_line(
            &["↑↓ move", "^t close"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            Some(props.cols),
        ),
        dim,
    )));
    paint_rows(&rows, area, buf);
}

/// One rule file's row, flagged when it is half of a near-duplicate pair.
fn rule_row(rule: &ProjectRuleSummary, dim: Style) -> Line<'static> {
    let flagged = rule.merged_from.len() > 1;
    Line::from(vec![
        Span::styled(
            format!("    {:<32}", clip_label(&rule.label)),
            if flagged {
                Style::default().fg(warn())
            } else {
                dim
            },
        ),
        Span::styled(format!("{:>8} B", rule.bytes), dim),
    ])
}

fn clip_label(label: &str) -> String {
    if label.chars().count() <= 32 {
        label.to_string()
    } else {
        let tail: String = label
            .chars()
            .rev()
            .take(29)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    }
}

/// The last two path segments — enough to tell `.claude/CLAUDE.md` from
/// `.bough/AGENTS.md`, which is the whole job of this string.
fn short(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Wrap on separators, never mid-word: this is a list of ids, and half an id
/// on each of two rows names nothing.
fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for part in text.split(" · ") {
        if !line.is_empty() && line.chars().count() + part.chars().count() + 3 > cols {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push_str(" · ");
        }
        line.push_str(part);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{PromptSection, PromptShape};

    fn view() -> PromptView {
        PromptView {
            shape: Some(PromptShape {
                sections: vec![
                    PromptSection {
                        id: "identity".into(),
                        sha: "a".into(),
                        bytes: 4000,
                    },
                    PromptSection {
                        id: "shell".into(),
                        sha: "b".into(),
                        bytes: 8000,
                    },
                    PromptSection {
                        id: "skill-catalog".into(),
                        sha: "c".into(),
                        bytes: 8661,
                    },
                    PromptSection {
                        id: "notes".into(),
                        sha: "d".into(),
                        bytes: 17000,
                    },
                ],
                stable_bytes: 12000,
                volatile_bytes: 25661,
            }),
            project_rules: vec![ProjectRuleSummary {
                label: ".bough/AGENTS.md".into(),
                path: "/h/.bough/AGENTS.md".into(),
                bytes: 9024,
                merged_from: vec!["/h/.claude/CLAUDE.md".into(), "/h/.bough/AGENTS.md".into()],
            }],
            worked_in: vec!["/h/repos/thing".into()],
            context_tokens: Some(77344),
            cached_tokens: Some(38612),
            context_limit: Some(1_050_000),
        }
    }

    fn painted(props: &ContextProps) -> String {
        let area = Rect::new(0, 0, 80, props.height as u16);
        let mut buf = Buffer::empty(area);
        render(props, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The whole point: the sizes are visible, and the near-duplicate the user
    /// could not have found by hand is named on screen.
    #[test]
    fn the_tab_names_each_cost_and_shows_what_was_merged() {
        let v = view();
        let text = painted(&ContextProps {
            view: Some(&v),
            note: None,
            height: 24,
            cols: 80,
        });
        assert!(text.contains("77k in the window"), "{text}");
        assert!(text.contains("⚡"), "{text}");
        // Named line items with sizes, not one opaque total.
        assert!(text.contains("skills available"), "{text}");
        assert!(text.contains("2.2k tok"), "{text}");
        // The merged block names both sources on screen — the audit trail for
        // a document the user did not write by hand.
        assert!(text.contains(".bough/AGENTS.md"), "{text}");
        assert!(text.contains("⧉ merged:"), "{text}");
        assert!(text.contains(".claude/CLAUDE.md"), "{text}");
        // The shared tier is totalled, not enumerated one row per section.
        assert!(text.contains("SHARED"), "{text}");
        assert!(text.contains("identity · shell"), "{text}");
    }

    /// `None` is not empty, and the two absences read differently.
    #[test]
    fn a_session_with_no_turn_yet_says_so_rather_than_claiming_an_empty_prompt() {
        let mut v = view();
        v.shape = None;
        let text = painted(&ContextProps {
            view: Some(&v),
            note: None,
            height: 12,
            cols: 80,
        });
        assert!(text.contains("no turn has run"), "{text}");
        assert!(!text.contains("SHARED"), "{text}");

        let failed = painted(&ContextProps {
            view: None,
            note: Some("could not reach the server"),
            height: 6,
            cols: 80,
        });
        assert!(failed.contains("could not reach the server"), "{failed}");
    }

    /// The row budget rule from `panel/mod.rs`: a short panel truncates and
    /// never paints past its box.
    #[test]
    fn the_body_never_paints_more_rows_than_its_budget() {
        let v = view();
        for height in [3usize, 6, 10] {
            let text = painted(&ContextProps {
                view: Some(&v),
                note: None,
                height,
                cols: 80,
            });
            assert!(text.lines().count() <= height, "height {height}: {text}");
        }
    }

    #[test]
    fn ids_wrap_on_the_separator_never_mid_word() {
        let wrapped = wrap("identity · shell · files · searching · ending", 22);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|l| !l.ends_with(" ·")));
        assert_eq!(
            wrapped.join(" · "),
            "identity · shell · files · searching · ending"
        );
    }
}
