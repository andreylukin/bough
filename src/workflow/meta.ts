/**
 * `export const meta = {…}` — read out of a workflow script WITHOUT running it.
 *
 * WHY THIS EXISTS. A run's name, description and phase list have to be known
 * before the script executes: they are what the run row is created with, what the
 * run view labels its phases from, and what a rerun inherits when the author edited
 * only the body (`workflow/run.ts`). The script itself runs detached, minutes to
 * hours later, in the workflow worker — so "just run it and read the
 * export" is not available at submit time, and even if it were, a script whose
 * `meta` came from a function call would report one name on the run and a different
 * one on the rerun.
 *
 * THE INVARIANT THIS HOLDS: **`meta` is a pure literal, located by a scan that
 * cannot be derailed by the script's own text, and evaluated by a parser that
 * cannot execute anything.** Two halves, and both are load-bearing:
 *
 *   1. *Finding it* is a balanced-brace scan that skips string bodies, template
 *      bodies (including nested `${…}` interpolations, which contain braces),
 *      line comments and block comments (plan §6, invariant 13). The naive
 *      `indexOf("}")` — or a regex — stops at the first brace inside
 *      `description: "handles {a} and {b}"` and hands the rest of the script to
 *      the parser as garbage. So does a `// TODO: export const meta = {` sitting
 *      above the real declaration, which is why the DECLARATION is located by the
 *      same skipping scan rather than by a bare regex over the whole file.
 *
 *   2. *Reading it* is a recursive-descent parser over object/array/string/
 *      number/boolean/null literals and nothing else. `name: NAME`,
 *      `description: head + tail`, `` `audit ${target}` ``, `phases: phasesFor(x)`
 *      and `{ ...defaults }` are all REJECTED, each with a message saying why —
 *      the host never runs the script, so a computed value is not a thing it can
 *      resolve. The old implementation eval'd the literal in a throwaway worker
 *      (`src/workflow.ts`), which turned `name: NAME` into a bare `ReferenceError`
 *      and silently accepted `'a' + 'b'` — different every run, and rerun keys are
 *      built from what the script asks for.
 *
 * Pure, synchronous, no worker, no clock, no filesystem: the whole module is string
 * math over a submitted script, which is what lets the submit boundary reject a bad
 * script in the same request that posted it.
 *
 * Ported from `src/workflow.ts` (`metaLiteral` / `evalMeta`). Deltas from that port
 * are marked `NOTE:`.
 */

import { z } from "zod";
import { WorkflowScriptError } from "../errors.ts";
import { WorkflowPhase } from "../schema/parts.ts";

// ---------------------------------------------------------------------------
// The validated shape
// ---------------------------------------------------------------------------

/**
 * What a script must declare. `.strict()` on both objects because a silently
 * dropped key is the worst outcome here: `phasez: [...]` stripped as unknown
 * produces a run with no phases and no complaint, and the author debugs the run
 * view instead of the typo.
 */
export const WorkflowMeta = z.object({
  name: z.string().min(1).max(80),
  description: z.string().min(1).max(500),
  phases: z.array(WorkflowPhase.strict()).optional(),
}).strict();

/** Structurally identical to `run.ts`'s `WorkflowMetaInput` — pass it straight in. */
export type WorkflowMeta = z.infer<typeof WorkflowMeta>;

/** Where the `export const meta = {…}` statement sits in the script. */
export interface MetaSpan {
  /** Offset of `export`. */
  start: number;
  /** Offset of the opening `{`. */
  literalStart: number;
  /** Offset just past the closing `}`. */
  end: number;
  /** The literal text, `{` through `}` inclusive. */
  literal: string;
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/** 1-based line of an offset — every message points at a line the author can see. */
function lineOf(src: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index && i < src.length; i++) if (src[i] === "\n") line++;
  return line;
}

/**
 * The one message for every computed value, because the author's next move is the
 * same in every case and the *reason* is the part they are missing: meta is read,
 * never executed.
 */
function computed(what: string, src: string, at: number): never {
  throw new WorkflowScriptError(
    `workflow meta must be a pure literal — ${what} on line ${lineOf(src, at)} is computed. ` +
      `The host reads \`meta\` by scanning the source and never runs the script, so it ` +
      `cannot resolve variables, calls, operators, spreads or \`\${…}\` interpolation. ` +
      `Write the value out literally (name: 'audit-handlers'), and compute whatever is ` +
      `dynamic inside the script body, where it runs.`,
  );
}

function malformed(what: string, src: string, at: number): never {
  throw new WorkflowScriptError(
    `workflow meta does not parse: ${what} on line ${lineOf(src, at)}. ` +
      `\`meta\` must be a literal object: {name, description, phases?: [{title, detail?}]}.`,
  );
}

// ---------------------------------------------------------------------------
// The scan (half 1: find it, undeceived by the script's own text)
// ---------------------------------------------------------------------------

const IDENT = /[A-Za-z0-9_$]/;
/** Sticky: tested at an offset the scanner already knows is real code. */
const DECL = /export[ \t\r\n]+const[ \t\r\n]+meta[ \t\r\n]*=[ \t\r\n]*\{/y;

/**
 * Consume a `'`/`"` string starting at its opening quote. Returns the offset just
 * past the closing quote, or -1 when it is unterminated — including by a raw
 * newline, which in JS closes nothing and is the single most common way a
 * hand-written script's brace balance goes wrong.
 */
function skipQuoted(src: string, at: number): number {
  const quote = src[at];
  for (let i = at + 1; i < src.length; i++) {
    const c = src[i];
    if (c === "\\") {
      i++;
      continue;
    }
    if (c === "\n") return -1;
    if (c === quote) return i + 1;
  }
  return -1;
}

/**
 * Frames of the scan. A template body is its own mode — braces inside it are text —
 * and each `${…}` opens a fresh CODE frame with its own brace depth, so the `}` that
 * closes an interpolation is never mistaken for the `}` that closes `meta`.
 *
 * NOTE: the port treated a backtick as an ordinary quote, which is correct for
 * `` `t {y}` `` (the field case in `src/workflow.test.ts`) and wrong the moment an
 * interpolation contains a nested template or a brace-bearing string.
 */
type Frame = { kind: "code"; depth: number } | { kind: "template" };

/**
 * From the opening `{` at `start`, return the offset just past its matching `}`.
 * Throws when the literal never closes — a truncated paste, or a string closed by a
 * newline — rather than returning a silently short literal.
 */
export function scanBalanced(src: string, start: number): number {
  const stack: Frame[] = [{ kind: "code", depth: 0 }];
  let i = start;
  while (i < src.length) {
    const top = stack[stack.length - 1];
    const c = src[i];

    if (top.kind === "template") {
      if (c === "\\") {
        i += 2;
        continue;
      }
      if (c === "`") {
        stack.pop();
        i++;
        continue;
      }
      if (c === "$" && src[i + 1] === "{") {
        stack.push({ kind: "code", depth: 0 });
        i += 2;
        continue;
      }
      i++;
      continue;
    }

    if (c === '"' || c === "'") {
      const next = skipQuoted(src, i);
      if (next < 0) {
        malformed(`a ${c === '"' ? "double" : "single"}-quoted string is never closed`, src, i);
      }
      i = next;
      continue;
    }
    if (c === "`") {
      stack.push({ kind: "template" });
      i++;
      continue;
    }
    if (c === "/" && src[i + 1] === "/") {
      const nl = src.indexOf("\n", i);
      if (nl < 0) break; // comment runs to EOF: the literal never closes
      i = nl + 1;
      continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      const end = src.indexOf("*/", i + 2);
      if (end < 0) malformed("a block comment is never closed", src, i);
      i = end + 2;
      continue;
    }
    if (c === "{") {
      top.depth++;
      i++;
      continue;
    }
    if (c === "}") {
      if (top.depth === 0) {
        // Closes the `${…}` this frame was opened by.
        if (stack.length > 1) {
          stack.pop();
          i++;
          continue;
        }
        malformed("an unbalanced `}`", src, i);
      }
      top.depth--;
      if (top.depth === 0 && stack.length === 1) return i + 1;
      i++;
      continue;
    }
    i++;
  }
  malformed("the `meta` literal is never closed", src, start);
}

/**
 * Locate the `export const meta = {…}` statement, skipping string bodies, template
 * bodies and comments so a commented-out or quoted declaration cannot be mistaken
 * for the real one. `null` when the script declares none.
 */
export function metaSpan(script: string): MetaSpan | null {
  const stack: Frame[] = [{ kind: "code", depth: 0 }];
  let i = 0;
  while (i < script.length) {
    const top = stack[stack.length - 1];
    const c = script[i];

    if (top.kind === "template") {
      if (c === "\\") {
        i += 2;
        continue;
      }
      if (c === "`") {
        stack.pop();
        i++;
        continue;
      }
      if (c === "$" && script[i + 1] === "{") {
        stack.push({ kind: "code", depth: 0 });
        i += 2;
        continue;
      }
      i++;
      continue;
    }

    if (c === '"' || c === "'") {
      const next = skipQuoted(script, i);
      // An unterminated string outside `meta` is the body's problem; the syntax
      // check in `run.ts` names it precisely. Here it just ends the search.
      if (next < 0) return null;
      i = next;
      continue;
    }
    if (c === "`") {
      stack.push({ kind: "template" });
      i++;
      continue;
    }
    if (c === "/" && script[i + 1] === "/") {
      const nl = script.indexOf("\n", i);
      if (nl < 0) return null;
      i = nl + 1;
      continue;
    }
    if (c === "/" && script[i + 1] === "*") {
      const end = script.indexOf("*/", i + 2);
      if (end < 0) return null;
      i = end + 2;
      continue;
    }
    if (c === "}" && top.depth === 0 && stack.length > 1) {
      stack.pop();
      i++;
      continue;
    }
    if (c === "e" && !IDENT.test(script[i - 1] ?? "")) {
      DECL.lastIndex = i;
      if (DECL.test(script)) {
        const literalStart = DECL.lastIndex - 1;
        const end = scanBalanced(script, literalStart);
        return { start: i, literalStart, end, literal: script.slice(literalStart, end) };
      }
    }
    if (c === "{") top.depth++;
    else if (c === "}") top.depth--;
    i++;
  }
  return null;
}

/**
 * The literal text of `export const meta = {…}`, `{` through `}`, or `null`.
 * Kept from the port (`metaLiteral`) as the narrow, testable half of the scan.
 */
export function metaLiteral(script: string): string | null {
  return metaSpan(script)?.literal ?? null;
}

// ---------------------------------------------------------------------------
// The parser (half 2: read it without executing anything)
// ---------------------------------------------------------------------------

/** Literals nest three deep at most; anything deeper is a script, not a literal. */
const MAX_DEPTH = 16;

interface Cursor {
  src: string;
  i: number;
  end: number;
}

/** Whitespace and comments between tokens — the only things allowed to be nothing. */
function trivia(cur: Cursor): void {
  while (cur.i < cur.end) {
    const c = cur.src[cur.i];
    if (c === " " || c === "\t" || c === "\r" || c === "\n") {
      cur.i++;
      continue;
    }
    if (c === "/" && cur.src[cur.i + 1] === "/") {
      const nl = cur.src.indexOf("\n", cur.i);
      cur.i = nl < 0 || nl > cur.end ? cur.end : nl + 1;
      continue;
    }
    if (c === "/" && cur.src[cur.i + 1] === "*") {
      const close = cur.src.indexOf("*/", cur.i + 2);
      if (close < 0) malformed("a block comment is never closed", cur.src, cur.i);
      cur.i = close + 2;
      continue;
    }
    return;
  }
}

/** JS string escapes, decoded. An unknown escape is its own character, as in JS. */
function unescape(src: string, from: number, to: number): string {
  let out = "";
  for (let i = from; i < to; i++) {
    const c = src[i];
    if (c !== "\\") {
      out += c;
      continue;
    }
    const e = src[++i];
    switch (e) {
      case "n":
        out += "\n";
        break;
      case "t":
        out += "\t";
        break;
      case "r":
        out += "\r";
        break;
      case "b":
        out += "\b";
        break;
      case "f":
        out += "\f";
        break;
      case "v":
        out += "\v";
        break;
      case "0":
        out += "\0";
        break;
      case "\n":
        break; // line continuation
      case "x": {
        out += String.fromCharCode(parseInt(src.slice(i + 1, i + 3), 16));
        i += 2;
        break;
      }
      case "u": {
        if (src[i + 1] === "{") {
          const close = src.indexOf("}", i);
          out += String.fromCodePoint(parseInt(src.slice(i + 2, close), 16));
          i = close;
        } else {
          out += String.fromCharCode(parseInt(src.slice(i + 1, i + 5), 16));
          i += 4;
        }
        break;
      }
      default:
        out += e ?? "";
    }
  }
  return out;
}

/** A quoted or backtick string. A template carrying `${…}` is computed, not a value. */
function parseString(cur: Cursor): string {
  const at = cur.i;
  const quote = cur.src[at];
  if (quote === "`") {
    for (let i = at + 1; i < cur.end; i++) {
      const c = cur.src[i];
      if (c === "\\") {
        i++;
        continue;
      }
      if (c === "$" && cur.src[i + 1] === "{") {
        computed("a template literal with `${…}` interpolation", cur.src, i);
      }
      if (c === "`") {
        const text = unescape(cur.src, at + 1, i);
        cur.i = i + 1;
        return text;
      }
    }
    malformed("a template literal is never closed", cur.src, at);
  }
  const next = skipQuoted(cur.src, at);
  if (next < 0 || next > cur.end) malformed("a string is never closed", cur.src, at);
  cur.i = next;
  return unescape(cur.src, at + 1, next - 1);
}

const NUMBER = /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/y;
const KEY = /[A-Za-z_$][A-Za-z0-9_$]*/y;

function parseValue(cur: Cursor, depth: number): unknown {
  if (depth > MAX_DEPTH) malformed("the literal nests too deeply", cur.src, cur.i);
  trivia(cur);
  if (cur.i >= cur.end) malformed("the literal ends early", cur.src, cur.i);
  const at = cur.i;
  const c = cur.src[at];

  if (c === "{") return parseObject(cur, depth);
  if (c === "[") return parseArray(cur, depth);
  if (c === '"' || c === "'" || c === "`") return parseString(cur);

  if (c === "-" || (c >= "0" && c <= "9")) {
    NUMBER.lastIndex = at;
    const m = NUMBER.exec(cur.src);
    if (!m || m.index !== at) computed("a numeric expression", cur.src, at);
    cur.i = at + m[0].length;
    return Number(m[0]);
  }

  KEY.lastIndex = at;
  const word = KEY.exec(cur.src);
  if (word && word.index === at) {
    cur.i = at + word[0].length;
    if (word[0] === "true") return true;
    if (word[0] === "false") return false;
    if (word[0] === "null") return null;
    if (word[0] === "undefined") {
      computed("`undefined`", cur.src, at);
    }
    // A bare name: a variable read, or the callee of a call. Both are the same
    // mistake and the same fix.
    computed(`\`${word[0]}\``, cur.src, at);
  }
  if (c === ".") computed("a `...` spread", cur.src, at);
  computed(`\`${cur.src.slice(at, Math.min(at + 16, cur.end)).split("\n")[0]}\``, cur.src, at);
}

function parseArray(cur: Cursor, depth: number): unknown[] {
  const out: unknown[] = [];
  cur.i++; // [
  for (;;) {
    trivia(cur);
    if (cur.i >= cur.end) malformed("an array is never closed", cur.src, cur.i);
    if (cur.src[cur.i] === "]") {
      cur.i++;
      return out;
    }
    if (cur.src[cur.i] === ",") {
      // `[a, , b]` is a hole — legal JS, not expressible in the meta we accept.
      malformed("an array hole (`,,`)", cur.src, cur.i);
    }
    out.push(parseValue(cur, depth + 1));
    trivia(cur);
    if (cur.src[cur.i] === ",") {
      cur.i++;
      continue;
    }
    if (cur.src[cur.i] === "]") {
      cur.i++;
      return out;
    }
    // Anything else here is an operator joining two values: `a + b`, `xs.concat(y)`.
    computed("an expression", cur.src, cur.i);
  }
}

function parseObject(cur: Cursor, depth: number): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  cur.i++; // {
  for (;;) {
    trivia(cur);
    if (cur.i >= cur.end) malformed("an object is never closed", cur.src, cur.i);
    if (cur.src[cur.i] === "}") {
      cur.i++;
      return out;
    }

    const keyAt = cur.i;
    const c = cur.src[keyAt];
    if (c === ".") computed("a `...` spread", cur.src, keyAt);
    if (c === "[") computed("a computed key `[…]`", cur.src, keyAt);

    let key: string;
    if (c === '"' || c === "'" || c === "`") {
      key = parseString(cur);
    } else {
      KEY.lastIndex = keyAt;
      const m = KEY.exec(cur.src);
      if (!m || m.index !== keyAt) malformed("a property name was expected", cur.src, keyAt);
      key = m[0];
      cur.i = keyAt + m[0].length;
    }

    trivia(cur);
    const after = cur.src[cur.i];
    if (after === "(") {
      computed(`the method \`${key}()\``, cur.src, cur.i);
    }
    if (after === "," || after === "}") {
      computed(`the shorthand property \`${key}\``, cur.src, keyAt);
    }
    if (after !== ":") malformed("a `:` was expected after a property name", cur.src, cur.i);
    cur.i++;

    const value = parseValue(cur, depth + 1);
    // defineProperty, not assignment: a literal `__proto__` key must be data here,
    // not a prototype swap on the object we are about to hand to Zod.
    Object.defineProperty(out, key, {
      value,
      enumerable: true,
      writable: true,
      configurable: true,
    });

    trivia(cur);
    if (cur.src[cur.i] === ",") {
      cur.i++;
      continue;
    }
    if (cur.src[cur.i] === "}") {
      cur.i++;
      return out;
    }
    computed("an expression", cur.src, cur.i);
  }
}

/**
 * Parse one pure JS literal out of `src[start..end)`. Exported for the tests and for
 * anything else that needs "read this literal, run nothing".
 */
export function parseLiteral(src: string, start: number, end: number): unknown {
  const cur: Cursor = { src, i: start, end };
  const value = parseValue(cur, 0);
  trivia(cur);
  if (cur.i < end) malformed("trailing text after the literal", src, cur.i);
  return value;
}

// ---------------------------------------------------------------------------
// The boundary
// ---------------------------------------------------------------------------

function formatIssues(error: z.ZodError): string {
  return error.issues
    .map((issue) => `${issue.path.length ? issue.path.join(".") : "meta"}: ${issue.message}`)
    .join("; ");
}

/**
 * Extract and validate a script's `meta`. Throws `WorkflowScriptError` (400) with a
 * message the author can act on: missing, computed, unparseable, or shaped wrong.
 */
export function extractMeta(script: string): WorkflowMeta {
  const span = metaSpan(script);
  if (!span) {
    throw new WorkflowScriptError(
      "workflow script must declare `export const meta = {name, description, phases?}` " +
        "as a pure literal. The host reads it without running the script — it names the " +
        "run and labels its phases before the first agent starts.",
    );
  }
  const raw = parseLiteral(script, span.literalStart, span.end);
  const parsed = WorkflowMeta.safeParse(raw);
  if (!parsed.success) {
    throw new WorkflowScriptError(
      `invalid workflow meta (line ${lineOf(script, span.start)}): ` +
        `${formatIssues(parsed.error)} — meta is ` +
        `{name, description, phases?: [{title, detail?}]}.`,
    );
  }
  return parsed.data;
}

/**
 * The script with its `meta` statement blanked out — the body the worker runs.
 *
 * Blanked, not deleted: every character is replaced by a space and every newline
 * kept, so a syntax error's line and column still match the script the author
 * wrote and the file mirrored to `~/.bough/workflows/<id>.js`. Removing the
 * statement is what makes the body compilable at all: `export` is illegal inside
 * the function body the workflow worker builds.
 *
 * NOTE: `run.ts`'s `workflowBody` reaches the same place by demoting `export const
 * meta =` to `const meta =`, and `startWorkflow` applies it itself — so a caller
 * that hands `startWorkflow` the ORIGINAL script is already covered, and should,
 * since the run row persists that script for rerun. Use this when building a body
 * directly (a syntax probe, a preview) and you want no `meta` binding at all.
 */
export function stripMeta(script: string): string {
  const span = metaSpan(script);
  if (!span) return script;
  const blanked = script.slice(span.start, span.end).replace(/[^\n]/g, " ");
  return script.slice(0, span.start) + blanked + script.slice(span.end);
}

/** Both halves at once: the validated meta and the body to run. */
export function readWorkflowMeta(script: string): { meta: WorkflowMeta; body: string } {
  return { meta: extractMeta(script), body: stripMeta(script) };
}
