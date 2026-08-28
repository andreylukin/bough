//! Invariant: a draft is a CARD in the conversation, where the agent wrote it, and the card
//! offers no send (the TUI brief, D6; track B's rule stands: nothing an agent drafts leaves this
//! machine by itself). The card says what it is, who it was for, that it was NOT sent, and gives
//! Andrey two buttons: `copy` puts it on the clipboard; `open` shows the whole body in place.
//!
//! The drafts PANE this replaces sat above the status line at all times and said "nothing written
//! yet" on every fresh session; the card appears only when there is a draft, and next to the
//! words that produced it.

use bough_plugin_tui_shell::pane::HitId;
use bough_plugin_tui_shell::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::claims::ClaimHit;

pub const HIT_PREFIX: &str = "draft:";

/// How many body lines a closed card shows before `… N more lines (open)`.
pub const CLOSED_BODY_LINES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftAction {
    Copy,
    Open,
}

impl DraftAction {
    pub const ALL: [DraftAction; 2] = [DraftAction::Copy, DraftAction::Open];

    /// The button's word. `open` becomes `close` on an opened card.
    pub fn label(self, opened: bool) -> &'static str {
        match self {
            DraftAction::Copy => "copy",
            DraftAction::Open => {
                if opened {
                    "close"
                } else {
                    "open"
                }
            }
        }
    }

    fn tag(self) -> &'static str {
        match self {
            DraftAction::Copy => "copy",
            DraftAction::Open => "open",
        }
    }
}

/// The clickable region id for one button of one draft: `draft:<id>:<action>`.
pub fn hit_for(draft: &str, action: DraftAction) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{draft}:{}", action.tag()))
}

/// The draft id and action a hit names, or `None` for any other hit.
pub fn action_of_hit(hit: &HitId) -> Option<(String, DraftAction)> {
    let rest = hit.as_str().strip_prefix(HIT_PREFIX)?;
    let (draft, tag) = rest.rsplit_once(':')?;
    let action = match tag {
        "copy" => DraftAction::Copy,
        "open" => DraftAction::Open,
        _ => return None,
    };
    if draft.is_empty() {
        return None;
    }
    Some((draft.to_string(), action))
}

/// What `copy` puts on the clipboard: the draft as it would be sent, headers first.
pub fn copy_text(audience: &str, subject: &str, body: &str) -> String {
    format!("to: {audience}\nsubject: {subject}\n\n{body}")
}

/// PURE: the card's lines and its buttons' regions, `first_line` being where the card starts in
/// the frame.
#[allow(clippy::too_many_arguments)]
pub fn card(
    draft: &str,
    kind: &str,
    audience: &str,
    subject: &str,
    body: &str,
    opened: bool,
    first_line: u16,
    width: u16,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<ClaimHit>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let accent = theme.warn;
    // `✎ draft · ticket  to: linear  <subject>  not sent` — the subject is clipped so that
    // `not sent` is always on screen: it is the card's one sentence that must not scroll away.
    let fixed = "\u{270e} draft \u{b7} ".chars().count()
        + kind.chars().count()
        + 2
        + "to: ".len()
        + audience.chars().count()
        + 2
        + 2
        + "not sent".len();
    let room = (width as usize).saturating_sub(fixed);
    let subject: String = if subject.chars().count() > room {
        if room == 0 {
            String::new()
        } else {
            subject
                .chars()
                .take(room.saturating_sub(1))
                .collect::<String>()
                + "\u{2026}"
        }
    } else {
        subject.to_string()
    };
    lines.push(Line::from(vec![
        Span::styled("\u{270e} draft \u{b7} ", Style::default().fg(accent)),
        Span::styled(
            kind.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("to: {audience}"), Style::default().fg(theme.dim)),
        Span::raw("  "),
        Span::styled(subject.to_string(), Style::default().fg(theme.fg)),
        Span::raw("  "),
        Span::styled("not sent", Style::default().fg(accent)),
    ]));
    let wrapped = bough_plugin_tui_render::wrap(body, width.saturating_sub(2));
    let shown = if opened {
        wrapped.len()
    } else {
        wrapped.len().min(CLOSED_BODY_LINES)
    };
    for l in wrapped.iter().take(shown) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(l.clone(), Style::default().fg(theme.fg)),
        ]));
    }
    if shown < wrapped.len() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("\u{2026} {} more lines (open)", wrapped.len() - shown),
                Style::default().fg(theme.dim),
            ),
        ]));
    }
    let mut hits = Vec::new();
    let mut spans = vec![Span::raw("  ")];
    let mut x: u16 = 2;
    for action in DraftAction::ALL {
        let label = action.label(opened);
        hits.push(ClaimHit {
            id: hit_for(draft, action),
            line: first_line + lines.len() as u16,
            x,
            width: label.len() as u16,
        });
        spans.push(Span::styled(label, Style::default().fg(theme.interactive)));
        spans.push(Span::raw(" "));
        x += label.len() as u16 + 1;
    }
    lines.push(Line::from(spans));
    (lines, hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_tui_shell::ThemeName;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    }

    #[test]
    fn a_hit_round_trips_through_its_id() {
        let hit = hit_for("d-7", DraftAction::Open);
        assert_eq!(
            action_of_hit(&hit),
            Some(("d-7".to_string(), DraftAction::Open))
        );
        assert_eq!(action_of_hit(&HitId::new("tool:x")), None);
        assert_eq!(action_of_hit(&HitId::new("draft::copy")), None);
    }

    #[test]
    fn the_card_says_what_it_is_that_it_was_not_sent_and_folds_a_long_body() {
        let theme = Theme::of(ThemeName::Dark);
        let body = (1..=8)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (closed, hits) = card(
            "d-7",
            "ticket",
            "linear",
            "Flaky test",
            &body,
            false,
            10,
            60,
            &theme,
        );
        let t = text(&closed);
        assert!(t[0].contains("draft \u{b7} ticket") && t[0].contains("to: linear"));
        assert!(
            t[0].contains("Flaky test") && t[0].contains("not sent"),
            "{t:?}"
        );
        assert!(t.iter().any(|l| l.contains("4 more lines (open)")), "{t:?}");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].line,
            10 + closed.len() as u16 - 1,
            "buttons on the last line"
        );
        assert!(t.last().unwrap().contains("copy") && t.last().unwrap().contains("open"));
        let (opened, _) = card(
            "d-7",
            "ticket",
            "linear",
            "Flaky test",
            &body,
            true,
            10,
            60,
            &theme,
        );
        let t = text(&opened);
        assert!(t.iter().any(|l| l.contains("line 8")), "{t:?}");
        assert!(!t.iter().any(|l| l.contains("more lines")), "{t:?}");
        assert!(t.last().unwrap().contains("close"), "{t:?}");
        // A long subject is clipped so `not sent` stays on the line.
        let long = "a subject that goes on far longer than any card header could hold at all";
        let (narrow, _) = card("d-7", "ticket", "linear", long, "b", false, 0, 60, &theme);
        let head = &text(&narrow)[0];
        assert!(head.chars().count() <= 60, "{head:?}");
        assert!(
            head.ends_with("not sent") && head.contains('\u{2026}'),
            "{head:?}"
        );
        assert_eq!(
            copy_text("linear", "Flaky test", "body"),
            "to: linear\nsubject: Flaky test\n\nbody"
        );
    }
}
