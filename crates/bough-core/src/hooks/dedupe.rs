//! Say it once: repeated injected text becomes a reference to itself.
//!
//! THE PROBLEM THIS EXISTS FOR, which the bundled adapters create. A
//! `TurnStart` hook that injects `CLAUDE.md` injects it EVERY turn. Nothing is
//! wrong with any one of those injections; the twentieth is four kilobytes of
//! bytes the model has already read, in a window it is paying for, and a
//! document repeated twenty times reads as twenty times more important than
//! one stated once.
//!
//! So injected text is remembered per session by DIGEST, and a repeat becomes
//! one line naming it. The content is not lost — it is above, in the same
//! conversation, which is precisely why repeating it adds nothing.
//!
//! ## Only when the reference is smaller
//!
//! A reference is itself text. Replacing `use tabs` with "(the same 47
//! characters as before)" makes the window BIGGER and the sentence harder to
//! read. So the substitution happens only when it saves bytes, which for short
//! blocks it never does. That is the whole rule, and it is why this is a
//! function that returns text rather than a flag that suppresses it.
//!
//! ## Digest, not equality of source
//!
//! Keyed on the CONTENT, so a file that changed says itself again — which is
//! the behaviour you want from a rules file someone is editing — and two
//! sources that happen to inject the same text collapse into one.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

/// How much of the first line names the block in a reference.
const LABEL_CHARS: usize = 72;

/// Per session, the digests already injected and what the first line was.
type Seen = HashMap<String, HashMap<String, String>>;

fn seen() -> &'static Mutex<Seen> {
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// The first meaningful line, clipped — what a reference names the block by.
///
/// Markdown headings and comment markers are stripped, because "## CLAUDE.md"
/// and "# CLAUDE.md" name the same document and the punctuation is not what
/// the reader needs.
fn label(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', '-', '/', '*', ' '])
        .trim();
    if line.chars().count() <= LABEL_CHARS {
        return line.to_string();
    }
    format!(
        "{}…",
        line.chars().take(LABEL_CHARS - 1).collect::<String>()
    )
}

/// What to inject for `text` in this session: the text itself the first time,
/// a reference to it after that — and the text again whenever the reference
/// would be the longer of the two.
pub fn once_per_session(session_id: &str, text: &str) -> String {
    let key = digest(text);
    let mut guard = match seen().lock() {
        Ok(g) => g,
        // A poisoned lock must not cost the injection; saying it twice is the
        // failure mode this module exists to reduce, not one it may cause.
        Err(e) => e.into_inner(),
    };
    let session = guard.entry(session_id.to_string()).or_default();
    let Some(known) = session.get(&key) else {
        session.insert(key, label(text));
        return text.to_string();
    };
    let reference = if known.is_empty() {
        "[unchanged since earlier in this conversation]".to_string()
    } else {
        format!("[{known} — unchanged since earlier in this conversation]")
    };
    // The rule: a reference that is not smaller is not a saving.
    if reference.len() < text.len() {
        reference
    } else {
        text.to_string()
    }
}

/// Drop exact repeats WITHIN one batch, keeping the first.
///
/// The other half of the rule, and the one that applies to text bound for the
/// SYSTEM PROMPT. That prompt is rebuilt every turn, so last turn's copy is
/// not above anything — a cross-turn reference would point at bytes the model
/// can no longer see, which is worse than the repetition it saves. Inside one
/// turn, though, two hooks injecting the same document is still one document.
pub fn within_batch(texts: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for text in texts {
        let key = digest(text);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(text.clone());
    }
    out
}

/// Forget one session's injections. Called when a session is reset so a fresh
/// thread starts from nothing — the reference would otherwise point at
/// content the model can no longer see.
pub fn forget(session_id: &str) {
    if let Ok(mut guard) = seen().lock() {
        guard.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn the_first_time_is_the_whole_thing_and_the_second_is_a_reference() {
        let s = session();
        let doc = format!("# CLAUDE.md\n\n{}", "always mention the moon. ".repeat(20));
        assert_eq!(once_per_session(&s, &doc), doc, "first time, in full");

        let second = once_per_session(&s, &doc);
        assert_ne!(second, doc);
        assert!(
            second.contains("CLAUDE.md"),
            "the reference NAMES it: {second}"
        );
        assert!(second.contains("unchanged"), "{second}");
        assert!(second.len() < doc.len() / 4);
    }

    #[test]
    fn short_text_is_repeated_because_a_reference_to_it_would_be_longer() {
        let s = session();
        let short = "use tabs";
        assert_eq!(once_per_session(&s, short), short);
        assert_eq!(
            once_per_session(&s, short),
            short,
            "a reference longer than the text is not a saving"
        );
    }

    #[test]
    fn changed_content_says_itself_again() {
        let s = session();
        let before = format!("# rules\n\n{}", "x".repeat(200));
        let after = format!("# rules\n\n{}", "y".repeat(200));
        assert_eq!(once_per_session(&s, &before), before);
        assert_eq!(
            once_per_session(&s, &after),
            after,
            "an edited file is new content, not a repeat"
        );
        // And the old one is still known, so flipping back still references.
        assert!(once_per_session(&s, &before).contains("unchanged"));
    }

    #[test]
    fn two_sessions_do_not_share_what_they_have_been_told() {
        let (a, b) = (session(), session());
        let doc = "# shared\n\n".to_string() + &"z".repeat(200);
        assert_eq!(once_per_session(&a, &doc), doc);
        assert_eq!(
            once_per_session(&b, &doc),
            doc,
            "the other conversation cannot see what this one was told"
        );
        assert!(once_per_session(&a, &doc).contains("unchanged"));
    }

    #[test]
    fn forgetting_a_session_makes_the_next_injection_whole_again() {
        let s = session();
        let doc = "# doc\n\n".to_string() + &"q".repeat(200);
        assert_eq!(once_per_session(&s, &doc), doc);
        assert!(once_per_session(&s, &doc).contains("unchanged"));
        forget(&s);
        assert_eq!(
            once_per_session(&s, &doc),
            doc,
            "a reference to text the model can no longer see is worse than the text"
        );
    }

    #[test]
    fn a_batch_keeps_the_first_copy_and_drops_the_rest() {
        let doc = "# CLAUDE.md\n\nbody".to_string();
        let other = "# other\n\nbody".to_string();
        assert_eq!(
            within_batch(&[doc.clone(), other.clone(), doc.clone()]),
            vec![doc, other],
            "two hooks injecting the same document is one document"
        );
    }

    #[test]
    fn a_label_names_the_document_without_its_punctuation() {
        assert_eq!(label("## CLAUDE.md\n\nbody"), "CLAUDE.md");
        assert_eq!(label("\n\n-- codex: notes\nmore"), "codex: notes");
        assert_eq!(label(""), "");
        let long = "a".repeat(200);
        assert!(label(&long).chars().count() <= LABEL_CHARS);
        assert!(label(&long).ends_with('…'));
    }
}
