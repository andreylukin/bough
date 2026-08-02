/**
 * The shapes the pipeline passes between its stages, and the vocabulary the
 * formatters render.
 *
 * Kept in one module with no imports so that every stage — masking, clustering,
 * accumulation, detection, formatting — agrees on the words without any of them
 * depending on each other. The JSON formatter serializes `Analysis` almost
 * verbatim, which makes this file the de-facto public contract of `--json`:
 * renaming a field here changes an output format somebody may be parsing.
 */

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

/**
 * What a variable slot turned out to hold.
 *
 * Two of these are NOT decided by the masker. `enum` and `id` are properties of a
 * slot's whole value distribution rather than of any one value — `200` looks like
 * an integer until you notice the slot only ever holds three values, and a hex
 * string looks like a number until you notice it never repeats. They are assigned
 * in `stats.ts` once the counts exist, which is why this type is shared rather
 * than owned by the masker.
 */
export type VarKind =
  | "ipv4"
  | "ipv6"
  | "uuid"
  | "url"
  | "path"
  | "duration"
  | "bytes"
  | "hex"
  | "float"
  | "int"
  | "quoted"
  | "timestamp"
  | "enum"
  | "id"
  | "string";

/** One variable occurrence pulled out of one line. */
export interface VarValue {
  kind: VarKind;
  /** The text exactly as it appeared, punctuation and unit suffix included. */
  raw: string;
  /**
   * The comparable magnitude, for kinds that have one.
   *
   * Normalized to a base unit so a slot holding `1.5s` and `900ms` produces
   * quantiles anyone can read: durations become milliseconds, sizes become bytes.
   * A p99 computed over the bare numerals would rank 900 above 1.5 and report the
   * fast case as the slow one.
   */
  num?: number;
  /**
   * Where the placeholder sits in the logtype, as a character offset.
   *
   * Needed because statistics are keyed on TEMPLATE POSITION, and positions are
   * tokens. Without an offset the only way to attribute a value to a token is to
   * re-split and re-match, which goes wrong on exactly the values that matter —
   * a `<quoted>` holding spaces is one token but looks like several.
   */
  at: number;
}

/** What one variable slot of one pattern turned out to be, over every line. */
export interface VarSummary {
  /** Position among the pattern's placeholders, left to right, from zero. */
  slot: number;
  kind: VarKind;
  /** How many lines supplied a value here. Below the pattern count when a wildcard swallowed some. */
  count: number;
  /** Estimated distinct values (HyperLogLog, ~1.6% error). */
  unique: number;
  /**
   * The most frequent values with their shares, or `null` when the slot has too
   * many distinct values for a ranking to mean anything.
   *
   * Null rather than a truncated list on purpose: past the tracking cap the counts
   * are biased toward whatever appeared first, and printing three arbitrary request
   * IDs as "the top values" invites a reader to conclude something false.
   */
  top: { value: string; count: number; share: number }[] | null;
  /** Quantiles, for the kinds that carry a magnitude. */
  numeric?: {
    min: number;
    max: number;
    mean: number;
    p50: number;
    p90: number;
    p99: number;
    /** The base unit `num` was normalized to — `ms`, `bytes`, or absent. */
    unit?: string;
  };
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

/** The severity read off a line's own words, ordered so comparisons work. */
export const SEVERITIES = ["debug", "info", "warn", "error", "fatal"] as const;
export type Severity = (typeof SEVERITIES)[number];

/** Something about a pattern that a reader would want pointed at rather than left to find. */
export interface Anomaly {
  kind:
    | "frequency-spike"
    | "error-burst"
    | "single-value"
    | "bimodal"
    | "rare"
    | "high-cardinality"
    | "long-tail";
  /** One line, already phrased for a human. Formatters print this verbatim. */
  detail: string;
}

/** One cluster of structurally identical lines. */
export interface Pattern {
  id: number;
  /** The line with its variables replaced by `<kind>` and its divergences by `<*>`. */
  template: string;
  count: number;
  /** Share of all analyzed lines, 0..1. */
  share: number;
  severity: Severity;
  /** Epoch ms of the first and last line carrying a parsable timestamp. */
  firstSeen?: number;
  lastSeen?: number;
  vars: VarSummary[];
  /** A uniform sample of real lines, capped and reproducible. */
  examples: string[];
  /**
   * Occupancy across the analysis's shared time buckets.
   *
   * Shared, not per-pattern: the spike test compares a pattern against its own
   * history and the correlation test compares patterns against each other, and
   * neither is meaningful if two patterns' bucket `3` cover different minutes.
   */
  buckets: number[];
  anomalies: Anomaly[];
}

/** Two patterns that appear to be related, and why. */
export interface Correlation {
  a: number;
  b: number;
  kind: "temporal" | "shared-value";
  /** 0..1. Cosine similarity of bucket vectors, or overlap of a shared variable's values. */
  strength: number;
  detail: string;
}

// ---------------------------------------------------------------------------
// The analysis
// ---------------------------------------------------------------------------

/** Everything one run learned. This is what `--json` prints. */
export interface Analysis {
  /** Lines read, including ones no pattern kept (blank lines are skipped, not counted). */
  lines: number;
  /** Patterns after clustering, before any `--top` truncation. */
  patternCount: number;
  /** Patterns to render, already sorted by interestingness. */
  patterns: Pattern[];
  correlations: Correlation[];
  /** Epoch ms bounds over every line with a parsable timestamp. */
  timeSpan?: { from: number; to: number };
  /** Milliseconds each shared bucket covers, so a bucket index can be dated. */
  bucketMs?: number;
  /**
   * True when clustering hit its cluster cap and started evicting.
   *
   * Surfaced rather than swallowed: past the cap the counts undercount whatever was
   * evicted, and a reader comparing two runs deserves to know the second one was
   * measuring something different.
   */
  truncated: boolean;
}
