/**
 * The patch engine is the only thing standing between parallel subagents sharing
 * one checkout and silent data loss (plan §8.1), so this file is deliberately
 * exhaustive: every operation, every rejection, and — the part that actually
 * matters — the rebase-vs-conflict decision proved in BOTH directions.
 *
 * Everything here is pure string math. No filesystem, no clock, no network.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied
 * by this environment's egress policy, so the jsr import declared in `deno.json`
 * cannot resolve. `node:assert` is built into the runtime and needs no fetch.
 * (Same constraint `bus.test.ts` and `paths.test.ts` document.)
 */

import { test } from "bun:test";
import { deepStrictEqual, ok } from "node:assert";
import { PatchError } from "../errors.ts";
import {
  applyPatch,
  checkOps,
  groupByFile,
  joinLines,
  lineMap,
  materialize,
  normalize,
  parsePatch,
  rebaseOps,
  renderNumbered,
  tagOf,
  toLines,
} from "./patch.ts";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/** Build file text from lines, with a trailing newline like a real file. */
const doc = (...lines: string[]) => lines.join("\n") + "\n";

function apply(
  input: string,
  current: Record<string, string>,
  base?: Record<string, string>,
): Map<string, string> {
  const files = new Map(Object.entries(current));
  const opts = base ? { base: new Map(Object.entries(base)) } : {};
  return applyPatch(files, parsePatch(input), opts);
}

/**
 * Assert the call throws a `PatchError` whose message contains `needle`, and hand
 * the error back so a test can make further claims about its text. Error text is
 * a product surface (spec §6), so it is asserted on, not merely tolerated.
 */
function throwsPatch(fn: () => unknown, needle?: string): PatchError {
  let caught: unknown;
  let threw = false;
  try {
    fn();
  } catch (e) {
    threw = true;
    caught = e;
  }
  ok(threw, "expected a PatchError, but nothing was thrown");
  ok(caught instanceof PatchError, `expected a PatchError, got ${caught}`);
  const err = caught as PatchError;
  if (needle !== undefined) {
    ok(
      err.message.includes(needle),
      `message did not contain ${JSON.stringify(needle)}:\n  ${err.message}`,
    );
  }
  return err;
}

/** A four-hex tag guaranteed not to be `text`'s. */
function wrongTag(text: string): string {
  return tagOf(text) === "0000" ? "FFFF" : "0000";
}

const SIX = doc("one", "two", "three", "four", "five", "six");

// ---------------------------------------------------------------------------
// tags, normalization, joining
// ---------------------------------------------------------------------------

test("tagOf: CRLF and a BOM do not change a file's identity", () => {
  const lf = "a\nb\n";
  deepStrictEqual(tagOf(lf), tagOf("a\r\nb\r\n"));
  deepStrictEqual(tagOf(lf), tagOf("﻿a\nb\n"));
  ok(tagOf(lf) !== tagOf("a\nc\n"));
  ok(/^[0-9A-F]{4}$/.test(tagOf(lf)), tagOf(lf));
});

test("normalize / toLines: a trailing newline is not a line", () => {
  deepStrictEqual(normalize("﻿a\r\nb\r\n"), "a\nb\n");
  deepStrictEqual(toLines("a\nb\n"), ["a", "b"]);
  deepStrictEqual(toLines("a\nb"), ["a", "b"]);
  deepStrictEqual(toLines(""), []);
  deepStrictEqual(toLines("\n"), [""]);
});

test("joinLines: line-ending style and trailing newline survive a patch", () => {
  deepStrictEqual(joinLines(["a", "b"], "x\r\ny\r\n"), "a\r\nb\r\n");
  deepStrictEqual(joinLines(["a", "b"], "x\ny\n"), "a\nb\n");
  deepStrictEqual(joinLines(["a", "b"], "x\ny"), "a\nb");
  // A file emptied by a patch is empty, not a blank line.
  deepStrictEqual(joinLines([], "x\ny\n"), "");
});

test("renderNumbered: the exact shape view() hands the model", () => {
  const text = doc("alpha", "beta");
  deepStrictEqual(renderNumbered("a.ts", text), `[a.ts#${tagOf(text)}]\n1:alpha\n2:beta`);
  // Numbers are right-aligned once the file needs two digits.
  const wide = renderNumbered("a.ts", doc(...Array.from({ length: 10 }, (_, i) => `L${i}`)));
  ok(wide.includes("\n 1:L0"), wide);
  ok(wide.includes("\n10:L9"), wide);
});

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

test("parsePatch: every operation, with path and tag attached", () => {
  const ops = parsePatch(
    [
      "[src/a.ts#A1B2]",
      "SWAP 74.=76:",
      "+  hello",
      "+",
      "DEL 91.=92",
      "INS.PRE 30:",
      "+// before",
      "INS.POST 30:",
      "+// after",
      "INS.HEAD:",
      "+// top",
      "INS.TAIL:",
      "+// bottom",
    ].join("\n"),
  );
  deepStrictEqual(ops.map((o) => o.kind), [
    "swap",
    "del",
    "ins_pre",
    "ins_post",
    "ins_head",
    "ins_tail",
  ]);
  ok(ops.every((o) => o.path === "src/a.ts" && o.tag === "A1B2"));
  deepStrictEqual([ops[0].a, ops[0].b], [74, 76]);
  // A lone "+" is a blank line, not a terminator.
  deepStrictEqual(ops[0].body, ["  hello", ""]);
  deepStrictEqual([ops[1].a, ops[1].b], [91, 92]);
  deepStrictEqual(ops[2].a, 30);
  deepStrictEqual(ops[3].a, 30);
  deepStrictEqual(ops[4].a, undefined);
  deepStrictEqual(ops[5].a, undefined);
  // The input line is retained so parse errors can point at it.
  deepStrictEqual(ops[0].at, 2);
});

test("parsePatch: tagless headers and lowercase tags", () => {
  deepStrictEqual(parsePatch("[a.ts#]\nDEL 1")[0].tag, "");
  deepStrictEqual(parsePatch("[a.ts]\nDEL 1")[0].tag, "");
  deepStrictEqual(parsePatch("[a.ts#a1b2]\nDEL 1")[0].tag, "A1B2");
  // A four-hex tag wins the trailing segment rather than joining the path.
  deepStrictEqual(parsePatch("[a.ts#a1b2]\nDEL 1")[0].path, "a.ts");
  // A "#" that is not a tag stays in the path.
  deepStrictEqual(parsePatch("[weird#name.ts]\nDEL 1")[0].path, "weird#name.ts");
});

test("parsePatch: single-line and alternate range spellings", () => {
  for (const spelling of ["SWAP 5:", "SWAP 5.=5:", "SWAP 5..5:", "SWAP 5-5:", "SWAP 5 5:"]) {
    const [op] = parsePatch(`[a.ts#]\n${spelling}\n+x`);
    deepStrictEqual([op.a, op.b], [5, 5], spelling);
  }
  deepStrictEqual(parsePatch("[a.ts#]\nDEL 7")[0].b, 7);
  deepStrictEqual(parsePatch("[a.ts#]\nDEL 7..9")[0].b, 9);
});

test("parsePatch: multiple files in one patch", () => {
  const ops = parsePatch("[a.ts#]\nDEL 1\n\n[b.ts#C0DE]\nINS.TAIL:\n+z");
  deepStrictEqual(groupByFile(ops).map((g) => [g.path, g.tag, g.ops.length]), [
    ["a.ts", "", 1],
    ["b.ts", "C0DE", 1],
  ]);
});

test("parsePatch: a blank line ends a body without becoming content", () => {
  const ops = parsePatch("[a.ts#]\nINS.HEAD:\n+x\n\nINS.TAIL:\n+y");
  deepStrictEqual(ops[0].body, ["x"]);
  deepStrictEqual(ops[1].body, ["y"]);
});

test("parsePatch: Codex-style envelopes are swallowed", () => {
  deepStrictEqual(parsePatch("*** Begin Patch\n[a.ts#]\nDEL 1\n*** End Patch").length, 1);
});

test("parsePatch: rejections name the input line and the fix", () => {
  const cases: Array<[string, string]> = [
    ["", "empty patch"],
    ["DEL 1\n+x", "expected a section header"],
    ["[a.ts#]", "has no operations"],
    ["[a.ts#]\n+x", "has no operation above it"],
    ["[a.ts#]\nDEL 1\n+x", "DEL takes no body rows"],
    ["[a.ts#]\n-old line", '"-" rows are not part of this format'],
    ["[a.ts#]\n  12:const x = 1;", "looks like a line from view()'s listing"],
    ["[a.ts#]\nREPLACE 1 2", "is not an operation"],
    ["[a.ts#]\nSWAP one:", "is not an operation"],
  ];
  for (const [input, needle] of cases) {
    throwsPatch(() => parsePatch(input), needle);
  }
});

test("parsePatch: pasting view() output back is diagnosed, not guessed at", () => {
  const listing = renderNumbered("a.ts", SIX);
  const err = throwsPatch(() => parsePatch(listing), "Do not pass view()'s output");
  ok(err.message.includes('"[a.ts#]"'), err.message);
});

test("groupByFile: one path with two different tags is refused", () => {
  throwsPatch(
    () => groupByFile(parsePatch("[a.ts#A1B2]\nDEL 1\n\n[a.ts#C3D4]\nDEL 5")),
    "appears twice with different tags",
  );
  // The same tag twice merges into one section.
  const merged = groupByFile(parsePatch("[a.ts#A1B2]\nDEL 1\n\n[a.ts#A1B2]\nDEL 5"));
  deepStrictEqual(merged.length, 1);
  deepStrictEqual(merged[0].ops.length, 2);
});

// ---------------------------------------------------------------------------
// every operation, end to end
// ---------------------------------------------------------------------------

test("apply SWAP: single line, multi-line range, collapse and expand", () => {
  deepStrictEqual(
    apply("[a#]\nSWAP 2:\n+TWO", { a: SIX }).get("a"),
    doc("one", "TWO", "three", "four", "five", "six"),
  );
  deepStrictEqual(
    apply("[a#]\nSWAP 2.=4:\n+X", { a: SIX }).get("a"),
    doc("one", "X", "five", "six"),
  );
  deepStrictEqual(
    apply("[a#]\nSWAP 2.=2:\n+X\n+Y\n+Z", { a: SIX }).get("a"),
    doc("one", "X", "Y", "Z", "three", "four", "five", "six"),
  );
});

test("apply DEL: single line, range, and the whole file", () => {
  deepStrictEqual(
    apply("[a#]\nDEL 3", { a: SIX }).get("a"),
    doc("one", "two", "four", "five", "six"),
  );
  deepStrictEqual(apply("[a#]\nDEL 2.=5", { a: SIX }).get("a"), doc("one", "six"));
  deepStrictEqual(apply("[a#]\nDEL 1.=6", { a: SIX }).get("a"), "");
});

test("apply INS.PRE: text lands before the named line", () => {
  deepStrictEqual(
    apply("[a#]\nINS.PRE 1:\n+ZERO", { a: SIX }).get("a"),
    doc("ZERO", "one", "two", "three", "four", "five", "six"),
  );
  deepStrictEqual(
    apply("[a#]\nINS.PRE 6:\n+FIVE-AND-A-HALF", { a: SIX }).get("a"),
    doc("one", "two", "three", "four", "five", "FIVE-AND-A-HALF", "six"),
  );
});

test("apply INS.POST: text lands after the named line", () => {
  deepStrictEqual(
    apply("[a#]\nINS.POST 1:\n+ONE-AND-A-HALF", { a: SIX }).get("a"),
    doc("one", "ONE-AND-A-HALF", "two", "three", "four", "five", "six"),
  );
  deepStrictEqual(
    apply("[a#]\nINS.POST 6:\n+SEVEN", { a: SIX }).get("a"),
    doc("one", "two", "three", "four", "five", "six", "SEVEN"),
  );
});

test("apply INS.HEAD / INS.TAIL", () => {
  deepStrictEqual(
    apply("[a#]\nINS.HEAD:\n+top\nINS.TAIL:\n+bottom", { a: SIX }).get("a"),
    doc("top", "one", "two", "three", "four", "five", "six", "bottom"),
  );
  // They are the only ops that work on an empty file.
  deepStrictEqual(apply("[a#]\nINS.HEAD:\n+first", { a: "" }).get("a"), doc("first"));
  deepStrictEqual(apply("[a#]\nINS.TAIL:\n+last", { a: "" }).get("a"), doc("last"));
});

test("apply: an empty INS body is a no-op, not a corruption", () => {
  deepStrictEqual(apply("[a#]\nINS.HEAD:\nINS.TAIL:", { a: SIX }).get("a"), SIX);
});

test("apply: all six operations at once", () => {
  const out = apply(
    [
      "[a#]",
      "INS.HEAD:",
      "+H",
      "INS.PRE 1:",
      "+P",
      "SWAP 2:",
      "+TWO",
      "INS.POST 3:",
      "+Q",
      "DEL 5",
      "INS.TAIL:",
      "+T",
    ].join("\n"),
    { a: SIX },
  ).get("a");
  deepStrictEqual(out, doc("H", "P", "one", "TWO", "three", "Q", "four", "six", "T"));
});

// ---------------------------------------------------------------------------
// viewed coordinates — the "never apply sequentially" rule
// ---------------------------------------------------------------------------

test("line numbers are in the VIEWED version's coordinates", () => {
  // DEL 1.=2 removes two lines; applied sequentially, SWAP 5 would then point at
  // "six". It must still mean "five".
  deepStrictEqual(
    apply("[a#]\nDEL 1.=2\nSWAP 5:\n+FIVE", { a: SIX }).get("a"),
    doc("three", "four", "FIVE", "six"),
  );
  // An expanding SWAP above must not shift the anchor below it either.
  deepStrictEqual(
    apply("[a#]\nSWAP 1:\n+A\n+B\n+C\nDEL 4", { a: SIX }).get("a"),
    doc("A", "B", "C", "two", "three", "five", "six"),
  );
});

test("op order in the patch text does not change the result", () => {
  const forwards = "[a#]\nDEL 1\nINS.POST 3:\n+X\nSWAP 6:\n+SIX";
  const backwards = "[a#]\nSWAP 6:\n+SIX\nINS.POST 3:\n+X\nDEL 1";
  deepStrictEqual(apply(forwards, { a: SIX }).get("a"), apply(backwards, { a: SIX }).get("a"));
  deepStrictEqual(
    apply(forwards, { a: SIX }).get("a"),
    doc("two", "three", "X", "four", "five", "SIX"),
  );
});

test("materialize: gap ordering is fixed and documented", () => {
  const lines = ["one", "two", "three"];
  deepStrictEqual(
    materialize(lines, parsePatch("[a#]\nINS.PRE 2:\n+pre2\nSWAP 2:\n+TWO\nINS.POST 2:\n+post2")),
    ["one", "pre2", "TWO", "post2", "three"],
  );
  // INS.POST N precedes INS.PRE N+1 in the gap they share.
  deepStrictEqual(
    materialize(lines, parsePatch("[a#]\nINS.PRE 2:\n+B\nINS.POST 1:\n+A")),
    ["one", "A", "B", "two", "three"],
  );
  // Two ops of the same kind at one anchor emit in patch order.
  deepStrictEqual(
    materialize(lines, parsePatch("[a#]\nINS.POST 1:\n+A\nINS.POST 1:\n+B")),
    ["one", "A", "B", "two", "three"],
  );
});

test("INS.POST at the last line of a DEL span still lands", () => {
  deepStrictEqual(
    apply("[a#]\nDEL 2.=4\nINS.POST 4:\n+X", { a: SIX }).get("a"),
    doc("one", "X", "five", "six"),
  );
});

// ---------------------------------------------------------------------------
// rejections
// ---------------------------------------------------------------------------

test("out-of-bounds anchors are rejected", () => {
  throwsPatch(() => apply("[a#]\nSWAP 7:\n+x", { a: SIX }), "out of range");
  throwsPatch(() => apply("[a#]\nDEL 0", { a: SIX }), "out of range");
  throwsPatch(() => apply("[a#]\nINS.PRE 9:\n+x", { a: SIX }), "out of range");
  throwsPatch(() => apply("[a#]\nINS.POST 7:\n+x", { a: SIX }), "out of range");
  throwsPatch(() => apply("[a#]\nDEL 3.=99", { a: SIX }), "is invalid");
  throwsPatch(() => apply("[a#]\nSWAP 4.=2:\n+x", { a: SIX }), "is invalid");
  // The message names the file and its real length so the model can re-aim.
  throwsPatch(() => apply("[a#]\nSWAP 7:\n+x", { a: SIX }), "a has 6 lines");
});

test("an empty file rejects line-anchored ops by name", () => {
  throwsPatch(() => apply("[a#]\nSWAP 1:\n+x", { a: "" }), "a is empty");
  throwsPatch(() => apply("[a#]\nDEL 1", { a: "" }), "INS.HEAD");
});

test("overlapping ranges are rejected rather than silently ordered", () => {
  throwsPatch(() => apply("[a#]\nSWAP 2.=4:\n+x\nDEL 3.=5", { a: SIX }), "operations overlap");
  // Identical spans overlap too.
  throwsPatch(() => apply("[a#]\nDEL 2\nDEL 2", { a: SIX }), "operations overlap");
  // A span fully containing another is caught regardless of the order written.
  throwsPatch(() => apply("[a#]\nDEL 3\nSWAP 2.=5:\n+x", { a: SIX }), "operations overlap");
  // Touching-but-disjoint spans are fine.
  deepStrictEqual(
    apply("[a#]\nSWAP 2.=3:\n+X\nDEL 4.=5", { a: SIX }).get("a"),
    doc("one", "X", "six"),
  );
});

test("an INS anchored inside a replaced span is rejected", () => {
  throwsPatch(
    () => apply("[a#]\nSWAP 2.=4:\n+X\nINS.PRE 3:\n+Y", { a: SIX }),
    "anchors inside lines 2.=4",
  );
  throwsPatch(
    () => apply("[a#]\nDEL 2.=4\nINS.POST 2:\n+Y", { a: SIX }),
    "anchors inside lines 2.=4",
  );
  // The span boundaries themselves are legal: before it, and after it.
  deepStrictEqual(
    apply("[a#]\nSWAP 2.=4:\n+X\nINS.PRE 2:\n+B\nINS.POST 4:\n+A", { a: SIX }).get("a"),
    doc("one", "B", "X", "A", "five", "six"),
  );
  // And so is inserting around a single-line SWAP.
  deepStrictEqual(
    apply("[a#]\nSWAP 2:\n+X\nINS.PRE 2:\n+B\nINS.POST 2:\n+A", { a: SIX }).get("a"),
    doc("one", "B", "X", "A", "three", "four", "five", "six"),
  );
});

test("SWAP with no body is rejected — DEL is how you remove lines", () => {
  throwsPatch(() => apply("[a#]\nSWAP 2.=3:", { a: SIX }), "has no body rows");
  throwsPatch(() => apply("[a#]\nSWAP 2.=3:", { a: SIX }), "use DEL 2.=3");
});

test("a path missing from the file set is named", () => {
  throwsPatch(
    () => apply("[nope.ts#]\nDEL 1", { a: SIX }),
    "nope.ts is not in this patch's file set",
  );
});

test("checkOps is callable directly and judges one file's ops", () => {
  const ops = parsePatch("[a#]\nDEL 1.=2");
  checkOps("a", ops, 6);
  throwsPatch(() => checkOps("a", ops, 1), "is invalid");
});

// ---------------------------------------------------------------------------
// tags: explicit, chained, stale
// ---------------------------------------------------------------------------

test("an explicit tag matching the current text applies", () => {
  deepStrictEqual(
    apply(`[a#${tagOf(SIX)}]\nDEL 1`, { a: SIX }).get("a"),
    doc("two", "three", "four", "five", "six"),
  );
});

test("a patch chains: the second is written against the first's echoed tag", () => {
  const first = apply("[a#]\nDEL 1", { a: SIX }).get("a")!;
  const second = apply(`[a#${tagOf(first)}]\nSWAP 1:\n+TWO`, { a: first }).get("a");
  deepStrictEqual(second, doc("TWO", "three", "four", "five", "six"));
});

test("a stale tag is refused with the empty-tag escape hatch spelled out", () => {
  const err = throwsPatch(() => apply(`[a#${wrongTag(SIX)}]\nDEL 1`, { a: SIX }), "stale tag");
  ok(err.message.includes(`is now #${tagOf(SIX)}`), err.message);
  ok(err.message.includes('"[a#]"'), err.message);
  deepStrictEqual(err.status, 400);
});

test("a tag that does not match the recorded snapshot is stale", () => {
  const viewed = doc("alpha", "beta");
  throwsPatch(
    () => apply(`[a#${wrongTag(viewed)}]\nDEL 1`, { a: viewed }, { a: viewed }),
    "stale tag",
  );
});

test("patching a file this session never viewed is refused", () => {
  // `base` is present but missing the path: there is no version to rebase from,
  // so applying against the current text would be exactly the silent clobber.
  throwsPatch(
    () => apply("[a#]\nDEL 1", { a: SIX, b: SIX }, { b: SIX }),
    "no viewed version of a is on record",
  );
});

// ---------------------------------------------------------------------------
// rebase vs conflict — BOTH directions
// ---------------------------------------------------------------------------

const BASE4 = doc("alpha", "beta", "gamma", "delta");

test("REBASE: the file moved but the patched range is untouched", () => {
  // Another agent inserted a line at the top of the version we viewed.
  const current = doc("header", "alpha", "beta", "gamma", "delta");
  const out = apply("[a#]\nSWAP 4:\n+DELTA", { a: current }, { a: BASE4 });
  // Both edits survive: the other agent's header AND ours, correctly aimed.
  deepStrictEqual(out.get("a"), doc("header", "alpha", "beta", "gamma", "DELTA"));
});

test("REBASE: an insert in the middle shifts only the ops below it", () => {
  const current = doc("one", "two", "NEW", "three", "four", "five", "six");
  const out = apply("[a#]\nSWAP 5.=6:\n+FIVE-SIX", { a: current }, { a: SIX });
  deepStrictEqual(out.get("a"), doc("one", "two", "NEW", "three", "four", "FIVE-SIX"));
});

test("REBASE: a deletion above shifts the ops below it", () => {
  const current = doc("one", "three", "four", "five", "six");
  const out = apply("[a#]\nDEL 6", { a: current }, { a: SIX });
  deepStrictEqual(out.get("a"), doc("one", "three", "four", "five"));
});

test("REBASE: an explicit tag naming a superseded-but-known version still rebases", () => {
  const current = doc("header", "alpha", "beta", "gamma", "delta");
  const out = apply(`[a#${tagOf(BASE4)}]\nSWAP 4:\n+DELTA`, { a: current }, { a: BASE4 });
  deepStrictEqual(out.get("a"), doc("header", "alpha", "beta", "gamma", "DELTA"));
});

test("REBASE: unchanged file needs no rebase and is byte-identical elsewhere", () => {
  const out = apply("[a#]\nSWAP 4:\n+DELTA", { a: BASE4 }, { a: BASE4 });
  deepStrictEqual(out.get("a"), doc("alpha", "beta", "gamma", "DELTA"));
});

test("CONFLICT: the patched line itself was rewritten", () => {
  const current = doc("alpha", "beta", "gamma", "delta -- edited elsewhere");
  const err = throwsPatch(
    () => apply("[a#]\nSWAP 4:\n+DELTA", { a: current }, { a: BASE4 }),
    "patch conflict in a",
  );
  // Names the file, the range, and the move (spec §6: error text is a surface).
  ok(err.message.includes("lines 4.=4 were rewritten"), err.message);
  ok(err.message.includes("Someone else changed a"), err.message);
  ok(err.message.includes("Re-view a"), err.message);
  ok(err.message.includes("Nothing was written"), err.message);
});

test("CONFLICT: lines were inserted INSIDE the patched span", () => {
  // The op's footprint would now cover a line the agent never saw; rewriting it
  // would silently discard the other agent's insert.
  const current = doc("one", "two", "NEW", "three", "four", "five", "six");
  throwsPatch(
    () => apply("[a#]\nSWAP 2.=4:\n+X", { a: current }, { a: SIX }),
    "lines 2.=4 had lines inserted inside them",
  );
});

test("CONFLICT: the patched line was deleted by the other write", () => {
  const current = doc("alpha", "gamma", "delta");
  throwsPatch(
    () => apply("[a#]\nSWAP 2:\n+BETA", { a: current }, { a: BASE4 }),
    "lines 2.=2 were rewritten",
  );
});

test("CONFLICT: every conflicting range is listed, not just the first", () => {
  const current = doc("alpha!", "beta", "gamma!", "delta");
  const err = throwsPatch(
    () => apply("[a#]\nSWAP 1:\n+A\nSWAP 3:\n+G", { a: current }, { a: BASE4 }),
  );
  ok(err.message.includes("lines 1.=1"), err.message);
  ok(err.message.includes("lines 3.=3"), err.message);
});

test("CONFLICT: one touched range refuses the whole file's other, clean ops", () => {
  const current = doc("alpha", "beta!", "gamma", "delta");
  throwsPatch(
    () => apply("[a#]\nSWAP 2:\n+B\nSWAP 4:\n+D", { a: current }, { a: BASE4 }),
    "patch conflict in a",
  );
  // The clean op alone would have landed — the refusal is the conflict rule.
  deepStrictEqual(
    apply("[a#]\nSWAP 4:\n+D", { a: current }, { a: BASE4 }).get("a"),
    doc("alpha", "beta!", "gamma", "D"),
  );
});

test("INS.HEAD / INS.TAIL never conflict — they name no line", () => {
  const current = doc("totally", "different", "content");
  const out = apply("[a#]\nINS.TAIL:\n+z", { a: current }, { a: BASE4 });
  deepStrictEqual(out.get("a"), doc("totally", "different", "content", "z"));
});

test("bounds are judged in VIEWED coordinates, not the current file's", () => {
  // The other write truncated the file; our op named line 4 of what we viewed,
  // which no longer exists. That is a conflict, not an out-of-range parse error.
  const current = doc("alpha", "beta");
  throwsPatch(
    () => apply("[a#]\nSWAP 4:\n+D", { a: current }, { a: BASE4 }),
    "patch conflict in a",
  );
});

test("rebaseOps and lineMap are usable directly", () => {
  const base = ["a", "b", "c"];
  deepStrictEqual(lineMap(base, ["x", "a", "b", "c"]), [1, 2, 3]);
  deepStrictEqual(lineMap(base, ["a", "B", "c"]), [0, null, 2]);

  const good = rebaseOps(parsePatch("[f#]\nDEL 2"), base, ["x", "a", "b", "c"]);
  ok(good.ok);
  if (good.ok) deepStrictEqual([good.ops[0].a, good.ops[0].b], [3, 3]);

  const bad = rebaseOps(parsePatch("[f#]\nDEL 2"), base, ["a", "B", "c"]);
  ok(!bad.ok);
  if (!bad.ok) deepStrictEqual(bad.conflicts.length, 1);
});

// ---------------------------------------------------------------------------
// multi-file atomicity
// ---------------------------------------------------------------------------

test("multi-file: all files change together on success", () => {
  const out = apply("[a#]\nDEL 1\n\n[b#]\nINS.TAIL:\n+z", {
    a: SIX,
    b: doc("x", "y"),
    untouched: doc("keep"),
  });
  deepStrictEqual(out.get("a"), doc("two", "three", "four", "five", "six"));
  deepStrictEqual(out.get("b"), doc("x", "y", "z"));
  // Files the patch never mentions come through verbatim.
  deepStrictEqual(out.get("untouched"), doc("keep"));
});

test("multi-file: ALL or NONE — one conflict discards the whole patch", () => {
  const current = { a: SIX, b: doc("x", "CHANGED ELSEWHERE") };
  const base = { a: SIX, b: doc("x", "y") };
  const files = new Map(Object.entries(current));
  const ops = parsePatch("[a#]\nDEL 1\n\n[b#]\nSWAP 2:\n+Y");

  throwsPatch(
    () => applyPatch(files, ops, { base: new Map(Object.entries(base)) }),
    "patch conflict in b",
  );
  // The input map is untouched — a caller that writes only the return value
  // cannot have half-applied this patch.
  deepStrictEqual(files.get("a"), SIX);
  deepStrictEqual(files.get("b"), current.b);

  // The first file alone would have applied, proving the refusal is the
  // atomicity rule and not some unrelated failure.
  deepStrictEqual(
    apply("[a#]\nDEL 1", current, base).get("a"),
    doc("two", "three", "four", "five", "six"),
  );
});

test("multi-file: a later out-of-range op discards the earlier valid file", () => {
  const files = new Map(Object.entries({ a: SIX, b: doc("x") }));
  throwsPatch(
    () => applyPatch(files, parsePatch("[a#]\nDEL 1\n\n[b#]\nDEL 9"), {}),
    "out of range",
  );
  deepStrictEqual(files.get("a"), SIX);
});

test("multi-file: a stale tag on the second file discards the first", () => {
  const files = new Map(Object.entries({ a: SIX, b: doc("x") }));
  const input = `[a#]\nDEL 1\n\n[b#${wrongTag(doc("x"))}]\nDEL 1`;
  throwsPatch(() => applyPatch(files, parsePatch(input), {}), "stale tag");
  deepStrictEqual(files.get("a"), SIX);
});

// ---------------------------------------------------------------------------
// purity
// ---------------------------------------------------------------------------

test("applyPatch mutates neither argument and is repeatable", () => {
  const files = new Map(Object.entries({ a: SIX }));
  const base = new Map(Object.entries({ a: SIX }));
  const ops = parsePatch("[a#]\nSWAP 1:\n+ONE");
  const snapshot = ops.map((o) => ({ ...o, body: [...o.body] }));

  const out = applyPatch(files, ops, { base });
  deepStrictEqual(files.get("a"), SIX);
  deepStrictEqual(base.get("a"), SIX);
  ok(out !== files);
  deepStrictEqual(out.get("a"), doc("ONE", "two", "three", "four", "five", "six"));
  // The rebase does not rewrite ops in place.
  deepStrictEqual(ops, snapshot);
  // Same inputs, same answer.
  deepStrictEqual(applyPatch(files, ops, { base }).get("a"), out.get("a"));
});

test("CRLF files keep their line endings through a patch", () => {
  deepStrictEqual(
    apply("[a#]\nDEL 2", { a: "one\r\ntwo\r\nthree\r\n" }).get("a"),
    "one\r\nthree\r\n",
  );
  // …including when the patch body itself is plain LF.
  deepStrictEqual(
    apply("[a#]\nSWAP 2:\n+TWO", { a: "one\r\ntwo\r\n" }).get("a"),
    "one\r\nTWO\r\n",
  );
});

test("a file with no trailing newline keeps having none", () => {
  deepStrictEqual(apply("[a#]\nSWAP 1:\n+ONE", { a: "one\ntwo" }).get("a"), "ONE\ntwo");
});

// ---------------------------------------------------------------------------
// Regression: multi-line spans must check their INTERIOR, not just endpoints.
// Found by adversarial review. The endpoint-only check was inherited verbatim
// from src/tools/hashedit.ts and silently discarded a concurrent in-place edit
// whenever the line count was preserved — the exact lost update this module
// exists to prevent.
// ---------------------------------------------------------------------------

test("CONFLICT: a multi-line SWAP whose interior was rewritten in place", () => {
  const base = doc("X", "Y", "Z");
  const cur = doc("X", "Y-EDITED", "Z"); // another writer changed line 2; count unchanged
  const err = throwsPatch(
    () => apply("[a#]\nSWAP 1.=3:\n+N", { a: cur }, { a: base }),
    "1.=3",
  );
  ok(err.message.includes("a"), "names the file");
});

test("CONFLICT: a multi-line DEL whose interior was rewritten in place", () => {
  const base = doc("a", "b", "c", "d", "e");
  const cur = doc("a", "B!", "C!", "d", "e");
  throwsPatch(() => apply("[f#]\nDEL 1.=5", { f: cur }, { f: base }), "1.=5");
});

test("REBASE still succeeds when the span is untouched and merely shifts", () => {
  const base = doc("a", "b", "c");
  const cur = doc("header", "a", "b", "c"); // inserted ABOVE the span
  const out = apply("[g#]\nSWAP 1.=3:\n+N", { g: cur }, { g: base });
  deepStrictEqual(out.get("g"), doc("header", "N"));
});
