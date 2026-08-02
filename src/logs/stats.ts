/**
 * Stage three: turn a stream of clustered lines into the numbers the output shows.
 *
 * WHAT THIS OWNS. Everything that requires seeing many lines of one pattern —
 * severity, the time span, the temporal shape, and for each variable position its
 * distribution. It never holds a line longer than it takes to fold it in, which is
 * what makes the memory claim true: a pattern costs a fixed number of sketches
 * regardless of whether it matched a thousand lines or a billion.
 *
 * STATISTICS ARE KEYED ON TEMPLATE POSITION, NOT ON TOKEN TEXT. A template mutates
 * as it generalizes — the word at position 3 can become `<*>` after ten thousand
 * lines have already been counted — so anything keyed on what a token said at
 * insertion time would scatter one slot's values across several. Position is the
 * only stable identity a slot has.
 *
 * TWO KINDS ARE DECIDED HERE RATHER THAN BY THE MASKER, because they are properties
 * of a distribution and not of any single value:
 *
 *   - `enum`  — the slot holds few distinct values, repeatedly. `200` is an integer
 *               when you look at one line and a status code when you look at ten
 *               thousand, and only the second reading is worth printing.
 *   - `id`    — the slot holds a different value nearly every time. A request ID is
 *               shaped exactly like an integer or a hex string, and the thing worth
 *               saying about it is that it never repeats — a p99 over request IDs is
 *               a number with no meaning, and printing one invites a reader to
 *               believe it has one.
 *
 * THE TIME AXIS IS SHARED AND ONLY EVER COARSENS. Every pattern buckets against one
 * origin and one width, so bucket 3 covers the same minutes for all of them —
 * without that, comparing two patterns' shapes (which the correlation pass does)
 * would be comparing different clocks. Since the full time span is not known until
 * the last line, the width starts fine and doubles whenever the span outgrows the
 * bucket budget, with existing counts folded pairwise. Coarsening is lossless in
 * the direction that matters: two adjacent buckets summed is exactly the count of
 * the wider bucket they tile.
 *
 * Pure apart from the sketches' own state. No clock, no filesystem.
 */
import { DDSketch, HyperLogLog, Reservoir, TopK } from "./sketch.ts";
import { WILDCARD } from "./drain.ts";
import type { Severity, VarKind, VarSummary, VarValue } from "./types.ts";

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/**
 * The severity words, most severe first so the first hit wins.
 *
 * Matched case-sensitively against upper-case forms and against the capitalized
 * form, but never against lower-case prose: a line saying `failed to warn the
 * operator` is not a warning, and a case-insensitive scan classifies it as one.
 * Log levels are conventionally shouted, and relying on that is far more accurate
 * than trying to read intent from lower-case English.
 */
const SEVERITY_WORDS: [Severity, string[]][] = [
  ["fatal", ["FATAL", "PANIC", "CRITICAL", "CRIT", "EMERG", "ALERT"]],
  ["error", ["ERROR", "ERR", "SEVERE", "EXCEPTION", "FAIL", "FAILED", "FAILURE"]],
  ["warn", ["WARN", "WARNING"]],
  ["debug", ["DEBUG", "TRACE", "VERBOSE", "FINE"]],
  ["info", ["INFO", "NOTICE"]],
];

/** Rank for comparisons; higher is worse. */
const RANK: Record<Severity, number> = { debug: 0, info: 1, warn: 2, error: 3, fatal: 4 };

/**
 * Read a severity off a line's own words, defaulting to `info`.
 *
 * Only whole tokens count. Substring matching would classify `ERRORS_TOTAL=0` — a
 * metric name, quite possibly reporting zero errors — as an error line, and
 * metric names of exactly that shape are everywhere.
 */
export function severityOf(line: string): Severity {
  // Split on anything that is not a letter or underscore so `[ERROR]`, `level=WARN`
  // and `WARN:` all yield the bare word.
  const words = line.split(/[^A-Za-z_]+/);
  for (const [sev, forms] of SEVERITY_WORDS) {
    for (const w of words) {
      if (w.length === 0) continue;
      const upper = w.toUpperCase();
      // The word must have been shouted or capitalized in the original.
      if (w !== upper && w[0] !== (w[0] as string).toUpperCase()) continue;
      if (forms.includes(upper)) return sev;
    }
  }
  return "info";
}

// ---------------------------------------------------------------------------
// The shared time axis
// ---------------------------------------------------------------------------

/** Buckets held before the axis coarsens. Also the widest bar any formatter draws. */
const MAX_BUCKETS = 512;

/**
 * One origin and one bucket width, shared by every pattern.
 *
 * Indices may be negative: logs are not reliably ordered, and a line older than the
 * first one seen must extend the axis backwards rather than be dropped or clamped
 * into bucket zero — clamping would invent a spike at the start of every
 * out-of-order file.
 */
export class TimeAxis {
  origin?: number;
  bucketMs = 1000;
  /** Doublings so far. A pattern lagging by `n` generations rescales by `>> n`. */
  generation = 0;
  private lo = 0;
  private hi = 0;

  /** The bucket for a moment, coarsening the axis first if the span demands it. */
  index(when: number): number {
    if (this.origin === undefined) this.origin = when;
    let idx = Math.floor((when - this.origin) / this.bucketMs);
    // Coarsen until the span fits the budget. A loop rather than an `if` because a
    // single line can arrive far outside the current range — the second line of a
    // file may be a year after the first.
    while (Math.max(this.hi, idx) - Math.min(this.lo, idx) >= MAX_BUCKETS) {
      this.bucketMs *= 2;
      this.generation++;
      // Arithmetic shift, not division: `>> 1` floors toward negative infinity,
      // which is the same direction `Math.floor` took, so a negative index keeps
      // landing in the bucket that actually contains it.
      this.lo >>= 1;
      this.hi >>= 1;
      idx = Math.floor((when - this.origin) / this.bucketMs);
    }
    if (idx < this.lo) this.lo = idx;
    if (idx > this.hi) this.hi = idx;
    return idx;
  }

  get range(): { lo: number; hi: number } {
    return { lo: this.lo, hi: this.hi };
  }
}

// ---------------------------------------------------------------------------
// Per-slot accumulation
// ---------------------------------------------------------------------------

/** Examples kept per pattern. Enough to see variety, few enough to stay readable. */
const EXAMPLES = 3;

/** Everything learned about one variable position of one pattern. */
class SlotAcc {
  readonly top = new TopK(1024);
  readonly unique = new HyperLogLog();
  /** Built lazily: most slots never hold a number, and a sketch per slot is not free. */
  private numeric?: DDSketch;
  count = 0;
  /** Kinds seen here, so a slot that holds two is described as the general one. */
  readonly kinds = new Map<VarKind, number>();
  unit?: string;

  add(v: VarValue): void {
    this.count++;
    this.top.add(v.raw);
    this.unique.add(v.raw);
    this.kinds.set(v.kind, (this.kinds.get(v.kind) ?? 0) + 1);
    if (v.num !== undefined) {
      this.numeric ??= new DDSketch();
      this.numeric.add(v.num);
      if (v.kind === "duration") this.unit = "ms";
      else if (v.kind === "bytes") this.unit = "bytes";
    }
  }

  get sketch(): DDSketch | undefined {
    return this.numeric;
  }
}

/**
 * Everything learned about one cluster.
 *
 * Slots are a Map keyed by `"<tokenIndex>.<ordinal>"` rather than an array, because
 * one token can carry several values — `10.0.1.15:5432` is one token and two
 * variables — and because a position may go unfilled on lines where a wildcard
 * swallowed a differently-shaped token.
 */
export class PatternAcc {
  severity: Severity = "debug";
  first?: number;
  last?: number;
  readonly examples = new Reservoir<string>(EXAMPLES, 0x5bf03635);
  readonly buckets = new Map<number, number>();
  private bucketGen = 0;
  readonly slots = new Map<string, SlotAcc>();
  count = 0;

  /**
   * Fold one line in.
   *
   * `tokenValues` is aligned to the TEMPLATE's tokens, not the line's, and holds
   * either the values masked out of that token or — where the template has
   * generalized — the token's own reconstructed text as a single value.
   */
  add(raw: string, when: number | undefined, tokenValues: VarValue[][], axis: TimeAxis): void {
    this.count++;
    // Severity is the worst seen, not the first or the last. A statement whose level
    // word generalized to a wildcard still emitted real errors, and describing that
    // pattern as `info` because most of its lines were would bury them.
    const sev = severityOf(raw);
    if (RANK[sev] > RANK[this.severity]) this.severity = sev;
    this.examples.add(raw);

    if (when !== undefined) {
      if (this.first === undefined || when < this.first) this.first = when;
      if (this.last === undefined || when > this.last) this.last = when;
      const idx = axis.index(when);
      this.rescale(axis.generation);
      this.buckets.set(idx, (this.buckets.get(idx) ?? 0) + 1);
    }

    for (let t = 0; t < tokenValues.length; t++) {
      const vals = tokenValues[t] as VarValue[];
      for (let o = 0; o < vals.length; o++) {
        const key = `${t}.${o}`;
        let slot = this.slots.get(key);
        if (!slot) {
          slot = new SlotAcc();
          this.slots.set(key, slot);
        }
        slot.add(vals[o] as VarValue);
      }
    }
  }

  /**
   * Fold this pattern's buckets down to the axis's current width.
   *
   * Done lazily on the next line rather than eagerly across every pattern when the
   * axis doubles: coarsening touches every pattern's map, and doing it eagerly makes
   * one unlucky line pay for all of them. A pattern that never receives another line
   * is folded once at the end instead.
   */
  rescale(generation: number): void {
    while (this.bucketGen < generation) {
      const folded = new Map<number, number>();
      for (const [idx, n] of this.buckets) {
        const half = idx >> 1;
        folded.set(half, (folded.get(half) ?? 0) + n);
      }
      this.buckets.clear();
      for (const [idx, n] of folded) this.buckets.set(idx, n);
      this.bucketGen++;
    }
  }

  /** Buckets as a dense array over the axis's range, for the formatters and detectors. */
  bucketArray(axis: TimeAxis): number[] {
    this.rescale(axis.generation);
    const { lo, hi } = axis.range;
    if (this.buckets.size === 0) return [];
    const out = new Array<number>(hi - lo + 1).fill(0);
    for (const [idx, n] of this.buckets) {
      const at = idx - lo;
      if (at >= 0 && at < out.length) out[at] = (out[at] as number) + n;
    }
    return out;
  }

  /** Every slot, described. Ordered by token position so output reads left to right. */
  summarize(): VarSummary[] {
    return [...this.slots.entries()]
      .sort((a, b) => {
        const [at, ao] = a[0].split(".").map(Number) as [number, number];
        const [bt, bo] = b[0].split(".").map(Number) as [number, number];
        return at - bt || ao - bo;
      })
      .map(([, slot], i) => describeSlot(slot, i));
  }
}

/**
 * Decide what a slot is and render its distribution.
 *
 * The ordering of the tests matters. `id` is checked before `enum` because a slot
 * with two lines and two distinct values satisfies both readings, and calling it an
 * enum of two would be a claim about a distribution that has not been observed yet.
 */
function describeSlot(slot: SlotAcc, index: number): VarSummary {
  // Clamped to the number of values actually seen. HyperLogLog is unbiased, which
  // means it overshoots about half the time, and `unique=1,936 of 1,925 lines` is
  // arithmetically impossible on its face — a reader who spots it stops trusting
  // every other number on the page. The clamp costs nothing: the estimate is only
  // ever consulted when it is well below the count.
  const unique = Math.min(slot.unique.count(), slot.count);
  // The masker's most frequent verdict, as the starting point.
  let kind: VarKind = "string";
  let best = -1;
  for (const [k, n] of slot.kinds) {
    if (n > best) {
      kind = k;
      best = n;
    }
  }

  const ratio = slot.count === 0 ? 0 : unique / slot.count;
  const topThree = slot.top.top(3);
  const topShare =
    slot.count === 0 ? 0 : topThree.reduce((s, e) => s + e.count, 0) / slot.count;

  // An identifier: nearly every line brought a new value. The threshold is high and
  // the sample requirement real — three distinct values out of three lines is not
  // evidence of anything.
  const looksLikeId = slot.count >= 10 && ratio > 0.9;
  // An enumeration: few values, seen repeatedly, dominating the slot. All three
  // conditions are needed — "few distinct values" alone is also true of a slot that
  // has only been filled twice.
  const looksLikeEnum = slot.count >= 10 && unique <= 20 && topShare >= 0.8;

  if (looksLikeId && (kind === "int" || kind === "hex" || kind === "string" || kind === "uuid")) {
    kind = "id";
  } else if (looksLikeEnum && (kind === "int" || kind === "string" || kind === "float")) {
    kind = "enum";
  }

  // Suppress the ranking when it cannot be trusted: past the tracking cap the counts
  // favour whatever arrived first, and for an identifier the "top values" are three
  // arbitrary IDs that a reader would reasonably mistake for hot spots.
  const rankable = !slot.top.saturated && kind !== "id";
  const top = rankable
    ? topThree.map((e) => ({ value: e.value, count: e.count, share: e.count / slot.count }))
    : null;

  const summary: VarSummary = { slot: index, kind, count: slot.count, unique, top };
  // Quantiles are reported only where they mean something, and there are three
  // ways for them not to:
  //
  //   - an IDENTIFIER that happens to be numeric has a p99, and it is noise
  //     dressed as a statistic;
  //   - an ENUM is categorical. A status code's median is 200 and that is not a
  //     fact about latency, it is an artefact of 200 sorting below 404. The top
  //     values already say everything true about the slot;
  //   - a CONSTANT has no distribution at all, and printing `p50=5378.9` for a
  //     port that is always 5432 advertises the sketch's error on a number that
  //     was never approximate.
  const meaningful = kind !== "id" && kind !== "enum" && unique > 1;
  const q = meaningful ? slot.sketch?.summary() : null;
  if (q) {
    summary.numeric = {
      min: q.min,
      max: q.max,
      mean: q.mean,
      p50: q.p50,
      p90: q.p90,
      p99: q.p99,
      ...(slot.unit ? { unit: slot.unit } : {}),
    };
  }
  return summary;
}

// ---------------------------------------------------------------------------
// Attributing a line's values to template positions
// ---------------------------------------------------------------------------

/** A whitespace-delimited token of a logtype, with where it started. */
export interface Tok {
  text: string;
  start: number;
}

/** Split a logtype into tokens, keeping each one's offset so values can be attributed. */
export function tokenize(logtype: string): Tok[] {
  const out: Tok[] = [];
  const re = /\S+/g;
  for (let m = re.exec(logtype); m !== null; m = re.exec(logtype)) {
    out.push({ text: m[0], start: m.index });
  }
  return out;
}

/**
 * Line up one line's masked values with a template's token positions.
 *
 * Two cases per position:
 *
 *   - The template kept the token. Whatever the masker pulled out of it belongs to
 *     that slot, in order — a token like `<ipv4>:<int>` contributes two.
 *   - The template generalized the token to `<*>`. Then the interesting value is the
 *     token itself, and it is reconstructed with its placeholders filled back in, so
 *     the slot reports `db-primary` and `10.0.1.15` rather than the useless literal
 *     string `<ipv4>`. Its kind is the masker's if the token was entirely one value,
 *     and `string` otherwise, since a mixed token has no single type.
 */
export function attribute(tokens: Tok[], values: VarValue[], template: string[]): VarValue[][] {
  const perToken: VarValue[][] = tokens.map(() => []);
  for (const v of values) {
    // The last token starting at or before the value's offset is the one containing
    // it. Tokens are ordered, so a backwards scan finds it immediately in practice.
    let t = tokens.length - 1;
    while (t > 0 && (tokens[t] as Tok).start > v.at) t--;
    (perToken[t] as VarValue[]).push(v);
  }

  const out: VarValue[][] = [];
  for (let i = 0; i < tokens.length; i++) {
    const tok = tokens[i] as Tok;
    const vals = perToken[i] as VarValue[];
    if (template[i] !== WILDCARD) {
      out.push(vals);
      continue;
    }
    // Reconstruct: walk the token, substituting each placeholder's raw text back in.
    let text = "";
    let cursor = tok.start;
    for (const v of vals) {
      text += tok.text.slice(cursor - tok.start, v.at - tok.start) + v.raw;
      cursor = v.at + `<${v.kind}>`.length;
    }
    text += tok.text.slice(cursor - tok.start);
    const soleKind = vals.length === 1 && vals[0]?.raw === text ? (vals[0] as VarValue).kind : "string";
    const num = vals.length === 1 ? vals[0]?.num : undefined;
    out.push([num === undefined ? { kind: soleKind, raw: text, at: tok.start } : { kind: soleKind, raw: text, num, at: tok.start }]);
  }
  return out;
}
