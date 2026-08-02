/**
 * The pipeline: lines in, an `Analysis` out.
 *
 * Every stage lives in its own module and this one only sequences them, which is
 * deliberate — the interesting decisions are all local (which kinds mask first,
 * what makes a slot an enum, when a spike is a spike) and none of them belong in an
 * orchestrator. What is genuinely this module's is the ORDER, the streaming
 * discipline, and the final ranking.
 *
 *   strip timestamp → mask values → tokenize → cluster → attribute → accumulate
 *                                                                        ↓
 *                        rank ← correlate ← detect anomalies ← summarize
 *
 * ONE PASS OVER THE INPUT, and nothing that scales with line count is retained. A
 * line is folded into its cluster's accumulators and dropped; what survives is a
 * fixed cost per pattern. That is the property that makes this usable on a file
 * nobody could open, and it is easy to lose by accident — anything added here that
 * pushes to an array per line silently converts the tool back into one that needs
 * the whole file in memory.
 *
 * RANKING IS THE LAST DECISION AND THE MOST IMPORTANT ONE. The output is truncated
 * to `top`, so ranking decides what a reader sees at all. Sorting by count alone —
 * the obvious choice — is actively wrong for this purpose: it puts a million INFO
 * request lines above three FATAL ones, and the three are the reason anyone opened
 * the file. Severity leads, volume follows within a severity, and anomalies break
 * ties upward. See `score` below.
 */
import { detect } from "./anomaly.ts";
import { correlate } from "./correlation.ts";
import { Drain, type DrainOptions } from "./drain.ts";
import { mask } from "./mask.ts";
import { attribute, PatternAcc, TimeAxis, tokenize } from "./stats.ts";
import { stripTimestamp } from "./timestamp.ts";
import type { Analysis, Pattern, Severity } from "./types.ts";

export interface AnalyzeOptions {
  /** Patterns to render. The rest are counted and dropped. */
  top?: number;
  /** Year for timestamp formats that omit one (syslog). */
  refYear?: number;
  /** Passed through to the clustering tree. */
  drain?: DrainOptions;
}

/** Severity ordering for the rank. */
const SEV_RANK: Record<Severity, number> = { debug: 0, info: 1, warn: 2, error: 3, fatal: 4 };

/**
 * The pipeline as something you push lines into.
 *
 * EXPOSED AS A CLASS SO NOTHING EVER HOLDS THE INPUT. The convenient shape is a
 * function over an array, and it is a trap: a caller with an async source has no
 * choice but to collect it first, and collecting is precisely the thing the
 * bounded-memory design exists to avoid. Reading a 48MB log into an array of
 * strings costs about 700MB of heap and makes every sketch in `stats.ts`
 * pointless — the tool would then be capped by the same limit as `cat`, while
 * claiming in its own comments not to be.
 *
 * `analyze()` below is the array-shaped convenience wrapper. It is for tests and
 * for callers who already hold the lines; the CLI uses this class directly.
 */
export class Analyzer {
  private readonly axis = new TimeAxis();
  private readonly accs = new Map<number, PatternAcc>();
  private readonly drain: Drain;
  private readonly top: number;
  private readonly refYear: number | undefined;
  private total = 0;
  private spanFrom?: number;
  private spanTo?: number;

  constructor(opts: AnalyzeOptions = {}) {
    this.top = opts.top ?? 20;
    this.refYear = opts.refYear;
    // A cluster the cap evicts must take its statistics with it, or this map becomes
    // the unbounded thing the cap exists to prevent.
    this.drain = new Drain({ ...opts.drain, onEvict: (c) => this.accs.delete(c.id) });
  }

  /** Fold one raw line in. Nothing about it is retained beyond its statistics. */
  push(raw: string): void {
    // Blank lines carry no structure and would form one enormous empty cluster.
    if (raw.trim().length === 0) return;
    this.total++;

    const stamped = stripTimestamp(raw, this.refYear);
    if (stamped.when !== undefined) {
      if (this.spanFrom === undefined || stamped.when < this.spanFrom) this.spanFrom = stamped.when;
      if (this.spanTo === undefined || stamped.when > this.spanTo) this.spanTo = stamped.when;
    }

    const masked = mask(stamped.rest);
    const toks = tokenize(masked.logtype);
    const cluster = this.drain.add(toks.map((t) => t.text));

    let acc = this.accs.get(cluster.id);
    if (!acc) {
      acc = new PatternAcc();
      this.accs.set(cluster.id, acc);
    }
    // Attribution uses the template as it stands NOW. A token that generalizes later
    // means earlier lines were attributed under the more specific reading — their
    // values are still in the right slot, which is what matters, because slots are
    // keyed on position and the position did not move.
    acc.add(raw, stamped.when, attribute(toks, masked.values, cluster.tokens), this.axis);
  }

  /** Materialize, rank, detect and correlate. Safe to call once. */
  finish(): Analysis {
    const all: Pattern[] = [];
    for (const cluster of this.drain.clusters()) {
      const acc = this.accs.get(cluster.id);
      if (!acc) continue;
      const pattern: Pattern = {
        id: cluster.id,
        template: cluster.tokens.join(" "),
        count: acc.count,
        share: this.total === 0 ? 0 : acc.count / this.total,
        severity: acc.severity,
        ...(acc.first !== undefined ? { firstSeen: acc.first } : {}),
        ...(acc.last !== undefined ? { lastSeen: acc.last } : {}),
        vars: acc.summarize(),
        examples: acc.examples.sample(),
        buckets: acc.bucketArray(this.axis),
        anomalies: [],
      };
      pattern.anomalies = detect(pattern, this.total);
      all.push(pattern);
    }

    all.sort((a, b) => score(b) - score(a) || b.count - a.count);
    // IDs are reassigned to render order so the correlation lines ("#1 and #3") name
    // things the reader can actually find. Cluster ids are allocation order, which is
    // an implementation detail nobody should be shown.
    const renumbered = all.slice(0, this.top).map((p, i) => ({ ...p, id: i + 1 }));

    return {
      lines: this.total,
      patternCount: all.length,
      patterns: renumbered,
      correlations: correlate(renumbered),
      ...(this.spanFrom !== undefined && this.spanTo !== undefined
        ? { timeSpan: { from: this.spanFrom, to: this.spanTo }, bucketMs: this.axis.bucketMs }
        : {}),
      truncated: this.drain.truncated,
    };
  }
}

/** Analyze lines already in hand. For an async or very large source, use `Analyzer`. */
export function analyze(lines: Iterable<string>, opts: AnalyzeOptions = {}): Analysis {
  const a = new Analyzer(opts);
  for (const line of lines) a.push(line);
  return a.finish();
}

/**
 * How interesting a pattern is, as one number.
 *
 * Severity dominates by construction — the gap between severity tiers is larger
 * than the entire range volume can contribute — because "show me the errors" is
 * what running this means nine times out of ten, and no amount of INFO volume
 * should outrank a FATAL. Within a tier, volume orders things, on a log scale so
 * that the difference between 10 and 100 lines counts for as much as the one
 * between 100 and 1,000. An anomaly is worth a fixed nudge: enough to lift a
 * flagged pattern above an unflagged one of similar size, not enough to jump a
 * severity tier it does not belong in.
 */
function score(p: Pattern): number {
  const severity = SEV_RANK[p.severity] * 100;
  const volume = Math.log10(p.count + 1) * 5;
  const flagged = p.anomalies.length > 0 ? 10 : 0;
  return severity + volume + flagged;
}
