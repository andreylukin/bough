//! Composer ghost text: the message the user was probably about to type,
//! predicted from the conversation so far (port of `src/worker/ghost.ts`).
//! The second of the cheap tier's three features (spec §12).
//!
//! THE INVARIANT THIS HOLDS: **the ghost is never on a turn's path.** It is
//! fetched by the composer over its own request, resolves to a suggestion or
//! to `None`, and no other surface reads it. That is what makes the feature
//! safe to bill on: the only thing a failed ghost costs is a grey line that
//! does not appear, and the route says so by answering `200 {ghost: null}`
//! rather than an error status (the route itself lives in `bough-server`).
//!
//! WHY IT READS THE THREAD RATHER THAN THE COMPOSER'S PREFIX. A prefix
//! completion needs the user to have started typing, and the moment the
//! suggestion is worth most is the empty composer right after the agent
//! finished — the "and now what" moment. So the prompt is the conversation
//! TAIL, and the typed prefix (when there is one) is an additional constraint
//! on the answer, not the whole of it.
//!
//! WHY LONG LINES KEEP THEIR TAIL. [`render_convo`] truncates from the FRONT,
//! which is backwards from every other truncation in the tree and deliberate:
//! an agent's reply ends with the outcome and what it proposes next, and that
//! ending is the entire signal for predicting the follow-up.

use std::sync::Arc;

use futures::FutureExt as _;

use crate::errors::BoughError;
use crate::schema::parts::{Message, Part, Role};
use crate::types::{CheapTier, SharedDb};
use crate::worker::titles::{cheap_text, CheapCallOpts};

// ---------------------------------------------------------------------------
// Prompt shaping (pure)
// ---------------------------------------------------------------------------

pub const GHOST_SYSTEM: &str =
    "You predict the next message a user will type to their coding agent, given \
     the conversation so far. Reply with that message only: one line, short and \
     concrete — the natural next step (fix what the agent flagged, run the tests, \
     commit, extend the change). No quotes, no explanation, no 'user:' label.";

/// How many trailing turns of context the prediction gets.
pub const MAX_LINES: usize = 8;
/// Per-line budget. Lines longer than this keep their TAIL; see the header.
pub const MAX_LINE_CHARS: usize = 600;
/// The longest suggestion the composer will render as ghost text.
pub const MAX_SUGGESTION: usize = 150;

/// Which side of the conversation a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvoRole {
    User,
    Agent,
}

impl ConvoRole {
    fn as_str(self) -> &'static str {
        match self {
            ConvoRole::User => "user",
            ConvoRole::Agent => "agent",
        }
    }
}

/// One conversation line, already reduced to its text.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvoLine {
    pub role: ConvoRole,
    pub text: String,
}

/// The thread as conversation lines. Pure, and the reason the route needs
/// nothing from the turn runner: a prediction is a function of what is
/// already persisted.
///
/// `system` messages are included as `user` lines because that is exactly how
/// they replay to the model (spec §4) — a detached subagent's report is often
/// the very thing the user's next message is about.
pub fn convo_from(messages: &[Message]) -> Vec<ConvoLine> {
    messages
        .iter()
        .filter_map(|m| {
            let text = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            if text.is_empty() {
                return None;
            }
            let role =
                if m.role == Role::Supervisor { ConvoRole::Agent } else { ConvoRole::User };
            Some(ConvoLine { role, text })
        })
        .collect()
}

/// The conversation tail as prompt text, oldest first.
pub fn render_convo(lines: &[ConvoLine]) -> String {
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..]
        .iter()
        .map(|l| {
            let chars: Vec<char> = l.text.chars().collect();
            let text = if chars.len() > MAX_LINE_CHARS {
                format!("…{}", chars[chars.len() - MAX_LINE_CHARS..].iter().collect::<String>())
            } else {
                l.text.clone()
            };
            format!("{}: {}", l.role.as_str(), text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The full prompt. `prefix` is what the user has already typed; when present
/// the model is told to CONTINUE it, because a suggestion that ignores the
/// half-written sentence in front of the cursor is worse than no suggestion.
pub fn ghost_prompt(lines: &[ConvoLine], prefix: &str) -> String {
    let convo = format!("Conversation, oldest first:\n{}", render_convo(lines));
    let typed = prefix.trim();
    if typed.is_empty() {
        format!("{convo}\n\nThe user's next message:")
    } else {
        format!(
            "{convo}\n\nThe user has started typing: {typed}\n\
             Complete it as the whole next message, starting from what they typed:"
        )
    }
}

/// First real line of the reply, unlabeled, unquoted and capped; `None` if
/// unusable.
pub fn sanitize_suggestion(raw: &str) -> Option<String> {
    static LABEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(user|next|suggestion)\s*:\s*").unwrap()
    });
    static QUOTES_LEAD: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new("^[\"'`]+").unwrap());
    static QUOTES_TRAIL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new("[\"'`]+$").unwrap());
    let line = raw.trim().split('\n').map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let clean = LABEL.replace(line, "");
    let clean = QUOTES_LEAD.replace(&clean, "");
    let clean = QUOTES_TRAIL.replace(&clean, "");
    let clean: String = clean.chars().take(MAX_SUGGESTION).collect();
    let clean = clean.trim();
    if clean.is_empty() { None } else { Some(clean.to_string()) }
}

// ---------------------------------------------------------------------------
// The cheap-tier method
// ---------------------------------------------------------------------------

/// `CheapTier::ghost_text`. Resolves the sanitized suggestion, or `None` —
/// never errors.
///
/// Takes the assembled prompt rather than the thread, so the tier stays a set
/// of three string-in/string-out methods a test can replace with three stubs,
/// and so the shaping above stays pure and directly testable.
pub async fn cheap_ghost(prompt: &str, opts: &CheapCallOpts) -> Option<String> {
    if prompt.trim().is_empty() {
        return None;
    }
    let raw = cheap_text(GHOST_SYSTEM, prompt, 64, opts).await?;
    sanitize_suggestion(&raw)
}

// ---------------------------------------------------------------------------
// The feature
// ---------------------------------------------------------------------------

/// Predict the next message for a session. `None` for an empty conversation,
/// an absent cheap tier, or any cheap-model failure — the three are the same
/// non-answer to the composer.
///
/// Only a database failure errors (that is the route's 500, a real defect); a
/// misbehaving injected tier — even one that panics — is a `None`.
pub async fn ghost_for(
    db: &SharedDb,
    cheap: Option<&Arc<dyn CheapTier>>,
    session_id: &str,
    prefix: &str,
) -> Result<Option<String>, BoughError> {
    let Some(cheap) = cheap else { return Ok(None) };
    let thread = db.lock().unwrap().thread_for(session_id)?;
    let lines = convo_from(&thread);
    if lines.is_empty() {
        return Ok(None);
    }
    let prompt = ghost_prompt(&lines, prefix);
    // The type says this cannot fail; an injected implementation is not bound
    // by the type, and a panicked ghost must not become a 500 on the composer.
    Ok(std::panic::AssertUnwindSafe(cheap.ghost_text(&prompt))
        .catch_unwind()
        .await
        .ok()
        .flatten())
}

// ---------------------------------------------------------------------------
// Tests — ported from src/worker/ghost.test.ts (shaping + ghost_for; the
// route's status-code contract is tested in bough-server::ghost)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::{seed_session, test_db, StubTier};

    fn message(role: Role, parts: Vec<Part>, at: i64) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "s".into(),
            role,
            parts,
            pending: false,
            created_at: at,
        }
    }

    #[test]
    fn render_convo_keeps_the_tail_of_a_long_line_not_the_head() {
        let mut long = "PREAMBLE".to_string();
        while long.chars().count() < MAX_LINE_CHARS + 200 {
            long.push('x');
        }
        long.push_str("THE-CONCLUSION");
        let rendered =
            render_convo(&[ConvoLine { role: ConvoRole::Agent, text: long }]);
        assert!(rendered.ends_with("THE-CONCLUSION"), "the outcome survives");
        assert!(!rendered.contains("PREAMBLE"), "the preamble is what gets dropped");
        assert!(rendered.starts_with("agent: …"));
    }

    #[test]
    fn render_convo_keeps_only_the_last_max_lines_turns_oldest_first() {
        let lines: Vec<ConvoLine> = (0..MAX_LINES + 4)
            .map(|i| ConvoLine {
                role: if i % 2 == 0 { ConvoRole::User } else { ConvoRole::Agent },
                text: format!("line{i}"),
            })
            .collect();
        let rendered: Vec<String> =
            render_convo(&lines).split('\n').map(String::from).collect();
        assert_eq!(rendered.len(), MAX_LINES);
        assert!(rendered[0].ends_with(&format!("line{}", lines.len() - MAX_LINES)));
        assert!(rendered.last().unwrap().ends_with(&format!("line{}", lines.len() - 1)));
    }

    #[test]
    fn a_typed_prefix_becomes_a_continuation_instruction() {
        let lines = [ConvoLine { role: ConvoRole::Agent, text: "done".into() }];
        assert!(ghost_prompt(&lines, "").contains("The user's next message:"));
        let with_prefix = ghost_prompt(&lines, "  run the  ");
        assert!(with_prefix.contains("has started typing: run the"), "{with_prefix}");
        assert!(with_prefix.contains("starting from what they typed"));
    }

    #[test]
    fn convo_from_reduces_a_thread_and_treats_system_notes_as_user_side_text() {
        let messages = vec![
            message(Role::User, vec![Part::Text { text: "go".into() }], 1),
            message(
                Role::Supervisor,
                vec![
                    Part::Reasoning { text: "thinking".into(), meta: None, model: None },
                    Part::Text { text: "done".into() },
                ],
                2,
            ),
            message(
                Role::System,
                vec![Part::Text { text: "[background] bg_1 finished".into() }],
                3,
            ),
            message(Role::Supervisor, vec![], 4),
        ];
        assert_eq!(
            convo_from(&messages),
            vec![
                ConvoLine { role: ConvoRole::User, text: "go".into() },
                // Reasoning is display-only and never reaches a prompt.
                ConvoLine { role: ConvoRole::Agent, text: "done".into() },
                ConvoLine { role: ConvoRole::User, text: "[background] bg_1 finished".into() },
            ]
        );
    }

    #[test]
    fn sanitize_suggestion_unlabels_unquotes_and_caps() {
        assert_eq!(sanitize_suggestion("next: \"run the tests\"").as_deref(), Some("run the tests"));
        assert_eq!(sanitize_suggestion("\n\ncommit it\nand push").as_deref(), Some("commit it"));
        assert_eq!(sanitize_suggestion("   "), None);
        let long = "x".repeat(MAX_SUGGESTION + 50);
        assert_eq!(sanitize_suggestion(&long).unwrap().chars().count(), MAX_SUGGESTION);
    }

    #[tokio::test]
    async fn cheap_ghost_is_none_for_an_empty_prompt_without_calling_anything() {
        struct MustNotBeCalled;
        #[async_trait::async_trait]
        impl crate::types::LlmClient for MustNotBeCalled {
            async fn run(
                &self,
                _p: crate::types::LlmParams,
                _t: crate::types::OnText,
                _c: tokio_util::sync::CancellationToken,
            ) -> Result<crate::types::LlmResult, crate::errors::BoughError> {
                panic!("must not be called")
            }
        }
        let opts =
            CheapCallOpts { llm: Some(Arc::new(MustNotBeCalled)), ..Default::default() };
        assert_eq!(cheap_ghost("  ", &opts).await, None);
    }

    #[tokio::test]
    async fn ghost_for_is_none_when_there_is_no_cheap_tier_at_all() {
        let db = test_db();
        let session_id = seed_session(&db, "");
        assert_eq!(ghost_for(&db, None, &session_id, "").await.unwrap(), None);
    }

    #[tokio::test]
    async fn ghost_for_answers_none_for_an_empty_conversation_and_buys_nothing() {
        let db = test_db();
        let session_id = seed_session(&db, "");
        let tier = Arc::new(StubTier::ghost("nope"));
        let cheap: Arc<dyn CheapTier> = tier.clone();
        assert_eq!(ghost_for(&db, Some(&cheap), &session_id, "").await.unwrap(), None);
        assert_eq!(
            tier.ghost_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "there is nothing to predict from"
        );
    }
}
