/**
 * Where a held paste ends up in the message.
 *
 * The defect these pin: a long paste was appended to the END of the draft at submit,
 * in queue order, so "compare THIS with THIS" arrived as both sentences followed by
 * both pastes — an order the user could neither choose nor see. The mark is the fix,
 * and the tests below are about the three things a mark buys that an offset would
 * not: it survives editing, deleting it drops the paste, and the DRAFT's order wins
 * over the queue's.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { expandPastes, pasteMark } from "./paste.ts";

const A = "aaa\nbbb";
const B = "console.error('boom')";

test("a mark expands where it sits, not at the end", () => {
  const draft = `compare ${pasteMark(1)} with ${pasteMark(2)} and explain`;
  assert.equal(expandPastes(draft, [A, B]), `compare ${A} with ${B} and explain`);
  // The whole point: the pastes are INSIDE the sentence, not trailing it.
  assert.ok(expandPastes(draft, [A, B]).endsWith("and explain"));
});

test("the draft's order wins over the queue's", () => {
  // #1 was pasted first; the user moved it after #2. What the draft says goes.
  assert.equal(expandPastes(`${pasteMark(2)} then ${pasteMark(1)}`, [A, B]), `${B} then ${A}`);
});

test("deleting a mark drops its paste — that is the removal gesture", () => {
  assert.equal(expandPastes(`only ${pasteMark(2)}`, [A, B]), `only ${B}`);
  assert.equal(expandPastes("nothing held", [A, B]), "nothing held");
});

test("an ordinal is a name, not a position", () => {
  // #1's mark is gone; #2 is still #2 in both the draft and the row. Renumbering
  // would rewrite marks the user is looking at, under their cursor.
  assert.equal(expandPastes(`keep ${pasteMark(2)}`, [A, B, "third"]), `keep ${B}`);
});

test("a mark repeated is a paste repeated", () => {
  assert.equal(expandPastes(`${pasteMark(1)} vs ${pasteMark(1)}`, [A]), `${A} vs ${A}`);
});

test("a mark with no paste behind it is left exactly as written", () => {
  // Typed by hand, or left over from a message that was already sent. Substituting
  // something for it would be worse than the literal text that was asked for.
  assert.equal(expandPastes(`see ${pasteMark(7)}`, [A]), `see ${pasteMark(7)}`);
});

test("the mark is the chip's own label, so the draft reads like the row", () => {
  assert.equal(pasteMark(1), "[Pasted text #1]");
});
