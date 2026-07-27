/**
 * The patch engine — hash-anchored line edits, computed as pure data.
 *
 * WHY THIS EXISTS. Subagents share their spawner's checkout (spec §7). There are
 * no worktrees, no leases, and no merge step: two agents editing one file is
 * ordinary traffic. Line numbers alone are worthless under that regime — a
 * concurrent insert above shifts every anchor below it — so every patch is bound
 * to a TAG naming the exact version the agent read. That binding is what makes a
 * stale patch *detectable*, and knowing the base text is what makes most stale
 * patches *recoverable*.
 *
 * THE INVARIANT THIS HOLDS: a patch never silently lands on text its author did
 * not see. Three rules, in order of importance:
 *
 *   1. **Rebase or refuse, never guess.** If the file changed since the tag but
 *      none of the patched line ranges were touched, the anchors rebase onto the
 *      new version and both edits land. If a patched range *was* touched, the
 *      patch is refused with the file and the range named, so the next round
 *      re-views instead of retrying blind. A silent lost update is the one
 *      outcome this module must never produce.
 *   2. **Viewed coordinates.** Every line number is in the coordinates of the
 *      version the agent viewed. Edits are collected against the original and the
 *      result is assembled in ONE pass (`materialize`); nothing is ever applied
 *      sequentially, so an earlier op in the same patch cannot shift a later op's
 *      anchor.
 *   3. **All or none.** A multi-file patch that fails on its third file leaves the
 *      first two untouched. `applyPatch` builds a new map and throws before
 *      returning it, so failure is indistinguishable from never having been
 *      called.
 *
 * The module is pure: strings and arrays in, strings and arrays out. No IO, no
 * clock, no snapshot store. Resolving a TAG to the text it names is the caller's
 * job (`hostfn/files.ts`) — see `ApplyOptions.base`.
 *
 * Ported from `src/tools/hashedit.ts`, which is where the conflict rules were
 * learned. Deltas from that port are marked `NOTE:` below.
 */

import { PatchError } from "../errors.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/** The six operations. `del` is the only one that carries no body. */
export type OpKind = "swap" | "del" | "ins_pre" | "ins_post" | "ins_head" | "ins_tail";

/**
 * One operation, already bound to the file section it was written under.
 *
 * `parsePatch` returns a flat list rather than a tree so callers can filter and
 * count without walking; `groupByFile` reassembles the per-file view when the
 * caller needs the `(path, tag)` pair to resolve a snapshot.
 */
export interface PatchOp {
  /** The path from the enclosing `[path#TAG]` header, verbatim. */
  path: string;
  /**
   * The four-hex version this op is written against, uppercased, or `""` for the
   * tagless form (`[path#]` / `[path]`) meaning "whatever I just viewed". Tagless
   * costs nothing in safety — the caller resolves it from the same snapshot a
   * rebase would use — and it spares the model from copying a hash out of
   * `view()`'s output, which measurably cost it rounds when it had to.
   */
  tag: string;
  kind: OpKind;
  /** First line of the footprint (1-based). Absent for `ins_head`/`ins_tail`. */
  a?: number;
  /** Last line of the footprint (1-based); equals `a` for single-line ops. */
  b?: number;
  /** Body rows with their `+` prefix stripped. A lone `+` yields `""`. */
  body: string[];
  /** 1-based line in the patch input this op was written on, for error text. */
  at: number;
}

/** One file's worth of operations, in the order they were written. */
export interface FileOps {
  path: string;
  tag: string;
  ops: PatchOp[];
}

export interface ApplyOptions {
  /**
   * `path` → the exact text the agent VIEWED, i.e. the snapshot the section's tag
   * names. The caller resolves this from its per-session snapshot store before
   * calling; that is the only piece of state the patch engine cannot compute.
   *
   * Three behaviours, chosen deliberately:
   *
   *   - **Map supplied, path present** — that text is the rebase base. If the
   *     section carried an explicit tag it must match, or the patch is refused as
   *     stale.
   *   - **Map supplied, path absent** — refused. The agent patched a file this
   *     session never viewed, so there is no base to rebase from and applying
   *     against the current text would be exactly the silent clobber this module
   *     exists to prevent.
   *   - **Map omitted entirely** — no snapshot store is in play (unit tests, or a
   *     caller that has just read the file itself), so the current text is the
   *     base and no rebase is possible. An explicit tag is still verified against
   *     the current text.
   */
  base?: ReadonlyMap<string, string>;
}

// ---------------------------------------------------------------------------
// Tags and text normalization
// ---------------------------------------------------------------------------

/** CRLF and a leading BOM must not change a file's identity. */
export function normalize(text: string): string {
  return text.replace(/^﻿/, "").replace(/\r\n/g, "\n");
}

/**
 * A file's tag: the low 16 bits of FNV-1a over the NORMALIZED text, as four
 * uppercase hex digits.
 *
 * It only ever round-trips inside this process's own snapshot store — it is an
 * identity check, not a checksum anyone else verifies — so 16 bits is the right
 * trade: short enough that the model copies it without error, wide enough that an
 * accidental collision between two versions of one file is remote. And a
 * collision degrades to a *rejected* patch, never a wrong one, because the rebase
 * re-checks the actual lines rather than trusting the tag.
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

/** Split normalized text into lines, dropping the trailing empty element. */
export function toLines(text: string): string[] {
  const norm = normalize(text);
  const lines = norm.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/**
 * Re-attach the original line-ending style and trailing newline.
 *
 * NOTE: unlike the port, a file emptied by a patch comes back as `""` rather than
 * a lone newline — deleting every line should not leave a blank one behind.
 */
export function joinLines(lines: string[], original: string): string {
  if (lines.length === 0) return "";
  const eol = /\r\n/.test(original) ? "\r\n" : "\n";
  const trailing = original === "" || /\n$/.test(normalize(original));
  return lines.join(eol) + (trailing ? eol : "");
}

/** `[path#TAG]` + `NNN:text` — the form `view()` hands the model. */
export function renderNumbered(path: string, text: string): string {
  const lines = toLines(text);
  const width = String(lines.length).length;
  const body = lines.map((l, i) => `${String(i + 1).padStart(width)}:${l}`).join("\n");
  return `[${path}#${tagOf(text)}]\n${body}`;
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

// `[path#TAG]`, plus the tagless `[path#]` / `[path]`. The path is lazy so a
// four-hex tag, when present, wins the trailing segment rather than being
// swallowed into the path.
const SECTION_RE = /^\[(.+?)(?:#([0-9a-fA-F]{4})?)?\]$/;
// `12:const x = 1;` — the shape of view()'s own listing. Recognised only so it can
// be named: pasting view()'s whole output back into patch() is the most natural
// mistake this format invites, and a generic parse error sent one subagent through
// three rounds of guessing before it found the tag.
const NUMBERED_LINE_RE = /^\s*\d+:/;
// `SWAP 12.=14:` — also accepts `SWAP 12:` and the `-` / `..` / bare-space range
// spellings, which weaker models reach for constantly and which are unambiguous.
const SWAP_RE = /^SWAP\s+(\d+)(?:\s*(?:\.=|\.\.|-|\s)\s*(\d+))?\s*:?\s*$/;
const DEL_RE = /^DEL\s+(\d+)(?:\s*(?:\.=|\.\.|-|\s)\s*(\d+))?\s*$/;
const INS_RE = /^INS\.(PRE|POST)\s+(\d+)\s*:?\s*$/;
const INS_END_RE = /^INS\.(HEAD|TAIL)\s*:?\s*$/;
// Codex-style envelopes are common muscle memory; swallow them silently.
const ENVELOPE_RE = /^\*\*\* (Begin|End) Patch\s*$/;

/**
 * Parse one or more file sections into a flat op list.
 *
 * Throws `PatchError` with a corrective message on anything malformed — a
 * rejected patch the model can fix beats a partially-understood one it cannot
 * see. Every message names the input line, what was wrong, and what to write.
 */
export function parsePatch(input: string): PatchOp[] {
  const lines = normalize(input).split("\n");
  const ops: PatchOp[] = [];
  /** Sections seen, so a header with no operations can be reported. */
  const seen: Array<{ path: string; tag: string; count: number }> = [];
  let cur: { path: string; tag: string; count: number } | null = null;
  /** The op currently accepting `+` body rows, if any. */
  let open: PatchOp | null = null;
  /** Set when the previous op was a DEL, so a stray body row says why. */
  let lastWasDel = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const at = i + 1;
    if (ENVELOPE_RE.test(line)) continue;

    if (line.startsWith("+")) {
      if (open === null) {
        if (lastWasDel) {
          bad(
            `line ${at}: DEL takes no body rows — it only removes lines. To ` +
              `replace lines, use SWAP A.=B: with the new text below it.`,
          );
        }
        bad(
          `line ${at}: body row "${trunc(line)}" has no operation above it. A ` +
            `"+" row must follow SWAP, INS.PRE, INS.POST, INS.HEAD or INS.TAIL.`,
        );
      }
      open.body.push(line.slice(1));
      continue;
    }
    if (line.trim() === "") {
      open = null; // a blank line ends a body; it never becomes content
      continue;
    }

    const trimmed = line.trim();
    const sec = SECTION_RE.exec(trimmed);
    if (sec) {
      const path = sec[1].trim();
      if (!path) bad(`line ${at}: section header "${trimmed}" has no path`);
      cur = { path, tag: (sec[2] ?? "").toUpperCase(), count: 0 };
      seen.push(cur);
      open = null;
      lastWasDel = false;
      continue;
    }
    if (!cur) {
      bad(
        `line ${at}: expected a section header "[path#TAG]" before any ` +
          `operation — the TAG comes from view(path), and "[path#]" with an ` +
          `empty tag means the version you just viewed.`,
      );
    }
    if (line.startsWith("-")) {
      bad(
        `line ${at}: "-" rows are not part of this format. Name the lines to ` +
          `remove with DEL, or replace them with SWAP; write literal text ` +
          `starting with "-" as a body row ("+- like this").`,
      );
    }
    if (NUMBERED_LINE_RE.test(line)) {
      bad(
        `line ${at}: "${trunc(line)}" looks like a line from view()'s listing. ` +
          `Do not pass view()'s output to patch() — the listing is for you to ` +
          `read. Write only the section header and your operations; the header ` +
          `may be just "[${cur.path}#]" to mean the version you viewed.`,
      );
    }

    let op: PatchOp;
    let m: RegExpExecArray | null;
    const base = { path: cur.path, tag: cur.tag, body: [] as string[], at };
    if ((m = SWAP_RE.exec(trimmed))) {
      const a = Number(m[1]);
      op = { ...base, kind: "swap", a, b: m[2] ? Number(m[2]) : a };
    } else if ((m = DEL_RE.exec(trimmed))) {
      const a = Number(m[1]);
      op = { ...base, kind: "del", a, b: m[2] ? Number(m[2]) : a };
    } else if ((m = INS_RE.exec(trimmed))) {
      const a = Number(m[2]);
      op = { ...base, kind: m[1] === "PRE" ? "ins_pre" : "ins_post", a, b: a };
    } else if ((m = INS_END_RE.exec(trimmed))) {
      op = { ...base, kind: m[1] === "HEAD" ? "ins_head" : "ins_tail" };
    } else {
      bad(
        `line ${at}: "${trunc(line)}" is not an operation. Use SWAP A.=B:, ` +
          `DEL A.=B, INS.PRE A:, INS.POST A:, INS.HEAD: or INS.TAIL:`,
      );
    }
    ops.push(op);
    cur.count++;
    // DEL takes no body, so a stray `+` row after it is an error, not silent text.
    lastWasDel = op.kind === "del";
    open = lastWasDel ? null : op;
  }

  if (!seen.length) bad('empty patch — expected at least one "[path#TAG]" section');
  for (const s of seen) {
    if (!s.count) bad(`section [${s.path}#${s.tag}] has no operations`);
  }
  return ops;
}

/**
 * Regroup a flat op list by file, preserving first-appearance order.
 *
 * Two sections naming one path are merged, which is how a model that writes the
 * header twice still gets one coherent edit. They must agree on the tag: two
 * different base versions of one file in a single patch is a plan the engine
 * cannot honour, and guessing which one wins is exactly the silent-clobber shape.
 */
export function groupByFile(ops: PatchOp[]): FileOps[] {
  const order: string[] = [];
  const byPath = new Map<string, FileOps>();
  for (const op of ops) {
    let g = byPath.get(op.path);
    if (!g) {
      g = { path: op.path, tag: op.tag, ops: [] };
      byPath.set(op.path, g);
      order.push(op.path);
    } else if (g.tag !== op.tag) {
      bad(
        `${op.path} appears twice with different tags ("[${op.path}#${g.tag}]" ` +
          `and "[${op.path}#${op.tag}]"). One file has one base version per ` +
          `patch — use a single section, or an empty tag "[${op.path}#]".`,
      );
    }
    g.ops.push(op);
  }
  return order.map((p) => byPath.get(p)!);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/**
 * Validate one file's ops against a file of `count` lines, in the coordinates the
 * ops are written in. Rejects out-of-range anchors, inverted ranges, bodiless
 * SWAPs, overlapping spans, and inserts anchored inside a span another op
 * replaces.
 *
 * Two ops rewriting one line is always a mistake in the model's plan, and letting
 * assembly order pick a winner would hide it.
 */
export function checkOps(path: string, ops: PatchOp[], count: number): void {
  for (const op of ops) {
    if (op.kind === "ins_head" || op.kind === "ins_tail") continue;
    const a = op.a!;
    const b = op.b!;
    if (a < 1 || a > count) {
      bad(
        count === 0
          ? `${path}: line ${a} is out of range — ${path} is empty. Use ` +
            `INS.HEAD: or INS.TAIL: to put the first lines in.`
          : `${path}: line ${a} is out of range — ${path} has ${count} lines. ` +
            `Line numbers are in the coordinates of the version you viewed.`,
      );
    }
    if (b < a || b > count) {
      bad(
        `${path}: range ${a}.=${b} is invalid for a file of ${count} lines — ` +
          `the range runs from the first line to the last, inclusive.`,
      );
    }
    if (op.kind === "swap" && op.body.length === 0) {
      bad(
        `${path}: SWAP ${a}.=${b} has no body rows. Put the replacement text on ` +
          `"+" rows beneath it, or use DEL ${a}.=${b} to remove those lines.`,
      );
    }
  }

  const spans = ops
    .filter((o) => o.kind === "swap" || o.kind === "del")
    .map((o) => ({ a: o.a!, b: o.b! }))
    .sort((x, y) => x.a - y.a);
  for (let i = 1; i < spans.length; i++) {
    if (spans[i].a <= spans[i - 1].b) {
      bad(
        `${path}: operations overlap — lines ${spans[i - 1].a}.=${spans[i - 1].b} ` +
          `and ${spans[i].a}.=${spans[i].b} both cover line ${spans[i].a}. Cover ` +
          `each line with at most one operation.`,
      );
    }
  }

  // NOTE: not in the port, where an INS anchored inside a SWAP span produced
  // scrambled output. An insert into text the same patch is replacing has no
  // meaning; saying so beats emitting the model's new lines interleaved at random.
  for (const op of ops) {
    if (op.kind !== "ins_pre" && op.kind !== "ins_post") continue;
    const x = op.a!;
    for (const s of spans) {
      const inside = op.kind === "ins_pre" ? x > s.a && x <= s.b : x >= s.a && x < s.b;
      if (inside) {
        const verb = op.kind === "ins_pre" ? "INS.PRE" : "INS.POST";
        bad(
          `${path}: ${verb} ${x} anchors inside lines ${s.a}.=${s.b}, which ` +
            `another operation in this patch replaces. Fold the inserted text ` +
            `into that operation's body instead.`,
        );
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/**
 * Assemble the result in ONE pass over the original lines.
 *
 * This is the mechanical guarantee behind rule 2: every anchor is read against
 * `lines` and nothing is ever spliced into a partially-edited array, so an
 * earlier op cannot shift a later op's coordinates. Assumes `checkOps` passed.
 *
 * Ordering at a single gap is fixed and documented rather than incidental:
 * `INS.HEAD` bodies, then for each line its `INS.PRE` bodies, then the line (or
 * its SWAP body, or nothing for DEL), then its `INS.POST` bodies, and finally
 * `INS.TAIL`. Multiple ops of the same kind at one anchor emit in patch order,
 * and `INS.POST N` precedes `INS.PRE N+1` in the gap they share.
 */
export function materialize(lines: string[], ops: PatchOp[]): string[] {
  const n = lines.length;
  const pre: Array<string[] | undefined> = new Array(n);
  const post: Array<string[] | undefined> = new Array(n);
  const spanAt = new Map<number, PatchOp>();
  const head: string[] = [];
  const tail: string[] = [];

  for (const op of ops) {
    switch (op.kind) {
      case "ins_head":
        head.push(...op.body);
        break;
      case "ins_tail":
        tail.push(...op.body);
        break;
      case "ins_pre":
        (pre[op.a! - 1] ??= []).push(...op.body);
        break;
      case "ins_post":
        (post[op.a! - 1] ??= []).push(...op.body);
        break;
      case "swap":
      case "del":
        spanAt.set(op.a! - 1, op);
        break;
    }
  }

  const out: string[] = [...head];
  let i = 0;
  while (i < n) {
    const p = pre[i];
    if (p) out.push(...p);
    const span = spanAt.get(i);
    if (span) {
      if (span.kind === "swap") out.push(...span.body);
      const last = span.b! - 1;
      const q = post[last];
      if (q) out.push(...q);
      i = last + 1;
    } else {
      out.push(lines[i]);
      const q = post[i];
      if (q) out.push(...q);
      i++;
    }
  }
  out.push(...tail);
  return out;
}

// ---------------------------------------------------------------------------
// Rebase
// ---------------------------------------------------------------------------

/**
 * Past this many lines of divergence the LCS is skipped and every line in the
 * diverged middle is reported as changed. Two agents editing one file almost
 * always diverge in a single small region, so the table normally runs over a
 * handful of lines; the cap keeps a pathological diff from becoming an O(n·m)
 * stall. Exceeding it costs a rejected patch, never a wrong one.
 */
const LCS_CAP = 400;

/**
 * Map each base line index to its index in `cur`, or `null` where the line was
 * changed or deleted by whoever wrote the file in the meantime.
 *
 * Common prefix and suffix are trimmed first — that is what makes this cheap —
 * and an LCS over the diverged middles supplies the rest. The result is
 * monotonically increasing, which is what lets `rebaseOps` conclude that
 * non-overlapping spans stay non-overlapping after rebasing.
 */
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
  | { ok: true; ops: PatchOp[] }
  | { ok: false; conflicts: Array<{ op: PatchOp; reason: string }> };

/**
 * Move ops from the coordinates of `base` (the text the agent read) onto `cur`
 * (the text as it stands now).
 *
 * An op survives when every line it names is still present AND still contiguous.
 * Contiguity is not pedantry: if someone inserted *into* the span, the op's
 * footprint now covers lines the agent never saw, and rewriting them would
 * silently discard that insert. Anything else is a genuine conflict, reported
 * rather than guessed at.
 */
export function rebaseOps(ops: PatchOp[], base: string[], cur: string[]): RebaseResult {
  const map = lineMap(base, cur);
  const out: PatchOp[] = [];
  const conflicts: Array<{ op: PatchOp; reason: string }> = [];
  for (const op of ops) {
    if (op.kind === "ins_head" || op.kind === "ins_tail") {
      out.push(op);
      continue;
    }
    const a = map[op.a! - 1];
    const b = map[op.b! - 1];
    if (a === null || a === undefined || b === null || b === undefined) {
      conflicts.push({ op, reason: `lines ${op.a}.=${op.b} were rewritten` });
      continue;
    }
    if (b - a !== op.b! - op.a!) {
      conflicts.push({ op, reason: `lines ${op.a}.=${op.b} had lines inserted inside them` });
      continue;
    }
    // EVERY line in the span, not just its endpoints. Checking only the ends
    // accepts an op whose interior was rewritten in place — the line count is
    // unchanged, so the contiguity guard above passes, and the op then
    // overwrites an edit the agent never saw. That is the silent lost update
    // this module exists to prevent, and it is the single most common
    // concurrent-edit shape (a renamed identifier, a changed constant) inside
    // a span another agent is replacing. Being stricter here can only turn a
    // silently-wrong apply into a reported conflict.
    let interiorMoved = false;
    for (let k = op.a! - 1; k <= op.b! - 1; k++) {
      if (map[k] !== a + (k - (op.a! - 1))) {
        interiorMoved = true;
        break;
      }
    }
    if (interiorMoved) {
      conflicts.push({ op, reason: `lines ${op.a}.=${op.b} were rewritten` });
      continue;
    }
    out.push({ ...op, a: a + 1, b: b + 1 });
  }
  return conflicts.length ? { ok: false, conflicts } : { ok: true, ops: out };
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

/**
 * Apply a parsed patch to a set of files.
 *
 * `files` maps path → the text as it stands NOW (what a writer would clobber).
 * `opts.base` maps path → the text the agent VIEWED. The return value is a NEW
 * map: every entry of `files` carried over, with patched paths replaced. Neither
 * argument is mutated.
 *
 * All or none: every file is validated, rebased and assembled before any result
 * is handed back, and any failure throws `PatchError` — so a caller that writes
 * only what this returns can never half-apply a patch.
 */
export function applyPatch(
  files: Map<string, string>,
  ops: PatchOp[],
  opts: ApplyOptions = {},
): Map<string, string> {
  const groups = groupByFile(ops);
  const result = new Map(files);

  for (const g of groups) {
    const current = files.get(g.path);
    if (current === undefined) {
      bad(
        `${g.path} is not in this patch's file set — the path in the section ` +
          `header must be the same path you viewed. Check for a typo or a ` +
          `wrong-directory prefix.`,
      );
    }

    const base = resolveBase(g, current, opts);
    const baseLines = toLines(base);

    // Bounds and overlap are judged in the coordinates the ops were WRITTEN in,
    // which is the viewed version — not whatever the file has since become.
    checkOps(g.path, g.ops, baseLines.length);

    const currentLines = toLines(current);
    let effective = g.ops;
    if (normalize(base) !== normalize(current)) {
      const rebased = rebaseOps(g.ops, baseLines, currentLines);
      if (!rebased.ok) bad(conflictMessage(g.path, rebased.conflicts));
      effective = rebased.ops;
    }

    result.set(g.path, joinLines(materialize(currentLines, effective), current));
  }

  return result;
}

/** Decide which text the ops' line numbers are written against. See `ApplyOptions`. */
function resolveBase(g: FileOps, current: string, opts: ApplyOptions): string {
  if (opts.base) {
    const snapshot = opts.base.get(g.path);
    if (snapshot === undefined) {
      bad(
        `no viewed version of ${g.path} is on record — call view("${g.path}") ` +
          `before patching it, then write "[${g.path}#]" with an empty tag to ` +
          `mean the version you just viewed.`,
      );
    }
    if (g.tag && tagOf(snapshot) !== g.tag) bad(staleTagMessage(g.path, g.tag, current));
    return snapshot;
  }
  // No snapshot store in play: the current text is the only version there is.
  if (g.tag && tagOf(current) !== g.tag) bad(staleTagMessage(g.path, g.tag, current));
  return current;
}

function staleTagMessage(path: string, tag: string, current: string): string {
  return (
    `stale tag: "[${path}#${tag}]" names a version of ${path} that is no longer ` +
    `on record — the file has moved on and is now #${tagOf(current)}. Re-view ` +
    `${path} and rewrite the operations against its line numbers, or write an ` +
    `empty tag "[${path}#]" to mean the version you just viewed.`
  );
}

function conflictMessage(
  path: string,
  conflicts: Array<{ op: PatchOp; reason: string }>,
): string {
  return (
    `patch conflict in ${path}: ${conflicts.map((c) => c.reason).join("; ")}. ` +
    `Someone else changed ${path} since the version you viewed. Nothing was ` +
    `written — a patch applies to all its files or none. Re-view ${path} and ` +
    `rewrite the operations against the new line numbers.`
  );
}

/** Throw with a message aimed at the model — say what failed and what to write. */
function bad(message: string): never {
  throw new PatchError(message);
}

/** Keep quoted input short enough not to flood the model's next round. */
function trunc(s: string): string {
  return s.length > 48 ? `${s.slice(0, 48)}…` : s;
}
