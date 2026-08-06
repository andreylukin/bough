//! Write-time tag hygiene (port of `src/history/hygiene.ts`): what a coined
//! tag has to earn to enter the vocabulary.
//!
//! THE MEASUREMENT THIS ANSWERS. Three days of live use produced 1,431
//! distinct coined tags, 572 of them used exactly once — 40% of the
//! vocabulary, written once and never looked up. `bough tags show` missed 7
//! of 46 guesses because the word the model reached for had never converged
//! on anything.
//!
//! SO THE ONLY DROPS HERE ARE ONES THAT DESTROY NO INFORMATION. A tag is
//! dropped only when the thing it names is still findable without it —
//! specifically, when it is already a substring of the command, which
//! `command_history_fts` indexes. That tag was duplicating the keyword index,
//! not adding to it. Everything else is SNAPPED (rewritten onto a word this
//! repo already uses) or kept. Nothing that carries information is thrown
//! away here; the rest of the problem is handled at READ time, where
//! `history/tags/stats.rs` demotes a once-used tag out of the priming note
//! without touching the row.

use std::collections::HashMap;

use super::record::{is_ref, split_tags};

/// Suffixes stripped when looking for the word a tag is an inflection of.
/// Ordered longest-first so `running` reaches `runn`→ no, but `runs` reaches
/// `run`; the stem is only ever accepted when it is ALREADY a word in this
/// repo, so a wrong strip fails closed rather than inventing a tag.
const SUFFIXES: [&str; 4] = ["ing", "ed", "es", "s"];

/// Distinct words a repo needs before the echo rule is allowed to drop
/// anything.
///
/// THE COLD-START TRAP THIS EXISTS FOR, which a test caught and which would
/// otherwise have been permanent. The rule drops a tag that is NOVEL and
/// inside its own command — but in a fresh repo every tag is novel, and the
/// best tags in this entire corpus are substrings of the commands they tag:
/// `git` (1,472 uses), `rg`, `bun`, `test`. On day one `git` on `git status`
/// reads as an echo, gets dropped, and therefore never enters the vocabulary
/// that would have protected it on day two. The rule would have starved the
/// vocabulary it depends on.
///
/// So "novel" only means something once there is a vocabulary to be novel
/// against. Below this, hygiene snaps but never drops.
const MIN_VOCAB_FOR_DROP: usize = 200;

/// A tag's stem candidates, longest suffix first, including the tag itself.
fn stems(tag: &str) -> Vec<String> {
    let mut out = vec![tag.to_string()];
    for suf in SUFFIXES {
        if tag.len() > suf.len() + 2 && tag.ends_with(suf) {
            out.push(tag[..tag.len() - suf.len()].to_string());
        }
    }
    out
}

/// The canonical spelling of each stem in a vocabulary: the most-used word
/// that reduces to it, ties broken alphabetically so the mapping is
/// deterministic.
///
/// Built from the vocabulary rather than from a stemmer's rules, which is
/// what makes this safe — `evaluators` snaps to `evaluator` only because
/// `evaluator` is a word this project already uses, never because an
/// algorithm thinks it should be.
pub fn canonical_by_stem(vocab: &HashMap<String, i64>) -> HashMap<String, String> {
    let mut best: HashMap<String, String> = HashMap::new();
    for (tag, &uses) in vocab {
        for stem in stems(tag) {
            match best.get(&stem) {
                None => {
                    best.insert(stem, tag.clone());
                }
                Some(cur) => {
                    let cur_uses = vocab.get(cur).copied().unwrap_or(0);
                    if uses > cur_uses || (uses == cur_uses && tag < cur) {
                        best.insert(stem, tag.clone());
                    }
                }
            }
        }
    }
    best
}

/// Apply hygiene to one command's normalized tags. Returns the tags to store.
///
/// `vocab` is this repo's existing coined words and their counts; `command`
/// is what the tags are about. Both rules need one of the two, which is why
/// this cannot live in `normalize_tags` — that function is pure and total by
/// design.
pub fn clean_tags(tag_list: &[String], command: &str, vocab: &HashMap<String, i64>) -> Vec<String> {
    if tag_list.is_empty() {
        return Vec::new();
    }
    let canonical = canonical_by_stem(vocab);
    let haystack = command.to_lowercase();
    let may_drop = vocab.len() >= MIN_VOCAB_FOR_DROP;
    let mut out: Vec<String> = Vec::new();
    for raw in tag_list {
        // A reference is a key, not a word. It is never snapped and never
        // dropped: it is meant to be used once, it is looked up by name, and
        // it is already excluded from every ranking (`rank_tags`).
        if is_ref(raw) {
            out.push(raw.clone());
            continue;
        }
        // SNAP FIRST, so a word that only looked novel because of an `s` is
        // a known word by the time the drop rule asks whether it is novel.
        let mut tag = raw.clone();
        if !vocab.contains_key(&tag) {
            for stem in stems(&tag) {
                if let Some(hit) = canonical.get(&stem) {
                    if hit != &tag {
                        tag = hit.clone();
                        break;
                    }
                }
            }
        }
        // DROP AN ECHO: novel here, and already inside the command it is
        // tagging. It adds nothing `command_history_fts` does not already
        // index, and by being novel it is a word nobody will ever guess in
        // `tags show`. A word that IS in the vocabulary stays even when it
        // echoes — `git` on `git status` is the best tag that command can
        // have, and its presence in the vocabulary is the proof.
        if may_drop && !vocab.contains_key(&tag) && tag.len() >= 4 && haystack.contains(&tag) {
            continue;
        }
        out.push(tag);
    }
    // Never let hygiene untag a command outright: 100% coverage is the one
    // property of this memory that has never slipped, and a row with no tags
    // is a row that only keyword search can reach. If everything was an
    // echo, keep the first.
    let mut kept: Vec<String> = Vec::new();
    for t in out {
        if !kept.contains(&t) {
            kept.push(t);
        }
    }
    if kept.is_empty() {
        vec![tag_list[0].clone()]
    } else {
        kept
    }
}

/// [`clean_tags`] over the colon-joined form the recorder carries.
pub fn clean_tag_string(tags: &str, command: &str, vocab: &HashMap<String, i64>) -> String {
    clean_tags(&split_tags(tags), command, vocab).join(":")
}

// ---------------------------------------------------------------------------
// Tests — ported from src/history/hygiene.test.ts.
//
// The two that are load-bearing, and why:
//
//   - **The cold-start guard.** Every tag in a fresh repo is novel, and the
//     best tags in the corpus (`git`, `bun`, `test`, `rg`) are substrings of
//     the commands they tag. Without the vocabulary floor the drop rule
//     starves the vocabulary it depends on, permanently.
//   - **Never untag a command.** 100% tag coverage is the one property of
//     this memory that has never slipped.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A vocabulary big enough that the drop rule is live.
    fn vocab(entries: &[(&str, i64)]) -> HashMap<String, i64> {
        let mut m: HashMap<String, i64> =
            entries.iter().map(|(t, n)| (t.to_string(), *n)).collect();
        let mut i = 0;
        while m.len() < 200 {
            m.insert(format!("filler{i}"), 3);
            i += 1;
        }
        m
    }

    /// Below the floor: hygiene may snap, never drop.
    fn young(entries: &[(&str, i64)]) -> HashMap<String, i64> {
        entries.iter().map(|(t, n)| (t.to_string(), *n)).collect()
    }

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_plural_snaps_onto_the_singular_this_project_already_uses() {
        let v = vocab(&[("evaluator", 40)]);
        assert_eq!(
            clean_tags(&tags(&["evaluators"]), "rg evaluators src", &v),
            ["evaluator"]
        );
    }

    #[test]
    fn the_snap_works_the_other_way_too_the_vocabulary_decides_not_a_rule() {
        let v = vocab(&[("evaluators", 40)]);
        assert_eq!(
            clean_tags(&tags(&["evaluator"]), "rg foo src", &v),
            ["evaluators"]
        );
    }

    #[test]
    fn the_most_used_spelling_wins_when_a_stem_has_two_forms() {
        let canon = canonical_by_stem(&young(&[("deploy", 3), ("deploys", 30)]));
        assert_eq!(canon.get("deploy").map(String::as_str), Some("deploys"));
    }

    #[test]
    fn a_novel_word_already_inside_its_own_command_is_dropped() {
        let v = vocab(&[("rg", 50)]);
        // `pycache` adds nothing: command_history_fts already indexes the command.
        assert_eq!(
            clean_tags(&tags(&["rg", "pycache"]), "rg -n pycache src/", &v),
            ["rg"]
        );
    }

    #[test]
    fn a_word_in_the_vocabulary_survives_echoing_its_command() {
        // `git` is the best tag `git status` can have, and its presence in
        // the vocabulary is the proof. Only NOVEL echoes are noise.
        let v = vocab(&[("git", 900), ("status", 200)]);
        assert_eq!(
            clean_tags(&tags(&["git", "status"]), "git status --short", &v),
            ["git", "status"]
        );
    }

    #[test]
    fn a_novel_word_not_in_its_command_is_kept_a_description_not_an_echo() {
        let v = vocab(&[("git", 900)]);
        assert_eq!(
            clean_tags(
                &tags(&["git", "quiesce"]),
                "git push --force-with-lease",
                &v
            ),
            ["git", "quiesce"]
        );
    }

    #[test]
    fn cold_start_with_no_vocabulary_to_be_novel_against_nothing_is_dropped() {
        // The trap this guards. `bun` and `test` are both inside the command
        // and both novel; dropping them here means neither ever enters the
        // vocabulary, so neither is ever protected, forever.
        assert_eq!(
            clean_tags(&tags(&["bun", "test"]), "bun test src/a.ts", &young(&[])),
            ["bun", "test"]
        );
        assert_eq!(
            clean_tags(&tags(&["git", "status"]), "git status", &young(&[("x", 1)])),
            ["git", "status"]
        );
    }

    #[test]
    fn hygiene_never_untags_a_command_outright() {
        let v = vocab(&[("rg", 50)]);
        let out = clean_tags(&tags(&["pycache"]), "rg pycache", &v);
        assert_eq!(out, ["pycache"], "the last tag survives even as an echo");
    }

    #[test]
    fn references_are_never_snapped_and_never_dropped() {
        let v = vocab(&[("linear", 30), ("pr", 30)]);
        // `linear.nme-1566` is inside its own command and would be a novel
        // echo by every test above. It is a key, not a word.
        assert_eq!(
            clean_tags(
                &tags(&["linear.nme-1566", "pr.19"]),
                "gh pr view 19 # linear.nme-1566",
                &v
            ),
            ["linear.nme-1566", "pr.19"]
        );
    }

    #[test]
    fn a_short_novel_word_is_not_treated_as_an_echo() {
        // Three letters match too much of too many commands to be evidence
        // of anything.
        let v = vocab(&[("git", 900)]);
        assert_eq!(
            clean_tags(&tags(&["git", "uae"]), "kubectl get pods -n uae", &v),
            ["git", "uae"]
        );
    }

    #[test]
    fn duplicates_collapse_when_two_tags_snap_onto_the_same_word() {
        let v = vocab(&[("deploy", 40)]);
        assert_eq!(
            clean_tags(&tags(&["deploy", "deploys"]), "helm upgrade", &v),
            ["deploy"]
        );
    }

    #[test]
    fn no_tags_in_no_tags_out() {
        assert_eq!(
            clean_tags(&[], "git status", &vocab(&[])),
            Vec::<String>::new()
        );
    }

    #[test]
    fn clean_tag_string_round_trips_the_colon_joined_form() {
        let v = vocab(&[("evaluator", 40)]);
        assert_eq!(
            clean_tag_string("evaluators:rg", "rg foo", &v),
            "evaluator:rg"
        );
        assert_eq!(clean_tag_string("", "rg foo", &v), "");
    }
}
