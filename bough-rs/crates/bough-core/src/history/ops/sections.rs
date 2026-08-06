//! Topic sections (port of `src/history/sections.ts`) — an LLM partitions a
//! conversation's turns into contiguous stretches labeled by WHAT THE WORK WAS
//! ABOUT ("token refresh race", "theme picker", "gap analysis"), so the client
//! can color history and offer a whole section as one selection for compaction
//! or extraction.
//!
//! THE INVARIANT THIS HOLDS: **this is a stateless labeling pass, and the CLIENT
//! decides what a turn is.** The request carries one gist per turn, in thread
//! order, and index i of the reply is turn i of the request. Nothing is read
//! from the database, nothing is written to it, and there is no `sections`
//! table, column or cache anywhere in the tree.
//!
//! Why the gists come from the client rather than being re-derived here from
//! `thread_for`: the returned ranges are only useful if they line up with the
//! rows the user is looking at, and turn grouping is a CLIENT decision — which
//! messages fold together, whether a system note starts a turn, how a subagent
//! rail collapses. A server that re-derived boundaries would answer "turns 3–5"
//! about a sequence the user cannot see, and the selection the label offers
//! would highlight the wrong rows.
//!
//! That the pass is stateless is also what makes it safe to run repeatedly: the
//! labels are a VIEW, so a client that re-labels after three more turns gets a
//! partition of the new history rather than a stale one stitched onto it.

use serde::{Deserialize, Serialize};

use crate::errors::{BoughError, ErrorKind};
use crate::llm::{client_for, complete_text, ClientOpts, CompleteTextOpts};
use crate::schema::requests::SectionsTurn;
use crate::types::LlmClient;
use std::sync::Arc;

/// One labeled stretch of history. Inclusive, 0-based, in the request's turn
/// indexes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Section {
    pub start: usize,
    pub end: usize,
    /// What this stretch was about, in the model's words.
    pub label: String,
}

/// Labeling is a cheap classification pass over one line per turn — always the
/// small model, NEVER the session's (possibly frontier) supervisor model, and
/// deliberately not `ctx.model`: a user pinned to Opus for the coding work
/// should not pay Opus rates to put seven-word headings on their history.
pub const SECTIONS_MODEL: &str = "claude-haiku-4-5";
const MAX_TOKENS: i64 = 1500;

const SYSTEM: &str = concat!(
    "You label the history of a coding-agent conversation. Given numbered turns ",
    "(each one line: the user's request and a gist of the reply), partition ALL turns into ",
    "contiguous sections BY TOPIC — group consecutive turns that are about the same piece of ",
    "work, and start a new section where the subject genuinely changes. Do NOT categorize by ",
    "activity type (debugging vs editing); the label says WHAT the work was about, in concrete ",
    "terms a reader scanning history would recognize (name the feature, bug, file, or question ",
    "— not 'various requests' or 'misc tasks'). Prefer fewer, broader sections over one per ",
    "turn.\n",
    "Reply with JSON only, no prose: [{\"start\":0,\"end\":2,\"label\":\"auth token refresh race\"}] ",
    "— start/end are inclusive 0-based turn indexes, labels at most 7 words.",
);

/// The longest label the UI is asked to render; anything longer is the model
/// ignoring the "7 words" instruction, and a rail is not the place to find that
/// out.
const LABEL_MAX: usize = 60;

/// One section as the model may emit it: bounds unchecked, in the model's own
/// (signed, unbounded) numbers.
#[derive(Deserialize, Clone, Debug, PartialEq)]
struct RawSection {
    start: i64,
    end: i64,
    label: String,
}

/// Parse the model's reply, tolerating code fences and surrounding prose.
///
/// Pure and public so the tolerance is directly testable: "```json\n[…]\n```"
/// and "Here you go: […]" are the two shapes a chat model actually returns when
/// told to emit JSON only, and both are ordinary answers rather than failures.
fn parse_sections(text: &str) -> Option<Vec<RawSection>> {
    let lo = text.find('[')?;
    let hi = text.rfind(']')?;
    if hi <= lo {
        return None;
    }
    serde_json::from_str::<Vec<RawSection>>(&text[lo..=hi])
        .ok()
        .filter(|rows| {
            // zod's `int().min(0)`: a negative or fractional index is not a section
            // this can normalize, it is a reply that was not understood.
            rows.iter().all(|r| r.start >= 0 && r.end >= 0)
        })
}

/// Force a possibly-sloppy answer into a clean PARTITION of `[0, n)`: sorted,
/// clipped to bounds, overlaps trimmed, gaps filled with "…".
///
/// The client renders these ranges directly and offers them as selections, so a
/// gap would be turns the user can see and cannot select, and an overlap would
/// be one turn claiming two labels. Normalizing here rather than re-prompting is
/// the right trade for a cosmetic pass: a slightly mislabeled boundary is a
/// usable answer, a second round-trip is a stall.
fn normalize_sections(raw: &[RawSection], n: usize) -> Vec<Section> {
    if n == 0 {
        return vec![];
    }
    let mut sorted: Vec<Section> = raw
        .iter()
        .filter(|s| (s.start as usize) < n && s.start <= s.end)
        .map(|s| Section {
            start: s.start as usize,
            end: (s.end as usize).min(n - 1),
            label: s.label.chars().take(LABEL_MAX).collect(),
        })
        .collect();
    sorted.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    let mut out: Vec<Section> = Vec::new();
    let mut next = 0usize;
    for s in sorted {
        if s.end < next {
            continue; // fully covered by an earlier section
        }
        let start = s.start.max(next);
        if start > next {
            out.push(Section {
                start: next,
                end: start - 1,
                label: "…".to_string(),
            });
        }
        out.push(Section {
            start,
            end: s.end,
            label: s.label,
        });
        next = s.end + 1;
    }
    if next < n {
        out.push(Section {
            start: next,
            end: n - 1,
            label: "…".to_string(),
        });
    }
    out
}

/// Partition `turns` into topic-labeled sections. Stateless: no database, no
/// session, no storage. 502 when the model's output cannot be parsed at all.
pub async fn sectionize(
    llm: Option<Arc<dyn LlmClient>>,
    turns: &[SectionsTurn],
) -> Result<Vec<Section>, BoughError> {
    let llm = llm.unwrap_or_else(|| client_for(SECTIONS_MODEL, ClientOpts::default()));
    // One line per turn, numbered — the numbers ARE the contract with the
    // reply, so a gist containing a newline is flattened rather than shifting
    // every index after it.
    let prompt = turns
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{i}. {}", t.gist.replace('\n', " ")))
        .collect::<Vec<_>>()
        .join("\n");
    let text = complete_text(
        &llm,
        CompleteTextOpts {
            model: SECTIONS_MODEL.to_string(),
            system: SYSTEM.to_string(),
            max_tokens: MAX_TOKENS,
            prompt,
        },
    )
    .await?;
    let Some(raw) = parse_sections(&text) else {
        return Err(BoughError::http(
            502,
            ErrorKind::Sections,
            format!(
                "section labeling failed: {SECTIONS_MODEL} returned no parseable JSON array \
                 (nothing was stored — history is unchanged; retry, or scroll without labels)"
            ),
        ));
    };
    Ok(normalize_sections(&raw, turns.len()))
}

// ---------------------------------------------------------------------------
// Tests (port of `src/history/sections.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ops::testkit::{scripted_ctx, Fixture};

    fn gists(lines: &[&str]) -> Vec<SectionsTurn> {
        lines
            .iter()
            .map(|g| SectionsTurn {
                gist: (*g).to_string(),
            })
            .collect()
    }

    fn ranges(sections: &[Section]) -> Vec<(usize, usize, String)> {
        sections
            .iter()
            .map(|s| (s.start, s.end, s.label.clone()))
            .collect()
    }

    fn raw(rows: &[(i64, i64, &str)]) -> Vec<RawSection> {
        rows.iter()
            .map(|(start, end, label)| RawSection {
                start: *start,
                end: *end,
                label: (*label).to_string(),
            })
            .collect()
    }

    fn with_reply(reply: &str) -> Fixture {
        let f = scripted_ctx();
        f.llm.set_reply(reply);
        f
    }

    // ---- parsing ------------------------------------------------------------

    #[test]
    fn parse_sections_tolerates_code_fences_and_surrounding_prose() {
        let wanted = raw(&[(0, 1, "auth")]);
        assert_eq!(
            parse_sections(r#"[{"start":0,"end":1,"label":"auth"}]"#),
            Some(wanted.clone())
        );
        assert_eq!(
            parse_sections("```json\n[{\"start\":0,\"end\":1,\"label\":\"auth\"}]\n```"),
            Some(wanted.clone())
        );
        assert_eq!(
            parse_sections(
                "Here you go: [{\"start\":0,\"end\":1,\"label\":\"auth\"}] — hope that helps!"
            ),
            Some(wanted)
        );
    }

    #[test]
    fn parse_sections_returns_none_on_anything_it_cannot_read() {
        assert_eq!(parse_sections("I'd rather not."), None);
        assert_eq!(parse_sections("[not json]"), None);
        assert_eq!(
            parse_sections(r#"[{"start":"zero","end":1,"label":"auth"}]"#),
            None,
            "wrong types"
        );
        assert_eq!(
            parse_sections(r#"[{"start":0,"end":1}]"#),
            None,
            "missing label"
        );
        assert_eq!(
            parse_sections("]["),
            None,
            "closing bracket before the opening one"
        );
        assert_eq!(
            parse_sections(r#"[{"start":-1,"end":1,"label":"x"}]"#),
            None,
            "negative index"
        );
    }

    // ---- normalization ------------------------------------------------------

    #[test]
    fn normalize_sections_fills_gaps_so_every_turn_is_selectable() {
        let out = normalize_sections(&raw(&[(2, 3, "theme picker")]), 6);
        assert_eq!(
            ranges(&out),
            vec![
                (0, 1, "…".to_string()),
                (2, 3, "theme picker".to_string()),
                (4, 5, "…".to_string())
            ]
        );
    }

    #[test]
    fn normalize_sections_trims_overlaps_so_no_turn_wears_two_labels() {
        let out = normalize_sections(
            &raw(&[(0, 3, "first"), (2, 5, "second"), (1, 2, "swallowed")]),
            6,
        );
        assert_eq!(
            ranges(&out),
            vec![(0, 3, "first".to_string()), (4, 5, "second".to_string())]
        );
    }

    #[test]
    fn normalize_sections_sorts_clips_to_bounds_and_drops_the_impossible() {
        let out = normalize_sections(
            &raw(&[
                (3, 99, "past the end"),
                (0, 2, "first"),
                (7, 8, "entirely past the end"),
                (2, 1, "backwards"),
            ]),
            5,
        );
        assert_eq!(
            ranges(&out),
            vec![
                (0, 2, "first".to_string()),
                (3, 4, "past the end".to_string())
            ]
        );
    }

    #[test]
    fn normalize_sections_always_covers_exactly_zero_to_n() {
        for rows in [
            vec![],
            raw(&[(0, 0, "a")]),
            raw(&[(1, 1, "b")]),
            raw(&[(0, 9, "everything")]),
        ] {
            let out = normalize_sections(&rows, 4);
            assert_eq!(out[0].start, 0);
            assert_eq!(out.last().unwrap().end, 3);
            for i in 1..out.len() {
                assert_eq!(
                    out[i].start,
                    out[i - 1].end + 1,
                    "contiguous, no gap and no overlap"
                );
            }
        }
    }

    #[test]
    fn normalize_sections_clips_a_runaway_label() {
        let out = normalize_sections(&raw(&[(0, 0, &"x".repeat(500))]), 1);
        assert_eq!(out[0].label.chars().count(), 60);
    }

    // ---- the pass -----------------------------------------------------------

    #[tokio::test]
    async fn sectionize_numbers_the_turns_it_sends_and_labels_them_on_the_cheap_model() {
        let f = with_reply(
            r#"[{"start":0,"end":1,"label":"token refresh race"},{"start":2,"end":2,"label":"theme picker"}]"#,
        );

        let out = sectionize(
            f.ctx.llm.clone(),
            &gists(&[
                "fix the refresh",
                "still failing\nsecond line",
                "pick a theme",
            ]),
        )
        .await
        .unwrap();

        assert_eq!(
            ranges(&out),
            vec![
                (0, 1, "token refresh race".to_string()),
                (2, 2, "theme picker".to_string())
            ]
        );
        assert_eq!(
            f.llm.models(),
            vec![SECTIONS_MODEL.to_string()],
            "never the session's frontier model"
        );
        assert_eq!(
            f.llm.prompts()[0].split('\n').collect::<Vec<_>>(),
            vec![
                "0. fix the refresh",
                "1. still failing second line",
                "2. pick a theme"
            ],
            "one line per turn — a newline in a gist would shift every index after it"
        );
    }

    #[tokio::test]
    async fn an_unparseable_reply_is_a_502_that_says_nothing_was_stored() {
        let f = with_reply("I can't do that.");
        let err = sectionize(f.ctx.llm.clone(), &gists(&["a", "b"]))
            .await
            .unwrap_err();
        assert_eq!(err.status(), 502);
        assert!(err.to_string().contains("nothing was stored"), "{err}");
    }

    #[tokio::test]
    async fn a_sloppy_reply_still_comes_back_as_a_usable_partition() {
        let f = with_reply(r#"[{"start":1,"end":99,"label":"the rest"}]"#);
        let out = sectionize(f.ctx.llm.clone(), &gists(&["a", "b", "c"]))
            .await
            .unwrap();
        assert_eq!(
            ranges(&out),
            vec![(0, 0, "…".to_string()), (1, 2, "the rest".to_string())]
        );
    }
}
