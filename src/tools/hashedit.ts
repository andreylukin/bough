/**
 * Hash-anchored edits: address code by LINE NUMBER bound to a CONTENT HASH.
 *
 * Two problems with exact-match edit() drove this, both observed in real
 * sessions rather than imagined:
 *
 * 1. ESCAPING. In code-mode the edit payload is JavaScript source, so matching
 *    `const abs = \`${root}/${rel}\`;` means writing that text inside the model's
 *    own template literal. One session burned six rounds on `old_string not
 *    found`, every failure caused by its own `${}` being evaluated before edit()
 *    ever saw it, and finally gave up and rewrote the whole file. Here the model
 *    never reproduces existing code — it names lines and supplies only new text,
 *    so there is nothing for its quoting to corrupt.
 *
 * 2. PARALLEL EDITS. Subagents share their spawner's checkout and fan out with
 *    Promise.all, so two agents editing one file is ordinary traffic. Line
 *    numbers alone are worthless there — a concurrent insert above shifts every
 *    anchor below it. Binding the anchors to a hash of the file the agent
 *    actually read makes a stale patch DETECTABLE, and knowing the base text
 *    makes most stale patches RECOVERABLE: if the other agent's change did not
 *    touch the lines this patch names, the anchors rebase onto the new file and
 *    both edits land. If it did, the patch is refused with the current text
 *    quoted, which the agent can act on. Silent lost updates are the one outcome
 *    this must never produce.
 *
 * The format is a subset of the `hashline` patch language:
 *
 *     [src/server/files.ts#A62C]
 *     SWAP 12.=14:
 *     +  const x = compute();
 *     +  return x;
 *     DEL 20.=21
 *     INS.POST 30:
 *     +// appended after line 30
 *
 * Line numbers are 1-based and in the coordinates of the TAGGED snapshot, never
 * of the file as it stands after earlier ops in the same patch — ops are applied
 * bottom-up so every anchor stays valid.
 *
 * This module is pure: parsing, rebasing and applying over arrays of lines. All
 * IO (and the per-session snapshot store) lives in patch_file.ts.
 */

/** Ops that carry replacement/inserted text; DEL is the one that does not. */
export type OpKind = "swap" | "del" | "ins_pre" | "ins_post" | "ins_head" | "ins_tail";

export interface Op {
  kind: OpKind;
  /** First line of the footprint (1-based), absent for ins_head/ins_tail. */
  a?: number;
  /** Last line of the footprint (1-based); equals `a` for single-line ops. */
  b?: number;
  /** Body rows, already stripped of their `+` prefix. */
  body: string[];
}

export interface Section {
  path: string;
  tag: string;
  ops: Op[];
}

/**
 * A file's tag: the low 16 bits of FNV-1a over the NORMALIZED text, as four
 * uppercase hex digits. It only ever round-trips inside this process's own
 * snapshot store — it is an identity check, not a checksum anyone else verifies —
 * so 16 bits is the right trade: short enough that the model copies it without
 * error, wide enough that an accidental collision between two versions of one
 * file is remote (and a collision degrades to a rejected patch, never a wrong one,
 * because rebasing re-checks the actual lines).
 */
export function tagOf(text: string): string {
  const norm = normalize(text);
  let h = 0x811c9dc5;
  for (let i = 0; i < norm.length; i++) {
    h ^= norm.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return ((h >>> 0) & 0xffff).toString(16).toUpperCase().padStart(4, "0");
}

/** CRLF and a leading BOM must not change a file's identity. */
export function normalize(text: string): string {
  return text.replace(/^﻿/, "").replace(/\r\n/g, "\n");
}

/** Split normalized text into lines, dropping the trailing empty element. */
export function toLines(text: string): string[] {
  const norm = normalize(text);
  const lines = norm.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/** `[path#TAG]` + `NNN:text`, the form view() hands the model. */
export function renderNumbered(path: string, text: string): string {
  const lines = toLines(text);
  const width = String(lines.length).length;
  const body = lines.map((l, i) => `${String(i + 1).padStart(width)}:${l}`).join("\n");
  return `[${path}#${tagOf(text)}]\n${body}`;
}

class PatchError extends Error {}

/** Throw with a message aimed at the model — say what to write instead. */
function bad(msg: string): never {
  throw new PatchError(msg);
}

const SECTION_RE = /^\[(.+)#([0-9a-fA-F]{4})\]$/;
// `SWAP 12.=14:` — also accepts `SWAP 12:` and the `-`/`..` range spellings,
// which weak models reach for constantly and which are unambiguous anyway.
const SWAP_RE = /^SWAP\s+(\d+)(?:\s*(?:\.=|\.\.|-|\s)\s*(\d+))?\s*:?\s*$/;
const DEL_RE = /^DEL\s+(\d+)(?:\s*(?:\.=|\.\.|-|\s)\s*(\d+))?\s*$/;
const INS_RE = /^INS\.(PRE|POST)\s+(\d+)\s*:?\s*$/;
const INS_END_RE = /^INS\.(HEAD|TAIL)\s*:?\s*$/;

/**
 * Parse one or more file sections. Throws with a corrective message on anything
 * malformed — a rejected patch the model can fix beats a partial one it cannot see.
 */
export function parsePatch(input: string): Section[] {
  const lines = normalize(input).split("\n");
  const sections: Section[] = [];
  let cur: Section | null = null;
  /** The op currently accepting `+` body rows, if any. */
  let open: Op | null = null;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    // Codex-style envelopes are common muscle memory; swallow them silently.
    if (/^\*\*\* (Begin|End) Patch\s*$/.test(line)) continue;

    if (line.startsWith("+")) {
      if (open === null) bad(`line ${i + 1}: body row "${line}" has no operation above it`);
      open.body.push(line.slice(1));
      continue;
    }
    if (line.trim() === "") {
      open = null; // a blank line ends a body; it never becomes content
      continue;
    }

    const sec = SECTION_RE.exec(line.trim());
    if (sec) {
      cur = { path: sec[1], tag: sec[2].toUpperCase(), ops: [] };
      sections.push(cur);
      open = null;
      continue;
    }
    if (!cur) {
      bad(
        `line ${i + 1}: expected a section header "[path#TAG]" before any ` +
          `operation — the TAG comes from view(path)`,
      );
    }
    if (line.startsWith("-")) {
      bad(
        `line ${i + 1}: "-" rows are not part of this format. Name the lines to ` +
          `remove with DEL, or replace them with SWAP; write literal text ` +
          `starting with "-" as a body row ("+- like this").`,
      );
    }

    let op: Op;
    let m: RegExpExecArray | null;
    if ((m = SWAP_RE.exec(line.trim()))) {
      const a = Number(m[1]);
      op = { kind: "swap", a, b: m[2] ? Number(m[2]) : a, body: [] };
    } else if ((m = DEL_RE.exec(line.trim()))) {
      const a = Number(m[1]);
      op = { kind: "del", a, b: m[2] ? Number(m[2]) : a, body: [] };
    } else if ((m = INS_RE.exec(line.trim()))) {
      const a = Number(m[2]);
      op = { kind: m[1] === "PRE" ? "ins_pre" : "ins_post", a, b: a, body: [] };
    } else if ((m = INS_END_RE.exec(line.trim()))) {
      op = { kind: m[1] === "HEAD" ? "ins_head" : "ins_tail", body: [] };
    } else {
      bad(
        `line ${i + 1}: "${line}" is not an operation. Use SWAP A.=B:, DEL A.=B, ` +
          `INS.PRE A:, INS.POST A:, INS.HEAD: or INS.TAIL:`,
      );
    }
    cur.ops.push(op);
    // DEL takes no body, so a stray `+` row after it is an error, not silent text.
    open = op.kind === "del" ? null : op;
  }
  if (!sections.length) bad('empty patch — expected at least one "[path#TAG]" section');
  for (const s of sections) {
    if (!s.ops.length) bad(`section [${s.path}#${s.tag}] has no operations`);
  }
  return sections;
}

/** Validate ranges against a file of `count` lines and reject overlaps. */
export function checkOps(ops: Op[], count: number): void {
  for (const op of ops) {
    if (op.a === undefined) continue;
    if (op.a < 1 || op.a > count) {
      bad(`line ${op.a} is out of range — the file has ${count} lines`);
    }
    if (op.b !== undefined && (op.b < op.a || op.b > count)) {
      bad(`range ${op.a}.=${op.b} is invalid for a file of ${count} lines`);
    }
    if ((op.kind === "swap" || op.kind === "del") && !op.body.length && op.kind === "swap") {
      bad(`SWAP ${op.a}.=${op.b} has no body rows — use DEL to remove lines`);
    }
  }
  // Two ops rewriting the same line is always a mistake in the model's plan, and
  // silently letting the bottom-up order pick a winner hides it.
  const spans = ops.filter((o) => o.kind === "swap" || o.kind === "del")
    .map((o) => [o.a!, o.b!] as const)
    .sort((x, y) => x[0] - y[0]);
  for (let i = 1; i < spans.length; i++) {
    if (spans[i][0] <= spans[i - 1][1]) {
      bad(
        `operations overlap: lines ${spans[i - 1][0]}.=${spans[i - 1][1]} and ` +
          `${spans[i][0]}.=${spans[i][1]} — cover each line with at most one op`,
      );
    }
  }
}

/**
 * Apply ops to `lines`, bottom-up so each anchor still means what the model meant.
 * Assumes checkOps has passed.
 */
export function applyOps(lines: string[], ops: Op[]): string[] {
  const out = lines.slice();
  // Sort by footprint descending. ins_post at line N must run before a swap
  // ending at N (it lands after that line's new text otherwise), hence the
  // kind tiebreak on equal anchors.
  const order = (o: Op) => (o.kind === "ins_post" ? 0.5 : 0);
  const sorted = ops.filter((o) => o.kind !== "ins_head" && o.kind !== "ins_tail")
    .sort((x, y) => (y.b! + order(y)) - (x.b! + order(x)));
  for (const op of sorted) {
    switch (op.kind) {
      case "swap":
        out.splice(op.a! - 1, op.b! - op.a! + 1, ...op.body);
        break;
      case "del":
        out.splice(op.a! - 1, op.b! - op.a! + 1);
        break;
      case "ins_pre":
        out.splice(op.a! - 1, 0, ...op.body);
        break;
      case "ins_post":
        out.splice(op.a!, 0, ...op.body);
        break;
    }
  }
  for (const op of ops) {
    if (op.kind === "ins_head") out.unshift(...op.body);
    if (op.kind === "ins_tail") out.push(...op.body);
  }
  return out;
}

/**
 * Map each base line number to its line number in `cur`, or null where the line
 * was changed or deleted by whoever wrote the file in the meantime.
 *
 * Common prefix/suffix are trimmed first, which is what makes this cheap: two
 * agents editing one file almost always diverge in a single small region, so the
 * LCS below runs over a handful of lines. The cap keeps a pathological diff from
 * turning into an O(n·m) stall — past it every middle line is simply reported as
 * changed, which costs a rejected patch, never a wrong one.
 */
const LCS_CAP = 400;

export function lineMap(base: string[], cur: string[]): Array<number | null> {
  const map = new Array<number | null>(base.length).fill(null);
  let p = 0;
  while (p < base.length && p < cur.length && base[p] === cur[p]) {
    map[p] = p;
    p++;
  }
  let s = 0;
  while (
    s < base.length - p && s < cur.length - p &&
    base[base.length - 1 - s] === cur[cur.length - 1 - s]
  ) {
    map[base.length - 1 - s] = cur.length - 1 - s;
    s++;
  }
  const bm = base.slice(p, base.length - s);
  const cm = cur.slice(p, cur.length - s);
  if (!bm.length || !cm.length) return map;
  if (bm.length > LCS_CAP || cm.length > LCS_CAP) return map;

  // Standard LCS table over the trimmed middles.
  const n = bm.length, m = cm.length;
  const dp: number[] = new Array((n + 1) * (m + 1)).fill(0);
  const at = (i: number, j: number) => i * (m + 1) + j;
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[at(i, j)] = bm[i] === cm[j]
        ? dp[at(i + 1, j + 1)] + 1
        : Math.max(dp[at(i + 1, j)], dp[at(i, j + 1)]);
    }
  }
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (bm[i] === cm[j]) {
      map[p + i] = p + j;
      i++;
      j++;
    } else if (dp[at(i + 1, j)] >= dp[at(i, j + 1)]) i++;
    else j++;
  }
  return map;
}

export type RebaseResult =
  | { ok: true; ops: Op[] }
  | { ok: false; conflicts: Array<{ op: Op; reason: string }> };

/**
 * Move ops from the coordinates of `base` (the text the agent read) onto `cur`
 * (the text on disk now). An op survives when every line it names is still
 * present and contiguous; otherwise it is a genuine conflict and the caller
 * reports it rather than guessing.
 */
export function rebaseOps(ops: Op[], base: string[], cur: string[]): RebaseResult {
  const map = lineMap(base, cur);
  const out: Op[] = [];
  const conflicts: Array<{ op: Op; reason: string }> = [];
  for (const op of ops) {
    if (op.kind === "ins_head" || op.kind === "ins_tail") {
      out.push(op);
      continue;
    }
    const a = map[op.a! - 1];
    const b = map[op.b! - 1];
    if (a === null || b === null) {
      conflicts.push({
        op,
        reason: `lines ${op.a}.=${op.b} were changed by the other write`,
      });
      continue;
    }
    // Contiguity matters: if someone inserted INTO the span, the op's footprint
    // now covers lines the agent never saw, and rewriting them would silently
    // discard that insert.
    if (b - a !== op.b! - op.a!) {
      conflicts.push({
        op,
        reason: `lines were inserted inside ${op.a}.=${op.b} by the other write`,
      });
      continue;
    }
    out.push({ ...op, a: a + 1, b: b + 1 });
  }
  return conflicts.length ? { ok: false, conflicts } : { ok: true, ops: out };
}

/** Re-attach the original line ending style and trailing newline. */
export function joinLines(lines: string[], original: string): string {
  const crlf = /\r\n/.test(original);
  const eol = crlf ? "\r\n" : "\n";
  const trailing = original === "" || /\n$/.test(normalize(original));
  return lines.join(eol) + (trailing ? eol : "");
}
