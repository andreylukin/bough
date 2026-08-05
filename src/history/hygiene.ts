/**
 * Write-time tag hygiene: what a coined tag has to earn to enter the vocabulary.
 *
 * THE MEASUREMENT THIS ANSWERS. Three days of live use produced 1,431 distinct
 * coined tags, 572 of them used exactly once — 40% of the vocabulary, written once
 * and never looked up. `bough tags show` missed 7 of 46 guesses because the word
 * the model reached for had never converged on anything.
 *
 * WHAT THE LITERATURE SAYS, and it is mostly a warning. Singleton dominance is the
 * normal steady state of a folksonomy, not a defect of this one: Guy & Tonkin
 * (*Tidying up Tags?*, D-Lib 2006) report that "the majority of tags are generally
 * believed to be single-use", and Cattuto et al. (*Vocabulary growth in
 * collaborative tagging systems*, 2007) show vocabulary growing as a sublinear
 * power law — Heaps' law — "remarkably regular throughout the entire history of the
 * system". Our own corpus fits that: γ ≈ 0.76, inside the folksonomy regime. So the
 * vocabulary will grow forever, no rule stops it, and a target of "no singletons"
 * is a target of beating a power law.
 *
 * Guy & Tonkin go further and argue against the cleanup instinct outright: a
 * single-use tag "may yet turn out to have a use in another domain or context", and
 * some are unique markers BY DESIGN — which is exactly what our `linear.*` / `pr.*`
 * references are, and why `isRef` exempts them everywhere below.
 *
 * SO THE ONLY DROPS HERE ARE ONES THAT DESTROY NO INFORMATION. A tag is dropped
 * only when the thing it names is still findable without it — specifically, when it
 * is already a substring of the command, which `command_history_fts` indexes. That
 * tag was duplicating the keyword index, not adding to it. Everything else is
 * SNAPPED (rewritten onto a word this repo already uses) or kept. Nothing that
 * carries information is thrown away here; the rest of the problem is handled at
 * READ time, where `history/stats.ts` demotes a once-used tag out of the priming
 * note without touching the row.
 *
 * That read-side lever is the one with the strongest evidence behind it. Sen et al.
 * (*tagging, communities, vocabulary, evolution*, CSCW 2006) ran a controlled
 * experiment across four tag-display algorithms and found that which tags you
 * DISPLAY changes which tags get applied — "pre-existing tags affect future tagging
 * behavior", and users "tend to follow the pre-seeded tag distribution". They also
 * proposed both moves this module makes: steering a tagger from "pop" to "soda" by
 * conflating the terms transparently, and treating a tag "applied very few times"
 * as too obscure to be worth displaying.
 */

import { isRef, splitTags } from "./record.ts";

/**
 * Suffixes stripped when looking for the word a tag is an inflection of. Ordered
 * longest-first so `running` reaches `runn`→ no, but `runs` reaches `run`; the
 * stem is only ever accepted when it is ALREADY a word in this repo, so a wrong
 * strip fails closed rather than inventing a tag.
 */
const SUFFIXES = ["ing", "ed", "es", "s"];

/**
 * Distinct words a repo needs before the echo rule is allowed to drop anything.
 *
 * THE COLD-START TRAP THIS EXISTS FOR, which a test caught and which would
 * otherwise have been permanent. The rule drops a tag that is NOVEL and inside its
 * own command — but in a fresh repo every tag is novel, and the best tags in this
 * entire corpus are substrings of the commands they tag: `git` (1,472 uses), `rg`,
 * `bun`, `test`. On day one `git` on `git status` reads as an echo, gets dropped,
 * and therefore never enters the vocabulary that would have protected it on day
 * two. The rule would have starved the vocabulary it depends on.
 *
 * So "novel" only means something once there is a vocabulary to be novel against.
 * Below this, hygiene snaps but never drops.
 */
const MIN_VOCAB_FOR_DROP = 200;

/** A tag's stem candidates, longest suffix first, including the tag itself. */
function stems(tag: string): string[] {
  const out = [tag];
  for (const suf of SUFFIXES) {
    if (tag.length > suf.length + 2 && tag.endsWith(suf)) out.push(tag.slice(0, -suf.length));
  }
  return out;
}

/**
 * The canonical spelling of each stem in a vocabulary: the most-used word that
 * reduces to it, ties broken alphabetically so the mapping is deterministic.
 *
 * Built from the vocabulary rather than from a stemmer's rules, which is what makes
 * this safe — `evaluators` snaps to `evaluator` only because `evaluator` is a word
 * this project already uses, never because an algorithm thinks it should be.
 */
export function canonicalByStem(vocab: Map<string, number>): Map<string, string> {
  const best = new Map<string, string>();
  for (const [tag, uses] of vocab) {
    for (const stem of stems(tag)) {
      const cur = best.get(stem);
      if (cur === undefined) {
        best.set(stem, tag);
        continue;
      }
      const curUses = vocab.get(cur) ?? 0;
      if (uses > curUses || (uses === curUses && tag < cur)) best.set(stem, tag);
    }
  }
  return best;
}

/**
 * Apply hygiene to one command's normalized tags. Returns the tags to store.
 *
 * `vocab` is this repo's existing coined words and their counts; `command` is what
 * the tags are about. Both rules need one of the two, which is why this cannot live
 * in `normalizeTags` — that function is pure and total by design.
 */
export function cleanTags(
  tagList: readonly string[],
  command: string,
  vocab: Map<string, number>,
): string[] {
  if (tagList.length === 0) return [];
  const canonical = canonicalByStem(vocab);
  const haystack = command.toLowerCase();
  const mayDrop = vocab.size >= MIN_VOCAB_FOR_DROP;
  const out: string[] = [];
  for (const raw of tagList) {
    // A reference is a key, not a word. It is never snapped and never dropped: it
    // is meant to be used once, it is looked up by name, and it is already excluded
    // from every ranking (`rankTags`).
    if (isRef(raw)) {
      out.push(raw);
      continue;
    }
    // SNAP FIRST, so a word that only looked novel because of an `s` is a known
    // word by the time the drop rule asks whether it is novel.
    let tag = raw;
    if (!vocab.has(tag)) {
      for (const stem of stems(tag)) {
        const hit = canonical.get(stem);
        if (hit !== undefined && hit !== tag) {
          tag = hit;
          break;
        }
      }
    }
    // DROP AN ECHO: novel here, and already inside the command it is tagging. It
    // adds nothing `command_history_fts` does not already index, and by being novel
    // it is a word nobody will ever guess in `tags show`. A word that IS in the
    // vocabulary stays even when it echoes — `git` on `git status` is the best tag
    // that command can have, and its presence in the vocabulary is the proof.
    if (mayDrop && !vocab.has(tag) && tag.length >= 4 && haystack.includes(tag)) continue;
    out.push(tag);
  }
  // Never let hygiene untag a command outright: 100% coverage is the one property
  // of this memory that has never slipped, and a row with no tags is a row that
  // only keyword search can reach. If everything was an echo, keep the first.
  const kept = [...new Set(out)];
  return kept.length > 0 ? kept : [tagList[0]];
}

/** `cleanTags` over the colon-joined form the recorder carries. */
export function cleanTagString(tags: string, command: string, vocab: Map<string, number>): string {
  return cleanTags(splitTags(tags), command, vocab).join(":");
}
