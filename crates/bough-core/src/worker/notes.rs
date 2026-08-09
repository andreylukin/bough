//! The automatic Log line: the cheap tier's fourth feature.
//!
//! WHAT IT IS ASKED. After a round that touched a reference, the cheap model
//! sees the last few Log lines, this round's references as `tag → exit code`,
//! and the user's message — and answers with ONE short line, or `SKIP`.
//!
//! WHAT IT IS NEVER SHOWN: command strings and `output_head`. Those live in
//! `command_history`, which is canonical; feeding them back is how the note
//! memory would become a lossy second copy of the memory it sits beside. The
//! restriction is enforced in [`round_gist`], which has no parameter a command
//! could arrive through, and pinned by
//! `the_model_is_never_shown_a_command_string`.
//!
//! SKIP IS THE DEFAULT, and the prompt says so twice. Most rounds are `git
//! status` and `rg` and are worth nothing to a future session; a tier that
//! writes a line per round produces forty lines of noise per ticket and buries
//! the two that mattered.
//!
//! WHY IT MAY NOT RESOLVE A CONTRADICTION. A weak model doing arbitration
//! moves the failure one layer down, where the loss is silent: the old claim
//! is gone and nothing records that a judgment was made. So this module can
//! only ever APPEND to the derived zone, and [`cheap_contradiction`] can only
//! ever RAISE a warning — clearing one needs the session model or a human.
//! That asymmetry is the whole trust model.

use crate::notes::MAX_LINE_CHARS;
use crate::types::{NoteLogRow, NoteRow};
use crate::worker::titles::{cheap_text, CheapCallOpts};

// ---------------------------------------------------------------------------
// Prompt shaping (pure)
// ---------------------------------------------------------------------------

pub const NOTE_SYSTEM: &str =
    "You maintain a short engineering log about one topic. Given what a coding session just \
     did, reply with ONE line of at most 20 words recording what was LEARNED or DECIDED — \
     an outcome, a blocker, a decision and its reason. \
     Reply with exactly SKIP when the round taught nothing worth keeping, which is most \
     rounds: routine inspection, searching, status checks, or work still in progress. \
     Never write a command, a file path, an exit code, or a number of attempts. \
     Never repeat a line already in the log. No quotes, no bullet, no preamble, no period.";

/// What the model may answer to say "nothing happened". Verbatim: the parser
/// checks for it, and a synonym is treated as a line, so the prompt names one
/// word and the check accepts only that word.
pub const SKIP: &str = "SKIP";

/// Log lines the model is shown. Enough to avoid repeating itself, few enough
/// that the prompt stays small on a page near its cap.
pub const CONTEXT_LINES: usize = 10;

/// One reference this round touched, and how it went.
#[derive(Clone, Debug, PartialEq)]
pub struct RoundTag {
    pub tag: String,
    /// None = still running when the turn moved on.
    pub exit_code: Option<i64>,
}

/// The prompt body. **Takes no command string by construction** — the whole
/// non-duplication invariant, expressed as a function signature.
pub fn round_gist(
    note: &NoteRow,
    claim: &str,
    log: &[NoteLogRow],
    tags: &[RoundTag],
    user_message: &str,
) -> String {
    let mut out = format!("Topic: {}\n", note.path);
    if !claim.trim().is_empty() {
        let body: String = claim.trim().chars().take(600).collect();
        out.push_str(&format!("\nWhat the note already claims:\n{body}\n"));
    }
    let recent: Vec<&str> = log
        .iter()
        .rev()
        .take(CONTEXT_LINES)
        .map(|l| l.text.as_str())
        .collect();
    if !recent.is_empty() {
        out.push_str("\nAlready logged (do not repeat):\n");
        for line in recent.iter().rev() {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out.push_str("\nThis round:\n");
    for t in tags {
        let outcome = match t.exit_code {
            Some(0) => "worked",
            Some(_) => "failed",
            None => "still running",
        };
        out.push_str(&format!("- {} — {outcome}\n", t.tag));
    }
    let asked: String = user_message.trim().chars().take(400).collect();
    if !asked.is_empty() {
        out.push_str(&format!("\nWhat was asked:\n{asked}\n"));
    }
    out.push_str("\nOne line, or SKIP.");
    out
}

/// First real line, unquoted, de-bulleted, capped; `None` for SKIP, an empty
/// answer, or anything that is not a single line of prose.
///
/// A multi-sentence answer is REFUSED rather than truncated: a model that
/// ignored "one line" also ignored "what was learned", and half of a wrong
/// answer is still wrong.
pub fn sanitize_line(raw: &str) -> Option<String> {
    let line = raw
        .trim()
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line
        .trim_start_matches(['-', '*', '+', '•'])
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ')'])
        .trim();
    let line = line.trim_matches(['"', '\'', '`']).trim();
    if line.is_empty() || line.eq_ignore_ascii_case(SKIP) {
        return None;
    }
    // A refusal or an explanation, not a log line.
    if line.len() > MAX_LINE_CHARS * 2 {
        return None;
    }
    let line = line.trim_end_matches('.').trim();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(MAX_LINE_CHARS).collect())
    }
}

// ---------------------------------------------------------------------------
// The cheap-tier methods
// ---------------------------------------------------------------------------

/// `CheapTier::note_line`. Resolves the sanitized line, or `None` — never
/// errors, never logs, and `None` is by far the commonest answer.
pub async fn cheap_note_line(prompt: &str, opts: &CheapCallOpts) -> Option<String> {
    if prompt.trim().is_empty() {
        return None;
    }
    let raw = cheap_text(NOTE_SYSTEM, prompt, 64, opts).await?;
    sanitize_line(&raw)
}

pub const CONTRADICTION_SYSTEM: &str =
    "You check whether a log contradicts a claim. Given a claim and recent log lines, reply \
     with exactly NO when nothing in the log contradicts the claim — which is the usual \
     answer. Only when a log line makes the claim FALSE, reply with one sentence of at most \
     25 words naming what changed. Never rewrite the claim. Never guess: silence about a \
     topic is not a contradiction.";

/// The prompt for a contradiction check.
pub fn contradiction_gist(claim: &str, log: &[NoteLogRow]) -> String {
    let claim: String = claim.trim().chars().take(800).collect();
    let lines: Vec<String> = log
        .iter()
        .rev()
        .take(CONTEXT_LINES)
        .map(|l| format!("- {}", l.text))
        .collect();
    format!(
        "Claim:\n{claim}\n\nRecent log:\n{}\n\nNO, or one sentence.",
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    )
}

/// `CheapTier::note_contradiction`. `Some(reason)` means RAISE A WARNING — it
/// never means edit anything.
pub async fn cheap_contradiction(prompt: &str, opts: &CheapCallOpts) -> Option<String> {
    if prompt.trim().is_empty() {
        return None;
    }
    let raw = cheap_text(CONTRADICTION_SYSTEM, prompt, 64, opts).await?;
    let line = sanitize_line(&raw)?;
    if line.eq_ignore_ascii_case("no") || line.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(line)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NoteAuthor, NoteLogRow, NoteRow};

    fn note() -> NoteRow {
        NoteRow {
            id: 1,
            path: "linear.nme-1673".into(),
            title: "NASED executor removal".into(),
            tags: vec!["linear.nme-1673".into()],
            created_at: 0,
            updated_at: 0,
            synced_ts: 0,
            closed_at: None,
        }
    }

    fn log(lines: &[&str]) -> Vec<NoteLogRow> {
        lines
            .iter()
            .enumerate()
            .map(|(i, t)| NoteLogRow {
                id: i as i64,
                ts: i as i64,
                source: NoteAuthor::Cheap,
                text: t.to_string(),
            })
            .collect()
    }

    #[test]
    fn the_model_is_never_shown_a_command_string() {
        // The invariant, at the only place it could be broken. `round_gist`
        // takes tags and exit codes; there is no parameter a command could
        // ride in, and this test is what should fail if one is added.
        let gist = round_gist(
            &note(),
            "DAG removal lands before the executor swap.",
            &log(&["dev quiesce merged"]),
            &[RoundTag {
                tag: "nased".into(),
                exit_code: Some(0),
            }],
            "roll out the executor change",
        );
        for leaked in ["kubectl", "git ", "bash", "exit_code", "output_head", "$"] {
            assert!(!gist.contains(leaked), "{leaked} reached the cheap model");
        }
        assert!(gist.contains("nased — worked"));
        assert!(gist.contains("roll out the executor change"));
        assert!(gist.contains("do not repeat"));
        assert!(gist.contains("dev quiesce merged"));
    }

    #[test]
    fn an_outcome_word_replaces_the_exit_code() {
        // A path with no digits, so a stray `3` can only have come from the
        // exit code this test is about.
        let plain = NoteRow {
            path: "nased".into(),
            tags: vec!["nased".into()],
            ..note()
        };
        let gist = round_gist(
            &plain,
            "",
            &[],
            &[
                RoundTag {
                    tag: "a".into(),
                    exit_code: Some(0),
                },
                RoundTag {
                    tag: "b".into(),
                    exit_code: Some(3),
                },
                RoundTag {
                    tag: "c".into(),
                    exit_code: None,
                },
            ],
            "",
        );
        assert!(gist.contains("a — worked"));
        assert!(gist.contains("b — failed"));
        assert!(gist.contains("c — still running"));
        assert!(
            !gist.contains('3'),
            "the code itself is not the model's business"
        );
    }

    #[test]
    fn skip_is_none_in_every_spelling_the_prompt_allows() {
        assert_eq!(sanitize_line("SKIP"), None);
        assert_eq!(sanitize_line("skip"), None);
        assert_eq!(sanitize_line("  SKIP  "), None);
        assert_eq!(sanitize_line(""), None);
        assert_eq!(sanitize_line("\n\n"), None);
    }

    #[test]
    fn decoration_is_stripped_and_a_lecture_is_refused() {
        assert_eq!(
            sanitize_line("- cutover blocked on the backfill window."),
            Some("cutover blocked on the backfill window".into())
        );
        assert_eq!(
            sanitize_line("\"prod otel rollout green\""),
            Some("prod otel rollout green".into())
        );
        assert_eq!(
            sanitize_line("1. dag removal must land first"),
            Some("dag removal must land first".into())
        );
        assert_eq!(
            sanitize_line(&"a very long explanation ".repeat(40)),
            None,
            "a model that ignored 'one line' also ignored the rest of the prompt"
        );
    }

    #[test]
    fn a_line_is_capped_at_the_stores_limit() {
        let long = "x".repeat(MAX_LINE_CHARS + 40);
        assert_eq!(
            sanitize_line(&long).unwrap().chars().count(),
            MAX_LINE_CHARS
        );
    }

    #[test]
    fn a_contradiction_check_defaults_to_silence() {
        assert_eq!(sanitize_line("NO"), Some("NO".into()));
        let gist = contradiction_gist(
            "DAG removal lands first.",
            &log(&["the cutover merged green"]),
        );
        assert!(gist.contains("Claim:"));
        assert!(gist.contains("the cutover merged green"));
        assert!(gist.contains("NO, or one sentence"));
    }

    #[test]
    fn the_prompts_say_skip_is_the_usual_answer() {
        // Measured behavior, not taste: a tier that writes on every round
        // buries the two lines per ticket that mattered.
        assert!(NOTE_SYSTEM.contains("which is most"));
        assert!(CONTRADICTION_SYSTEM.contains("which is the usual"));
        assert!(NOTE_SYSTEM.contains("Never write a command"));
        assert!(CONTRADICTION_SYSTEM.contains("Never rewrite the claim"));
    }
}
