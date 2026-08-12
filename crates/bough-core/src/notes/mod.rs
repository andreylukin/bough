//! The note memory: prose keyed on the tags the commands already carry.
//!
//! WHY IT EXISTS. `command_history` records what ran and whether it worked; it
//! cannot record WHY. That knowledge either lives in a head or dies with the
//! session — the observed failure being nine `session_state` keys for one PR
//! rollout, each written by a different lineage root and invisible to the next.
//!
//! THE INVARIANT: **a note holds no command strings and no output.** Every
//! "how" is a citation into `bough tags show TAG` — a POINTER, never a copy.
//! Break it and the two stores become two records of one fact that age apart.
//!
//! PLACEMENT IS NOT ATTACHMENT, and conflating them is the trap this module
//! exists to keep untangled:
//!
//!   * **placement** (`notes.path`) is where a note sits for browsing — a colon
//!     path in the tag grammar's own order, so depth 1 is a top-level note
//!     about a word and deeper paths are notes about a combination;
//!   * **attachment** (`note_tags`, `section_tags`) is what a note COVERS —
//!     order-free set membership, because `tool:intent:subject` is a faceted
//!     grammar and not a containment tree. `atlas` appears under
//!     `kubectl:rollout:atlas` and `helm:upgrade:atlas` both, so prefix
//!     matching would miss the half that carries the meaning.
//!
//! THE SECTION IS THE ATOM. A lesson learned while working on
//! `atlas:rollout:prod` is often a truth about `atlas`; with the note as the
//! atom it would be stuck where it was written. A section has ONE home and
//! MANY appearances — resolved at read time, never copied, so one fix repairs
//! every appearance.

use std::collections::HashMap;

use crate::errors::BoughError;
use crate::history::tags::record::{is_ref, normalize_tags, split_tags};
use crate::history::tags::stats::TagSpread;
use crate::types::{Citation, Db, NoteRow, SectionRow};

pub mod resolve;

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

/// One log line. A cheap model asked for "a line" writes a paragraph if
/// nothing stops it.
pub const MAX_LINE_CHARS: usize = 120;

/// One section body. Notes, not storage: a payload belongs in a file with a
/// `[file:…]` citation pointing at it.
pub const MAX_SECTION_BYTES: usize = 16 * 1024;

/// A reference needs this many commands before automation will start a page
/// for it. On a real memory (1,971 tags, 143 references) this is the
/// difference between ~6 pages and 143 stubs.
pub const AUTO_CREATE_MIN_COMMANDS: usize = 20;

/// …across at least this many sessions, so one long afternoon does not mint a
/// page for a reference that never comes back.
pub const AUTO_CREATE_MIN_SESSIONS: usize = 2;

// ---------------------------------------------------------------------------
// Keys and paths
// ---------------------------------------------------------------------------

/// The key a note may live at, or why it may not.
///
/// THE JOIN IS THE WHOLE POINT, so a note has to sit at a key a COMMAND could
/// carry. `normalize_tags` treats a dash as a separator, so `wrapper-check` is
/// two tags and never one: a note filed there could never be reached by
/// `bough tags show`, never trigger a hint, and would report zero drift
/// forever — a page that looks filed and is actually orphaned.
///
/// Normalization that LOSES nothing is applied silently (`ATLAS` → `atlas`).
/// Normalization that would SPLIT is refused, because picking one of the two
/// halves would be guessing at which topic was meant.
pub fn canonical_key(raw: &str) -> Result<String, BoughError> {
    let normalized = normalize_tags(Some(raw));
    let parts = split_tags(&normalized);
    match parts.len() {
        1 => Ok(parts.into_iter().next().unwrap()),
        0 => Err(BoughError::bad_request(format!(
            "`{raw}` is not a tag — a tag needs at least one letter or digit. \
             A note is keyed on a tag so `bough tags show` can find it."
        ))),
        _ => {
            let joined = parts.join("_");
            let dotted = parts.join(".");
            Err(BoughError::bad_request(format!(
                "`{raw}` is {} tags, not one — a dash or a space SEPARATES tags, so no \
                 command could ever carry `{raw}` and a note there would be unreachable. \
                 Try `{joined}` for one word, or `{dotted}` if it names a ticket or a PR \
                 (a dot makes it a reference, and a reference keeps its dashes).",
                parts.len()
            )))
        }
    }
}

/// A note's PATH from what the user typed: `kubectl:rollout:atlas` stays in
/// the order written, because that order is the grammar's
/// (`tool:intent:subject`) and re-sorting it would put the tool where the
/// subject belongs.
///
/// Every segment must be a legal tag on its own — the path is a placement, but
/// its segments are the attachment, and an unreachable segment would be an
/// unreachable note.
pub fn canonical_path(raw: &str) -> Result<(String, Vec<String>), BoughError> {
    let mut segments: Vec<String> = Vec::new();
    for piece in raw.split(':') {
        if piece.trim().is_empty() {
            continue;
        }
        let key = canonical_key(piece)?;
        if !segments.contains(&key) {
            segments.push(key);
        }
    }
    if segments.is_empty() {
        return Err(BoughError::bad_request(format!(
            "`{raw}` names no tag — a note's path is one or more tags, colon separated"
        )));
    }
    Ok((segments.join(":"), segments))
}

/// How deep a path sits. Depth 1 is a top-level note about a single word.
pub fn depth(path: &str) -> usize {
    path.split(':').filter(|p| !p.is_empty()).count()
}

/// The STUBS between a set of paths: intermediate nodes nothing was written
/// at, needed so a tree renders as a tree.
///
/// Computed, never stored. An empty row would be a note that says nothing, and
/// the reason folder hierarchies ossify is that they make you create the
/// container before you have anything to put in it.
pub fn stubs_for(paths: &[String]) -> Vec<String> {
    let mut stubs: Vec<String> = Vec::new();
    for path in paths {
        let segments: Vec<&str> = path.split(':').collect();
        for cut in 1..segments.len() {
            let prefix = segments[..cut].join(":");
            if !paths.contains(&prefix) && !stubs.contains(&prefix) {
                stubs.push(prefix);
            }
        }
    }
    stubs.sort();
    stubs
}

// ---------------------------------------------------------------------------
// Citations
// ---------------------------------------------------------------------------

/// Pull the citations out of a section body.
///
/// Markdown in the prose, rows in the database — the same trick wikilinks use.
/// The readable form is what a human writes and reads; the rows are what can be
/// VALIDATED, and a citation that cannot be validated rots silently, which is
/// the failure this mechanism exists to prevent.
///
/// `[cmd:1234]` · `[msg:<id>]` · `[file:src/x.rs@3c1c78e]` · `[url:https://…]`
/// · `[sec:12]`
pub fn parse_citations(body: &str) -> Vec<Citation> {
    let mut out: Vec<Citation> = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '[' {
            i += 1;
            continue;
        }
        let Some(end) = chars[i..].iter().position(|c| *c == ']') else {
            break;
        };
        let inner: String = chars[i + 1..i + end].iter().collect();
        i += end + 1;
        let Some((prefix, reference)) = inner.split_once(':') else {
            continue;
        };
        let kind = match prefix.trim() {
            "cmd" | "command" => "command",
            "msg" | "message" => "message",
            "file" => "file",
            "url" => "url",
            "sec" | "section" => "section",
            _ => continue,
        };
        let reference = reference.trim().to_string();
        if reference.is_empty() {
            continue;
        }
        let citation = Citation {
            kind: kind.to_string(),
            reference,
        };
        if !out.contains(&citation) {
            out.push(citation);
        }
    }
    out
}

/// Render a command citation the way [`parse_citations`] reads it back.
pub fn cite_command(id: i64) -> String {
    format!("[cmd:{id}]")
}

/// Keep only the citations that resolve, and say which were dropped.
///
/// THE GUARD ON MACHINE WRITES. A `command` citation must name a row that
/// exists AND that carries one of the section's tags: existence stops an
/// invented id, and the tag check stops a real id with nothing to do with the
/// claim, which is the shape a plausible-but-wrong citation actually takes.
pub fn validate_citations(
    db: &dyn Db,
    citations: &[Citation],
    tags: &[String],
) -> (Vec<Citation>, Vec<Citation>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for c in citations {
        match db.citation_is_valid(&c.kind, &c.reference, tags) {
            Ok(true) => kept.push(c.clone()),
            _ => dropped.push(c.clone()),
        }
    }
    (kept, dropped)
}

// ---------------------------------------------------------------------------
// Sections from markdown
// ---------------------------------------------------------------------------

/// One parsed section: a `## Heading` and the prose under it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSection {
    pub heading: String,
    pub body: String,
    /// From a `tags:` line directly under the heading. `None` = inherit the
    /// note's, which is the default and what keeps a section where it was
    /// written. Narrowing this is PROMOTION.
    pub tags: Option<Vec<String>>,
}

/// Split a markdown body into sections.
///
/// Prose before the first heading becomes a section headed `Summary`, so a
/// one-paragraph note needs no ceremony — the common case must not require
/// learning the section syntax.
pub fn split_sections(markdown: &str) -> Vec<ParsedSection> {
    let mut out: Vec<ParsedSection> = Vec::new();
    let mut heading = String::from("Summary");
    let mut body: Vec<&str> = Vec::new();
    let mut tags: Option<Vec<String>> = None;

    fn flush(
        out: &mut Vec<ParsedSection>,
        heading: &str,
        body: &[&str],
        tags: &Option<Vec<String>>,
    ) {
        let text = body.join("\n").trim().to_string();
        if !text.is_empty() {
            out.push(ParsedSection {
                heading: heading.to_string(),
                body: text,
                tags: tags.clone(),
            });
        }
    }

    for line in markdown.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("## ") {
            flush(&mut out, &heading, &body, &tags);
            heading = rest.trim().to_string();
            body.clear();
            tags = None;
            continue;
        }
        // `tags: a, b` immediately under a heading narrows that section.
        if body.is_empty() {
            if let Some(rest) = line.trim().strip_prefix("tags:") {
                let parsed: Vec<String> = rest
                    .split([',', ' '])
                    .filter(|w| !w.trim().is_empty())
                    .filter_map(|w| canonical_key(w).ok())
                    .collect();
                if !parsed.is_empty() {
                    tags = Some(parsed);
                    continue;
                }
            }
        }
        body.push(line);
    }
    flush(&mut out, &heading, &body, &tags);
    out
}

/// Render sections back to the markdown [`split_sections`] reads.
pub fn render_sections(sections: &[SectionRow], note_tags: &[String]) -> String {
    let mut out = String::new();
    for s in sections {
        out.push_str(&format!("## {}\n", s.heading));
        // Only when it diverges: printing the inherited set on every section
        // would make promotion invisible by making it look routine.
        if s.tags != note_tags {
            out.push_str(&format!("tags: {}\n", s.tags.join(", ")));
        }
        out.push_str(&s.body);
        out.push_str("\n\n");
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Drift
// ---------------------------------------------------------------------------

/// How far behind a note is, on this machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Drift {
    pub path: String,
    pub unfolded: usize,
    /// What a fold advances the frontier to — never `now`, so a fold that
    /// skipped rows cannot mark them accounted for.
    pub newest_ts: Option<i64>,
    pub sessions: usize,
    /// An unresolved contradiction marker somewhere in the note.
    pub warned: bool,
}

impl Drift {
    /// Queue order: warnings above volume.
    pub fn severity(&self) -> (bool, usize) {
        (self.warned, self.unfolded)
    }
}

/// The marker a contradiction check inserts. Prose, so it reads in place; a
/// fixed prefix, so it is findable without a column.
pub const WARNING_PREFIX: &str = "> [!WARNING]";

pub fn is_warned(sections: &[SectionRow]) -> bool {
    sections.iter().any(|s| {
        s.body
            .lines()
            .any(|l| l.trim_start().starts_with(WARNING_PREFIX))
    })
}

/// Measure a note against the command memory.
///
/// A personal wiki cannot do this: it has no fact stream to check a page
/// against, so its only trigger for revision is "a new source arrived" and its
/// lint is structural. Here it is a COUNT, and no model is involved.
pub fn drift_for(db: &dyn Db, note: &NoteRow, repo: Option<&str>, warned: bool) -> Drift {
    let mut newest: Option<i64> = None;
    let mut sessions: Vec<String> = Vec::new();
    let mut seen_ids: Vec<i64> = Vec::new();
    let mut total = 0usize;
    for tag in &note.tags {
        let Ok(rows) = db.commands_for_tag(tag, repo, Some(500)) else {
            continue;
        };
        for row in rows.into_iter().filter(|r| r.ts > note.synced_ts) {
            // A command carrying two of the note's tags is ONE command; a
            // per-tag sum would report a two-tag note as twice as stale.
            if seen_ids.contains(&row.ts) {
                continue;
            }
            seen_ids.push(row.ts);
            total += 1;
            newest = Some(newest.map_or(row.ts, |n: i64| n.max(row.ts)));
            if !sessions.contains(&row.session_id) {
                sessions.push(row.session_id);
            }
        }
    }
    Drift {
        path: note.path.clone(),
        unfolded: total,
        newest_ts: newest,
        sessions: sessions.len(),
        warned,
    }
}

/// Has a reference earned a page yet?
pub fn earns_a_page(drift: &Drift) -> bool {
    drift.unfolded >= AUTO_CREATE_MIN_COMMANDS && drift.sessions >= AUTO_CREATE_MIN_SESSIONS
}

// ---------------------------------------------------------------------------
// Scoring, shared with the tag ranking
// ---------------------------------------------------------------------------

/// Inverse document frequency over repos — the SAME correction the priming
/// note ranks by, and the reason promotion needs no policing.
///
/// A section promoted to a tag every repo uses (`git`: 26 repos, `rg`: 28,
/// `inspect`: 36) scores at the floor and never wins a slot, however many
/// pages it becomes eligible for. One promoted to `atlas` (6 repos) is lifted.
/// The incentive to over-promote disappears because the payoff does.
pub fn idf(spread: &TagSpread, tag: &str) -> f64 {
    // BOTH FLOORS ARE LOAD-BEARING. `repos` is 0 on a memory that has recorded
    // nothing, and an unfloored numerator makes every idf `ln 1` = 0 — which
    // would filter out every section and mean NO note ever surfaces on a fresh
    // install. Floored, a fresh memory hands everything `ln 2` and the ranking
    // degenerates to "all equal", the honest answer when there is nothing to
    // contrast against, and the same fallback `rank_tags` documents.
    let repos = spread.repos.max(1) as f64;
    let used_by = spread.by_tag.get(tag).copied().unwrap_or(1).max(1) as f64;
    (1.0f64 + repos / used_by).ln()
}

/// Sum of idf over the tags a section and a context share.
///
/// Deliberately NOT `rank_tags`, whose two filters are right for a vocabulary
/// list and wrong here: it drops references, and `linear.nme-1673` is the most
/// specific match a section can have; and it demotes single-use tags, but a
/// section deliberately promoted to one is still a valid narrow match.
pub fn section_score(spread: &TagSpread, section: &SectionRow, context: &[String]) -> f64 {
    section
        .tags
        .iter()
        .filter(|t| context.contains(t))
        .map(|t| idf(spread, t))
        .sum()
}

/// Tag frequency across a set of notes — for reporting which promotions are
/// actually paying.
pub fn tag_use_counts(notes: &[NoteRow]) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for n in notes {
        for t in &n.tags {
            *out.entry(t.clone()).or_insert(0) += 1;
        }
    }
    out
}

/// Is this key a reference (an id with a life outside bough)?
pub fn is_reference(key: &str) -> bool {
    is_ref(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NoteAuthor;

    fn section(tags: &[&str], body: &str) -> SectionRow {
        SectionRow {
            id: 1,
            note_id: 1,
            note_path: "n".into(),
            ord: 0,
            heading: "h".into(),
            body: body.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            citations: vec![],
            author: NoteAuthor::Human,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn a_key_must_be_something_a_command_could_carry() {
        assert_eq!(canonical_key("atlas").unwrap(), "atlas");
        assert_eq!(canonical_key("linear.nme-1673").unwrap(), "linear.nme-1673");
        assert_eq!(canonical_key("ATLAS").unwrap(), "atlas");

        let split = canonical_key("wrapper-check").unwrap_err().to_string();
        assert!(split.contains("2 tags, not one"), "{split}");
        assert!(split.contains("unreachable"), "{split}");
        assert!(canonical_key("...").is_err());
    }

    #[test]
    fn every_legal_key_survives_a_round_trip_through_the_tag_normalizer() {
        for key in ["atlas", "linear.nme-1673", "pr.7134", "wrapper_check"] {
            let canonical = canonical_key(key).unwrap();
            assert_eq!(normalize_tags(Some(&canonical)), canonical, "{key}");
        }
    }

    #[test]
    fn a_path_keeps_the_grammars_order_and_yields_its_segments() {
        let (path, tags) = canonical_path("kubectl:rollout:atlas").unwrap();
        assert_eq!(path, "kubectl:rollout:atlas");
        assert_eq!(tags, vec!["kubectl", "rollout", "atlas"]);
        assert_eq!(depth(&path), 3);
        assert_eq!(depth("atlas"), 1, "a top-level note");
        // Re-sorting would put the tool where the subject belongs.
        assert_eq!(canonical_path("atlas:kubectl").unwrap().0, "atlas:kubectl");
        assert!(canonical_path("kubectl:wrapper-check").is_err());
    }

    #[test]
    fn stubs_are_the_gaps_between_paths_and_are_never_stored() {
        let paths = vec![
            "kubectl:rollout:atlas".to_string(),
            "kubectl:rollout:prod".to_string(),
            "atlas".to_string(),
        ];
        assert_eq!(stubs_for(&paths), vec!["kubectl", "kubectl:rollout"]);
        assert!(stubs_for(&["atlas".to_string()]).is_empty());
    }

    #[test]
    fn citations_are_parsed_out_of_prose() {
        let body = "the rollout worked [cmd:1234] but see [file:src/x.rs@3c1c78e] \
                    and [url:https://example.com/x], plus [sec:12] and [msg:abc].";
        let cites = parse_citations(body);
        assert_eq!(cites.len(), 5);
        assert!(cites.contains(&Citation {
            kind: "command".into(),
            reference: "1234".into()
        }));
        assert!(cites.contains(&Citation {
            kind: "file".into(),
            reference: "src/x.rs@3c1c78e".into()
        }));
        // A wikilink or an ordinary bracket is not a citation.
        assert!(parse_citations("see [[atlas]] and [note]").is_empty());
        assert_eq!(parse_citations(&cite_command(9))[0].reference, "9");
    }

    #[test]
    fn prose_before_any_heading_becomes_a_summary_section() {
        let parsed = split_sections("the DAG removal lands first.");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].heading, "Summary");
        assert_eq!(parsed[0].tags, None, "inherit by default");
    }

    #[test]
    fn a_tags_line_under_a_heading_is_promotion() {
        let parsed = split_sections(
            "intro prose\n\n## Executor ordering\ntags: atlas\nDAG removal first.\n\n\
             ## Backfill window\nonly here.",
        );
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].heading, "Summary");
        assert_eq!(parsed[1].heading, "Executor ordering");
        assert_eq!(parsed[1].tags, Some(vec!["atlas".to_string()]));
        assert!(!parsed[1].body.contains("tags:"), "the line is consumed");
        assert_eq!(parsed[2].tags, None, "unpromoted, so it inherits");
    }

    #[test]
    fn an_empty_memory_still_surfaces_notes() {
        // The bug this pins: with `repos = 0` every idf was `ln 1` = 0, every
        // section scored zero, and resolution dropped all of them — so a fresh
        // install would have had a note memory that never said anything.
        let empty = TagSpread {
            repos: 0,
            by_tag: HashMap::new(),
        };
        assert!(idf(&empty, "anything") > 0.0);
        let s = section(&["atlas"], "b");
        assert!(section_score(&empty, &s, &["atlas".into()]) > 0.0);
    }

    #[test]
    fn idf_damps_a_word_every_project_uses() {
        let mut spread = TagSpread {
            repos: 30,
            by_tag: HashMap::new(),
        };
        spread.by_tag.insert("git".into(), 26);
        spread.by_tag.insert("atlas".into(), 6);
        // This inequality IS the promotion policy: there is no other one.
        // ln(1 + 30/6) = 1.79 against ln(1 + 30/26) = 0.77 — a word six repos
        // use is worth more than twice one that twenty-six do.
        assert!(idf(&spread, "atlas") > idf(&spread, "git") * 2.0);
        assert!(
            idf(&spread, "unseen") > idf(&spread, "atlas"),
            "unknown = maximally specific"
        );
    }

    #[test]
    fn a_section_scores_only_on_the_tags_the_context_shares() {
        let mut spread = TagSpread {
            repos: 30,
            by_tag: HashMap::new(),
        };
        spread.by_tag.insert("atlas".into(), 6);
        spread.by_tag.insert("git".into(), 26);
        let s = section(&["atlas", "git"], "b");
        let both = section_score(&spread, &s, &["atlas".into(), "git".into()]);
        let one = section_score(&spread, &s, &["git".into()]);
        assert!(both > one);
        assert_eq!(section_score(&spread, &s, &["other".into()]), 0.0);
    }

    #[test]
    fn a_warning_anywhere_in_a_note_marks_it() {
        assert!(!is_warned(&[section(&[], "a claim")]));
        assert!(is_warned(&[
            section(&[], "a claim"),
            section(&[], "> [!WARNING] disputed")
        ]));
    }
}
