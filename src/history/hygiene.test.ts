/**
 * Tests for write-time tag hygiene.
 *
 * The two that are load-bearing, and why:
 *
 *   - **The cold-start guard.** Every tag in a fresh repo is novel, and the best
 *     tags in the corpus (`git`, `bun`, `test`, `rg`) are substrings of the commands
 *     they tag. Without the vocabulary floor the drop rule starves the vocabulary it
 *     depends on, permanently, and nothing downstream would ever look wrong.
 *   - **Never untag a command.** 100% tag coverage is the one property of this
 *     memory that has never slipped. A row with no tags is reachable only by keyword.
 *
 * The snap direction matters too: it is driven by what this repo ALREADY says, never
 * by a stemmer's opinion, so a wrong strip fails closed instead of inventing a word.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { canonicalByStem, cleanTags } from "./hygiene.ts";

/** A vocabulary big enough that the drop rule is live. */
function vocab(entries: [string, number][]): Map<string, number> {
  const m = new Map(entries);
  for (let i = 0; m.size < 200; i++) m.set(`filler${i}`, 3);
  return m;
}

/** Below the floor: hygiene may snap, never drop. */
const young = (entries: [string, number][]) => new Map(entries);

test("a plural snaps onto the singular this project already uses", () => {
  const v = vocab([["evaluator", 40]]);
  assert.deepEqual(cleanTags(["evaluators"], "rg evaluators src", v), ["evaluator"]);
});

test("the snap works the other way too — the vocabulary decides, not a rule", () => {
  const v = vocab([["evaluators", 40]]);
  assert.deepEqual(cleanTags(["evaluator"], "rg foo src", v), ["evaluators"]);
});

test("the most-used spelling wins when a stem has two forms", () => {
  const canon = canonicalByStem(new Map([["deploy", 3], ["deploys", 30]]));
  assert.equal(canon.get("deploy"), "deploys");
});

test("a novel word already inside its own command is dropped", () => {
  const v = vocab([["rg", 50]]);
  // `pycache` adds nothing: command_history_fts already indexes the command.
  assert.deepEqual(cleanTags(["rg", "pycache"], "rg -n pycache src/", v), ["rg"]);
});

test("a word IN the vocabulary survives echoing its command", () => {
  // `git` is the best tag `git status` can have, and its presence in the vocabulary
  // is the proof. Only NOVEL echoes are noise.
  const v = vocab([["git", 900], ["status", 200]]);
  assert.deepEqual(cleanTags(["git", "status"], "git status --short", v), ["git", "status"]);
});

test("a novel word NOT in its command is kept — it is a description, not an echo", () => {
  const v = vocab([["git", 900]]);
  assert.deepEqual(cleanTags(["git", "quiesce"], "git push --force-with-lease", v), [
    "git",
    "quiesce",
  ]);
});

test("COLD START: with no vocabulary to be novel against, nothing is dropped", () => {
  // The trap this guards. `bun` and `test` are both inside the command and both
  // novel; dropping them here means neither ever enters the vocabulary, so neither
  // is ever protected, forever.
  assert.deepEqual(cleanTags(["bun", "test"], "bun test src/a.ts", young([])), ["bun", "test"]);
  assert.deepEqual(cleanTags(["git", "status"], "git status", young([["x", 1]])), [
    "git",
    "status",
  ]);
});

test("hygiene never untags a command outright", () => {
  const v = vocab([["rg", 50]]);
  const out = cleanTags(["pycache"], "rg pycache", v);
  assert.deepEqual(out, ["pycache"], "the last tag survives even as an echo");
});

test("references are never snapped and never dropped", () => {
  const v = vocab([["linear", 30], ["pr", 30]]);
  // `linear.nme-1566` is inside its own command and would be a novel echo by every
  // test above. It is a key, not a word — Guy & Tonkin's unique-marker case.
  assert.deepEqual(
    cleanTags(["linear.nme-1566", "pr.19"], "gh pr view 19 # linear.nme-1566", v),
    ["linear.nme-1566", "pr.19"],
  );
});

test("a short novel word is not treated as an echo", () => {
  // Three letters match too much of too many commands to be evidence of anything.
  const v = vocab([["git", 900]]);
  assert.deepEqual(cleanTags(["git", "uae"], "kubectl get pods -n uae", v), ["git", "uae"]);
});

test("duplicates collapse when two tags snap onto the same word", () => {
  const v = vocab([["deploy", 40]]);
  assert.deepEqual(cleanTags(["deploy", "deploys"], "helm upgrade", v), ["deploy"]);
});

test("no tags in, no tags out", () => {
  assert.deepEqual(cleanTags([], "git status", vocab([])), []);
});
