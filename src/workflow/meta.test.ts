/**
 * Meta extraction is pure string math over a submitted script, so this file is
 * exhaustive about the two ways it can go wrong and both are silent-failure
 * shaped: a scan that stops at the wrong brace produces a *plausible* meta from
 * half a literal, and an evaluator that runs the script produces a name that
 * changes between the run and its rerun.
 *
 * So the adversarial cases are the point: a description containing braces, a
 * template literal whose `${…}` interpolation carries braces of its own, braces
 * inside a line comment and inside a block comment, a commented-out declaration
 * above the real one — and, on the other side, every shape of computed value.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve. `node:assert` is built into the runtime and needs no fetch. (Same
 * constraint `bus.test.ts`, `paths.test.ts` and `patch.test.ts` document.)
 */

import { deepStrictEqual, match, ok, strictEqual, throws } from "node:assert";
import { WorkflowScriptError } from "../errors.ts";
import {
  extractMeta,
  metaLiteral,
  metaSpan,
  parseLiteral,
  readWorkflowMeta,
  stripMeta,
} from "./meta.ts";
import { checkWorkflowSyntax } from "./run.ts";

/** The four brace hazards in one script: string, template, line and block comment. */
const HAZARDS = `// a stray { in a line comment, plus export const meta = { decoy
/* and a { in a block comment } */
export const meta = {
  name: 'audit-handlers',            // trailing { comment }
  description: "matches {a} and {b}, 'quoted' \\"too\\"",
  phases: [
    { title: \`Review \\\${'x'} {y}\` },  /* { block } */
    { title: 'Verify', detail: 'second pass' },
  ],
}
const rest = { not: "meta" }
return rest
`;

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

Deno.test("metaLiteral: braces in a string do not end the literal", () => {
  const lit = metaLiteral(
    `export const meta = { name: 'x', description: "has {braces} and } more" }\nconst rest = 1`,
  );
  ok(lit !== null);
  ok(lit.startsWith("{") && lit.endsWith("}"));
  ok(lit.includes("has {braces} and } more"));
  ok(!lit.includes("rest"));
});

Deno.test("metaLiteral: a template literal with ${} interpolation does not end it", () => {
  // The interpolation contributes a `{` and a `}` of its own, and nests a string
  // and a template that each carry more braces. A scan that treats a backtick as an
  // ordinary quote closes at the inner backtick and truncates the literal.
  const script = "export const meta = {\n" +
    "  name: `run ${ { a: `${'}'}` }.a } {x}`,\n" +
    "  description: 'after',\n" +
    "}\nconst rest = 1\n";
  const lit = metaLiteral(script);
  ok(lit !== null);
  ok(lit.includes("description: 'after'"));
  ok(!lit.includes("rest"));
  strictEqual(lit.at(-1), "}");
});

Deno.test("metaLiteral: braces inside line and block comments are skipped", () => {
  const lit = metaLiteral(HAZARDS);
  ok(lit !== null);
  ok(lit.includes("trailing { comment }"));
  ok(lit.includes("/* { block } */"));
  ok(lit.includes("'second pass'"));
  ok(!lit.includes("not:"));
});

Deno.test("metaLiteral: a commented-out or quoted declaration is not mistaken for it", () => {
  const span = metaSpan(HAZARDS)!;
  ok(span !== null);
  // Line 3, not the decoy on line 1.
  strictEqual(HAZARDS.slice(0, span.start).split("\n").length, 3);

  const quoted = `const doc = "export const meta = { name: 'fake' }"\n` +
    `export const meta = { name: 'real', description: 'd' }\n`;
  const meta = extractMeta(quoted);
  strictEqual(meta.name, "real");

  strictEqual(metaLiteral("const meta = {}"), null); // must be exported
  strictEqual(metaLiteral("// export const meta = { name: 'x' }\nreturn 1"), null);
  strictEqual(metaLiteral("return 1"), null);
});

Deno.test("metaLiteral: an unterminated literal is an error, not a short literal", () => {
  throws(
    () => metaLiteral("export const meta = { name: 'x',\n  description: 'y'\n"),
    (err: Error) => err instanceof WorkflowScriptError && /never closed/.test(err.message),
  );
  throws(
    () => metaLiteral("export const meta = { name: 'x\n }"),
    (err: Error) => err instanceof WorkflowScriptError && /single-quoted string/.test(err.message),
  );
});

// ---------------------------------------------------------------------------
// The parse
// ---------------------------------------------------------------------------

Deno.test("extractMeta: reads the whole literal, comments and escapes and all", () => {
  const meta = extractMeta(HAZARDS);
  strictEqual(meta.name, "audit-handlers");
  strictEqual(meta.description, `matches {a} and {b}, 'quoted' "too"`);
  deepStrictEqual(meta.phases, [
    { title: "Review ${'x'} {y}" },
    { title: "Verify", detail: "second pass" },
  ]);
});

Deno.test("extractMeta: accepts an interpolation-free template and decodes escapes", () => {
  const meta = extractMeta(
    "export const meta = {\n" +
      "  name: `audit`,\n" +
      '  description: "line\\none\\ttab \\u2713 \\x41",\n' +
      "}\n",
  );
  strictEqual(meta.name, "audit");
  strictEqual(meta.description, "line\none\ttab \u2713 A");
});

Deno.test("extractMeta: trailing commas and no phases are fine", () => {
  const meta = extractMeta("export const meta = { name: 'n', description: 'd', }\n");
  strictEqual(meta.phases, undefined);
});

// ---------------------------------------------------------------------------
// Computed values — the whole reason this is a parser and not an eval
// ---------------------------------------------------------------------------

const COMPUTED: Array<[string, string, RegExp]> = [
  ["a variable", "export const meta = { name: NAME, description: 'd' }", /`NAME`/],
  ["a call", "export const meta = { name: nameFor('x'), description: 'd' }", /`nameFor`/],
  ["concatenation", "export const meta = { name: 'a' + 'b', description: 'd' }", /computed/],
  [
    "interpolation",
    "export const meta = { name: `audit ${target}`, description: 'd' }",
    /interpolation/,
  ],
  ["a spread", "export const meta = { ...defaults, description: 'd' }", /spread/],
  ["a shorthand property", "export const meta = { name, description: 'd' }", /shorthand/],
  ["a computed key", "export const meta = { [key]: 'x', description: 'd' }", /computed key/],
  ["a method", "export const meta = { name() { return 'x' }, description: 'd' }", /method/],
  [
    "a call inside phases",
    "export const meta = { name: 'n', description: 'd', phases: phasesFor(1) }",
    /`phasesFor`/,
  ],
  [
    "an expression inside an array",
    "export const meta = { name: 'n', description: 'd', phases: [{ title: 'a' }].concat(b) }",
    /computed/,
  ],
];

for (const [what, script, expected] of COMPUTED) {
  Deno.test(`extractMeta: rejects ${what}, saying why`, () => {
    throws(
      () => extractMeta(script),
      (err: Error) => {
        ok(err instanceof WorkflowScriptError, `expected WorkflowScriptError, got ${err.name}`);
        strictEqual((err as WorkflowScriptError).status, 400);
        match(err.message, expected);
        // Every computed rejection must carry the reason and the fix, not just
        // "invalid" — this message is what the author acts on (spec §6).
        match(err.message, /pure literal/);
        match(err.message, /never runs the script/);
        match(err.message, /line \d+/);
        return true;
      },
    );
  });
}

Deno.test("extractMeta: a `__proto__` key is data, never a prototype swap", () => {
  const value = parseLiteral(`{ "__proto__": { "polluted": true } }`, 0, 36) as Record<
    string,
    unknown
  >;
  strictEqual(Object.getPrototypeOf(value), Object.prototype);
  strictEqual(({} as Record<string, unknown>).polluted, undefined);
  deepStrictEqual(value.__proto__, { polluted: true });
});

// ---------------------------------------------------------------------------
// Shape validation
// ---------------------------------------------------------------------------

Deno.test("extractMeta: a missing meta names the declaration the author must write", () => {
  throws(
    () => extractMeta("phase('Review')\nreturn 1\n"),
    (err: Error) =>
      err instanceof WorkflowScriptError &&
      /export const meta = \{name, description, phases\?\}/.test(err.message),
  );
});

Deno.test("extractMeta: a wrong shape is reported per field", () => {
  throws(
    () => extractMeta("export const meta = { name: 'x' }\nreturn 1"),
    (err: Error) => err instanceof WorkflowScriptError && /invalid workflow meta/.test(err.message),
  );
  throws(
    () => extractMeta("export const meta = { name: '', description: 'd' }"),
    (err: Error) => /name:/.test(err.message),
  );
  throws(
    () => extractMeta("export const meta = { name: 'n', description: 'd', phases: [{}] }"),
    (err: Error) => /phases\.0\.title/.test(err.message),
  );
  throws(
    () => extractMeta("export const meta = { name: 'n', description: 'd', phasez: [] }"),
    // A dropped unknown key would produce a run with no phases and no complaint.
    (err: Error) => /phasez/.test(err.message),
  );
  throws(
    () => extractMeta("export const meta = { name: 42, description: 'd' }"),
    (err: Error) => /name:/.test(err.message),
  );
});

// ---------------------------------------------------------------------------
// Stripping
// ---------------------------------------------------------------------------

Deno.test("stripMeta: removes the statement and keeps every line number", () => {
  const body = stripMeta(HAZARDS);
  strictEqual(body.split("\n").length, HAZARDS.split("\n").length);
  ok(!body.includes("audit-handlers"));
  const line = (s: string, n: number) => s.split("\n")[n - 1];
  // The statement's lines (3–10) are blanked, not deleted...
  for (let n = 3; n <= 10; n++) strictEqual(line(body, n).trim(), "");
  // ...and everything outside it survives verbatim, on its original line — the
  // decoy comment on line 1 included, since it is the body's text, not meta's.
  for (const n of [1, 2, 11, 12]) strictEqual(line(body, n), line(HAZARDS, n));
  ok(body.includes(`const rest = { not: "meta" }`));
});

Deno.test("stripMeta: the stripped body is what the workflow worker can compile", () => {
  // `export` is illegal inside the function body the worker builds, so this is the
  // property that matters — asserted against the real pre-flight from `run.ts`.
  ok(checkWorkflowSyntax(HAZARDS) !== null, "the raw script must not compile as a body");
  strictEqual(checkWorkflowSyntax(stripMeta(HAZARDS)), null);
});

Deno.test("stripMeta: a script with no meta is returned untouched", () => {
  const script = "phase('Review')\nreturn 1\n";
  strictEqual(stripMeta(script), script);
});

Deno.test("readWorkflowMeta: validated meta plus the runnable body, in one pass", () => {
  const { meta, body } = readWorkflowMeta(HAZARDS);
  strictEqual(meta.name, "audit-handlers");
  strictEqual(meta.phases?.length, 2);
  strictEqual(checkWorkflowSyntax(body), null);
});
