//! Invariant: a claim card is drawn from the LEDGER BODY, by step-type name (P3-D11) — this
//! module knows nothing of `bough-plugin-claims`. And ACCEPTANCE IS ANDREY'S ACT (§16, P5-D16):
//! the card's three hit regions do not decide anything themselves. A click turns into the same
//! `/accept`, `/edit`, `/reject` line the keyboard path types, dispatched through the shell's
//! command seam, so the click path and the keyboard path cannot drift apart — and a build with no
//! `claims` row mounted simply has no such command, which is what "read-only" means here.

use bough_plugin_tui_shell::pane::HitId;
use bough_plugin_tui_shell::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::rows::ClaimState;

/// The prefix a claim card's clickable regions are minted under, spelled once.
pub const HIT_PREFIX: &str = "claim:";

/// What one of a card's three regions asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimAction {
    Accept,
    Edit,
    Reject,
}

impl ClaimAction {
    /// The label drawn on the button.
    pub fn label(&self) -> &'static str {
        match self {
            ClaimAction::Accept => "[accept]",
            ClaimAction::Edit => "[edit]",
            ClaimAction::Reject => "[reject]",
        }
    }

    fn word(&self) -> &'static str {
        match self {
            ClaimAction::Accept => "accept",
            ClaimAction::Edit => "edit",
            ClaimAction::Reject => "reject",
        }
    }

    /// The three, in drawn order.
    pub const ALL: [ClaimAction; 3] = [ClaimAction::Accept, ClaimAction::Edit, ClaimAction::Reject];
}

/// The clickable region id for one action on one claim.
pub fn hit_for_claim(claim: &str, action: ClaimAction) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{claim}:{}", action.word()))
}

/// PURE: a clicked region ⇒ the claim and the action. `None` for a region this pane did not mint.
pub fn claim_action_of_hit(hit: &HitId) -> Option<(String, ClaimAction)> {
    let rest = hit.as_str().strip_prefix(HIT_PREFIX)?;
    // The claim id is opaque and may itself contain `:`, so the ACTION is split off the end.
    let (claim, word) = rest.rsplit_once(':')?;
    if claim.is_empty() {
        return None;
    }
    let action = match word {
        "accept" => ClaimAction::Accept,
        "edit" => ClaimAction::Edit,
        "reject" => ClaimAction::Reject,
        _ => return None,
    };
    Some((claim.to_string(), action))
}

/// One drawn button: where it is, and what it means.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimHit {
    pub id: HitId,
    /// The card-relative line index within the frame's line list.
    pub line: u16,
    /// The column the button starts at, and how wide it is.
    pub x: u16,
    pub width: u16,
}

/// PURE: one claim card ⇒ its lines and its hit regions. An OPEN card draws three regions; a
/// decided one draws none — there is nothing left to decide.
#[allow(clippy::too_many_arguments)]
pub fn card(
    claim: &str,
    kind: &str,
    title: &str,
    body: &str,
    state: &ClaimState,
    first_line: u16,
    width: u16,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<ClaimHit>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let accent = match state {
        ClaimState::Open => theme.warn,
        ClaimState::Accepted { .. } => theme.evidence,
        ClaimState::Rejected { .. } => theme.dim,
    };
    lines.push(Line::from(vec![
        Span::styled("◇ claim ", Style::default().fg(accent)),
        Span::styled(
            kind.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(title.to_string(), Style::default().fg(theme.fg)),
        Span::raw("  "),
        Span::styled(state.word().to_string(), Style::default().fg(accent)),
    ]));
    for l in bough_plugin_tui_render::wrap(body, width.saturating_sub(2)) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(l, Style::default().fg(theme.fg)),
        ]));
    }
    if let ClaimState::Rejected { reason } = state {
        // The reason is the whole point of a rejection: a rejected card that did not say why
        // would leave the agent's proposal looking arbitrary.
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("rejected: {reason}"),
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }
    let mut hits = Vec::new();
    if state.is_open() {
        let mut spans = vec![Span::raw("  ")];
        let mut x: u16 = 2;
        for action in ClaimAction::ALL {
            let label = action.label();
            hits.push(ClaimHit {
                id: hit_for_claim(claim, action),
                line: first_line + lines.len() as u16,
                x,
                width: label.len() as u16,
            });
            // A button is a thing you CLICK: the interactive role (visual audit F5).
            spans.push(Span::styled(label, Style::default().fg(theme.interactive)));
            spans.push(Span::raw(" "));
            x += label.len() as u16 + 1;
        }
        lines.push(Line::from(spans));
    }
    (lines, hits)
}

/// PURE: an action on a claim ⇒ the line the shell dispatches, and whether it is a COMMAND (run
/// now) or a COMPOSE (put in the composer for Andrey to finish). `/edit` needs the new text and
/// `/reject` needs a reason, so both are composed rather than run.
pub fn line_for(claim: &str, action: ClaimAction, body: &str) -> (String, bool) {
    match action {
        ClaimAction::Accept => (format!("/accept {claim}"), true),
        ClaimAction::Edit => (format!("/edit {claim} {body}"), false),
        ClaimAction::Reject => (format!("/reject {claim} "), false),
    }
}
