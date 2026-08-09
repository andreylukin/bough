//! What a reader sees at a path, and what a turn is told.
//!
//! TWO MECHANISMS, and they solve the two risks transclusion creates.
//!
//! **Resolution** gathers the sections whose tags are a subset of the reader's
//! context and ranks them by `idf` — the same arithmetic the tag priming note
//! uses. That is the entire answer to promotion inflation: a section promoted
//! to a word every repo uses scores at the floor and never wins a slot, so the
//! incentive to over-promote disappears because the payoff does. There is no
//! cap, no quota, and no policy to tune.
//!
//! **The injection ledger** is the answer to context bloat. A hint that
//! re-says what the session was already told is pure cost, so the ledger
//! remembers the exact text injected per session per section and, on a later
//! round, sends **nothing** when it is unchanged and **only the difference**
//! when it grew. Bloat is therefore bounded by CHANGE rather than by a cap,
//! and a stable note costs one injection per session no matter how many rounds
//! touch it.
//!
//! The ledger is memory-only and bounded. Nothing is persisted: a restart that
//! re-injects one section once is harmless, and a table would make a cosmetic
//! feature a durability problem.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use sha2::{Digest, Sha256};

use crate::history::tags::stats::TagSpread;
use crate::types::SectionRow;

use super::section_score;

/// Sessions the ledger remembers. Bounded like the stats memo: the process
/// outlives any one conversation, and an unbounded map would grow with every
/// session forever.
const LEDGER_CAP: usize = 512;

/// Above this share of changed lines a section is re-sent WHOLE rather than as
/// a diff.
///
/// An added-lines diff would show a corrected claim as an addition, so a
/// resolved contradiction would read as new information sitting beside the old
/// claim still in context. Re-sending and labelling it is the honest form.
const REWRITE_SHARE: f64 = 0.5;

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// One section, resolved into a reader's context.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub section: SectionRow,
    pub score: f64,
    /// False when the section is authored on the note being read — its own
    /// prose, not something transcluded in.
    pub transcluded: bool,
}

/// Rank sections for a context, best first.
///
/// A section with NO tag in common scores zero and is dropped: `sections_for_context`
/// already guarantees subset membership, and a zero here means the section's
/// tags are a subset only because the context happens to be wide.
pub fn rank(
    spread: &TagSpread,
    sections: Vec<SectionRow>,
    context: &[String],
    own_note: Option<i64>,
) -> Vec<Resolved> {
    let mut out: Vec<Resolved> = sections
        .into_iter()
        .map(|section| {
            let score = section_score(spread, &section, context);
            let transcluded = Some(section.note_id) != own_note;
            Resolved {
                section,
                score,
                transcluded,
            }
        })
        .filter(|r| r.score > 0.0)
        .collect();
    out.sort_by(|a, b| {
        // Own sections first — a page's own prose is what it is about; then by
        // specificity; then newest, so a tie reads as the current thinking.
        a.transcluded
            .cmp(&b.transcluded)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.section.updated_at.cmp(&a.section.updated_at))
    });
    out
}

// ---------------------------------------------------------------------------
// The injection ledger
// ---------------------------------------------------------------------------

static LEDGER: LazyLock<Mutex<HashMap<String, HashMap<i64, Injected>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct Injected {
    hash: String,
    lines: Vec<String>,
}

/// What to put in front of the model for one section, this round.
#[derive(Clone, Debug, PartialEq)]
pub enum Injection {
    /// Already in the context above, unchanged. Say nothing.
    Skip,
    /// Never sent before.
    Full(String),
    /// Sent before and grown; only the new lines.
    Added(Vec<String>),
    /// Sent before and substantially rewritten; the whole thing again.
    Rewritten(String),
    /// Sent before, and only shrank. One line, because the model never needs
    /// the deleted text — it needs to know not to rely on it.
    Shrank,
}

fn hash_of(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// Decide what this session still needs to be told about `section_id`.
///
/// Records what it decides to send, so the next round sees it as sent.
pub fn injection_for(session_id: &str, section_id: i64, text: &str) -> Injection {
    let text = text.trim();
    if text.is_empty() {
        return Injection::Skip;
    }
    let hash = hash_of(text);
    let lines: Vec<String> = text.lines().map(str::to_string).collect();

    let mut guard = match LEDGER.lock() {
        Ok(g) => g,
        // A poisoned ledger must not silence a hint: fall back to sending it.
        Err(_) => return Injection::Full(text.to_string()),
    };
    if guard.len() >= LEDGER_CAP && !guard.contains_key(session_id) {
        // Bounded: drop an arbitrary other session rather than grow forever.
        if let Some(victim) = guard.keys().next().cloned() {
            guard.remove(&victim);
        }
    }
    let per_session = guard.entry(session_id.to_string()).or_default();

    let decision = match per_session.get(&section_id) {
        None => Injection::Full(text.to_string()),
        Some(prev) if prev.hash == hash => Injection::Skip,
        Some(prev) => {
            let added: Vec<String> = lines
                .iter()
                .filter(|l| !prev.lines.contains(l))
                .cloned()
                .collect();
            let removed = prev.lines.iter().filter(|l| !lines.contains(l)).count();
            let churn = (added.len() + removed) as f64 / prev.lines.len().max(1) as f64;
            if churn > REWRITE_SHARE {
                Injection::Rewritten(text.to_string())
            } else if !added.is_empty() {
                Injection::Added(added)
            } else if removed > 0 {
                Injection::Shrank
            } else {
                Injection::Skip
            }
        }
    };
    if decision != Injection::Skip {
        per_session.insert(section_id, Injected { hash, lines });
    }
    decision
}

/// Forget a session's ledger. Used by tests; a real session simply ages out.
pub fn forget(session_id: &str) {
    if let Ok(mut guard) = LEDGER.lock() {
        guard.remove(session_id);
    }
}

/// The one line a round's result carries for a resolved section.
///
/// `None` when there is nothing new to say — the commonest outcome once a
/// session has been running for a while, and the whole point of the ledger.
pub fn hint_line(session_id: &str, resolved: &Resolved) -> Option<String> {
    let s = &resolved.section;
    let label = if resolved.transcluded {
        format!("{} · {}", s.note_path, s.heading)
    } else {
        s.heading.clone()
    };
    match injection_for(session_id, s.id, &s.body) {
        Injection::Skip => None,
        Injection::Full(text) => Some(format!("[notes] {label}: {}", one_line(&text))),
        Injection::Added(lines) => Some(format!(
            "[notes] {label} +{}: {}",
            lines.len(),
            one_line(&lines.join(" "))
        )),
        Injection::Rewritten(text) => {
            Some(format!("[notes] {label} (rewritten): {}", one_line(&text)))
        }
        Injection::Shrank => Some(format!(
            "[notes] {label}: shortened since you were told it — bough notes show {}",
            s.note_path
        )),
    }
}

/// Flatten to one readable line. A hint rides the round's RESULT, where a
/// paragraph would be a wall.
fn one_line(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('>'))
        .collect::<Vec<_>>()
        .join(" ");
    let capped: String = joined.chars().take(200).collect();
    capped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NoteAuthor;

    fn section(id: i64, note_id: i64, tags: &[&str], body: &str) -> SectionRow {
        SectionRow {
            id,
            note_id,
            note_path: "nased".into(),
            ord: 0,
            heading: format!("h{id}"),
            body: body.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            citations: vec![],
            author: NoteAuthor::Human,
            created_at: 0,
            updated_at: id,
        }
    }

    fn spread() -> TagSpread {
        let mut by_tag = HashMap::new();
        by_tag.insert("git".to_string(), 26i64);
        by_tag.insert("nased".to_string(), 6i64);
        by_tag.insert("linear.nme-1673".to_string(), 1i64);
        TagSpread { repos: 30, by_tag }
    }

    #[test]
    fn a_section_promoted_to_a_ubiquitous_word_never_outranks_a_specific_one() {
        // THE promotion policy, and the only one. Nothing caps how many
        // sections may be promoted to `git`; they simply never win.
        let ranked = rank(
            &spread(),
            vec![
                section(1, 9, &["git"], "generic"),
                section(2, 9, &["nased"], "specific"),
                section(3, 9, &["linear.nme-1673"], "a ticket"),
            ],
            &["git".into(), "nased".into(), "linear.nme-1673".into()],
            None,
        );
        let order: Vec<i64> = ranked.iter().map(|r| r.section.id).collect();
        assert_eq!(order, vec![3, 2, 1], "reference > project word > tool word");
    }

    #[test]
    fn a_notes_own_sections_come_before_anything_transcluded() {
        let ranked = rank(
            &spread(),
            vec![
                section(1, 7, &["linear.nme-1673"], "borrowed, and very specific"),
                section(2, 9, &["git"], "mine, and generic"),
            ],
            &["git".into(), "linear.nme-1673".into()],
            Some(9),
        );
        assert_eq!(ranked[0].section.id, 2, "own prose first");
        assert!(!ranked[0].transcluded);
        assert!(ranked[1].transcluded);
    }

    #[test]
    fn a_section_sharing_nothing_with_the_context_is_dropped() {
        let ranked = rank(
            &spread(),
            vec![section(1, 9, &["other"], "x")],
            &["git".into()],
            None,
        );
        assert!(ranked.is_empty());
    }

    #[test]
    fn the_same_section_is_injected_once_per_session() {
        forget("s1");
        let body = "the DAG removal lands first";
        assert!(matches!(injection_for("s1", 1, body), Injection::Full(_)));
        assert_eq!(injection_for("s1", 1, body), Injection::Skip);
        assert_eq!(injection_for("s1", 1, body), Injection::Skip);
        // A different session is told from scratch.
        forget("s2");
        assert!(matches!(injection_for("s2", 1, body), Injection::Full(_)));
    }

    #[test]
    fn growth_sends_only_the_new_lines() {
        forget("s3");
        injection_for("s3", 1, "line one\nline two\nline three\nline four");
        match injection_for(
            "s3",
            1,
            "line one\nline two\nline three\nline four\nline five",
        ) {
            Injection::Added(lines) => assert_eq!(lines, vec!["line five".to_string()]),
            other => panic!("{other:?}"),
        }
        // And once told, it is not told again.
        assert_eq!(
            injection_for(
                "s3",
                1,
                "line one\nline two\nline three\nline four\nline five"
            ),
            Injection::Skip
        );
    }

    #[test]
    fn a_substantial_rewrite_is_re_sent_whole() {
        // An added-lines diff would show a corrected claim as an ADDITION,
        // leaving the superseded claim standing in the context above it.
        forget("s4");
        injection_for("s4", 1, "the cutover is blocked on the backfill window");
        match injection_for("s4", 1, "the cutover merged green on the second attempt") {
            Injection::Rewritten(text) => assert!(text.contains("merged green")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shrinking_says_so_without_reprinting_what_was_removed() {
        forget("s5");
        injection_for("s5", 1, "a\nb\nc\nd\ne\nf");
        assert_eq!(injection_for("s5", 1, "a\nb\nc\nd\ne"), Injection::Shrank);
    }

    #[test]
    fn a_hint_line_names_where_a_transcluded_section_is_authored() {
        forget("s6");
        let mut s = section(1, 7, &["nased"], "DAG removal lands first");
        s.note_path = "nased".into();
        s.heading = "Executor ordering".into();
        let resolved = Resolved {
            section: s,
            score: 1.0,
            transcluded: true,
        };
        let line = hint_line("s6", &resolved).unwrap();
        assert!(
            line.starts_with("[notes] nased · Executor ordering:"),
            "{line}"
        );
        assert!(line.contains("DAG removal lands first"));
        assert_eq!(hint_line("s6", &resolved), None, "said once");
    }

    #[test]
    fn a_warning_line_is_not_what_a_hint_quotes() {
        // The claim is the useful part; the marker is noise in one line.
        forget("s7");
        let resolved = Resolved {
            section: section(1, 7, &["nased"], "> [!WARNING] disputed\nthe real claim"),
            score: 1.0,
            transcluded: false,
        };
        let line = hint_line("s7", &resolved).unwrap();
        assert!(line.contains("the real claim"));
        assert!(!line.contains("[!WARNING]"), "{line}");
    }
}
