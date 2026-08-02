/**
 * Stage five: which patterns move together.
 *
 * WHAT THIS IS FOR. The single most common question asked of a log is not "what
 * happened" but "what happened AT THE SAME TIME as this". A pattern-by-pattern
 * report answers the first and actively obstructs the second, because it presents
 * each statement in isolation and the reader has to reconstruct simultaneity by
 * eye from timestamps that have already been summarized away. Two patterns whose
 * counts rise and fall together are the closest this pipeline gets to handing over
 * a causal lead.
 *
 * IT IS A LEAD, NOT A CAUSE, and the wording of every result reflects that. Two
 * patterns can co-occur because one causes the other, because a third thing causes
 * both, or because they are simply the two busiest statements in a service that
 * gets busy in bursts. Nothing here can tell those apart, so nothing here says
 * "caused" — the detail line reports the observation and leaves the inference to
 * the reader, which is the same discipline `anomaly.ts` holds.
 *
 * TWO INDEPENDENT SIGNALS, because they fail in different directions:
 *
 *   - TEMPORAL — the two patterns' bucket vectors point the same way. Catches
 *     relationships between statements that share no data at all, which is most of
 *     them: a connection error and a retry warning name nothing in common.
 *   - SHARED VALUE — the two patterns have a slot holding the same small set of
 *     values. Catches relationships that are invisible in time because one pattern
 *     is rare: three errors about `10.0.1.15` among a million lines never move any
 *     bucket, but they are the whole story.
 *
 * COSINE, NOT CORRELATION COEFFICIENT. Pearson's r subtracts the mean, which for
 * bucket counts means a pattern that is quiet in the same buckets where another is
 * quiet scores as strongly as one that is BUSY where the other is busy. On sparse
 * log data most buckets are empty, so mean-centering makes co-absence the dominant
 * signal and pairs of unrelated rare patterns score near 1. Cosine treats a zero as
 * "nothing happened" rather than "below average", which is what a zero means here.
 *
 * Pure: takes patterns, returns pairs.
 */
import type { Correlation, Pattern } from "./types.ts";

/** Cosine similarity below which a temporal pair is not worth mentioning. */
const TEMPORAL_MIN = 0.8;

/** Overlap below which a shared-value pair is not worth mentioning. */
const SHARED_MIN = 0.5;

/** Lines each side needs before its shape is trusted. */
const MIN_COUNT = 10;

/** Active buckets each side needs, so a pair of one-bucket patterns cannot score 1.0. */
const MIN_ACTIVE = 3;

/** Pairs reported. Beyond a handful this stops being a lead and becomes another table. */
const MAX_RESULTS = 8;

/**
 * Find related pairs among the patterns given.
 *
 * Quadratic in the pattern count, which is fine and bounded by design: this runs
 * over the patterns being RENDERED — a few dozen after `--top` — not over the
 * thousands that may exist. Handing it the full set would be the one place in the
 * pipeline with unbounded cost.
 */
export function correlate(patterns: Pattern[]): Correlation[] {
  const out: Correlation[] = [];
  for (let i = 0; i < patterns.length; i++) {
    for (let j = i + 1; j < patterns.length; j++) {
      const a = patterns[i] as Pattern;
      const b = patterns[j] as Pattern;
      const temporal = temporalPair(a, b);
      if (temporal) out.push(temporal);
      const shared = sharedValuePair(a, b);
      if (shared) out.push(shared);
    }
  }
  return out.sort((x, y) => y.strength - x.strength).slice(0, MAX_RESULTS);
}

/** Do these two rise and fall together? */
function temporalPair(a: Pattern, b: Pattern): Correlation | undefined {
  if (a.count < MIN_COUNT || b.count < MIN_COUNT) return undefined;
  const n = Math.min(a.buckets.length, b.buckets.length);
  if (n === 0) return undefined;
  // Both sides must be spread over time. Two patterns that each occupy a single
  // bucket score a perfect 1.0 while sharing nothing but a minute, and on a short
  // log that describes most pairs.
  if (activeCount(a.buckets) < MIN_ACTIVE || activeCount(b.buckets) < MIN_ACTIVE) return undefined;

  let dot = 0;
  let na = 0;
  let nb = 0;
  for (let i = 0; i < n; i++) {
    const x = a.buckets[i] as number;
    const y = b.buckets[i] as number;
    dot += x * y;
    na += x * x;
    nb += y * y;
  }
  if (na === 0 || nb === 0) return undefined;
  const strength = dot / (Math.sqrt(na) * Math.sqrt(nb));
  if (strength < TEMPORAL_MIN) return undefined;
  return {
    a: a.id,
    b: b.id,
    kind: "temporal",
    strength,
    detail: `#${a.id} and #${b.id} rise and fall together (${(strength * 100).toFixed(0)}% aligned over time)`,
  };
}

function activeCount(buckets: number[]): number {
  let n = 0;
  for (const b of buckets) if (b > 0) n++;
  return n;
}

/**
 * Do these two talk about the same things?
 *
 * Only slots with a usable ranking participate — `top` is null for identifiers and
 * for saturated slots, and both are exactly the cases where an overlap would be
 * meaningless. Two patterns "sharing" request IDs is what request IDs are for, and
 * finding that out is not a lead.
 */
function sharedValuePair(a: Pattern, b: Pattern): Correlation | undefined {
  if (a.count < MIN_COUNT || b.count < MIN_COUNT) return undefined;
  let best: { overlap: number; value: string; sa: number; sb: number } | undefined;

  for (const va of a.vars) {
    if (!va.top || va.top.length === 0 || va.unique > 50) continue;
    for (const vb of b.vars) {
      if (!vb.top || vb.top.length === 0 || vb.unique > 50) continue;
      // Kinds must agree, or a status code and a retry count "share" the value 3.
      if (va.kind !== vb.kind) continue;
      // Bare integers are excluded outright: small integers collide constantly
      // across unrelated slots and would dominate every result.
      if (va.kind === "int" || va.kind === "float") continue;

      const setB = new Map(vb.top.map((e) => [e.value, e.share]));
      for (const ea of va.top) {
        const shareB = setB.get(ea.value);
        if (shareB === undefined) continue;
        // Strength is the weaker of the two shares: a value that is 90% of one slot
        // and 2% of the other is not a shared story, it is a coincidence in the
        // busier pattern.
        const overlap = Math.min(ea.share, shareB);
        if (!best || overlap > best.overlap) {
          best = { overlap, value: ea.value, sa: va.slot, sb: vb.slot };
        }
      }
    }
  }

  if (!best || best.overlap < SHARED_MIN) return undefined;
  return {
    a: a.id,
    b: b.id,
    kind: "shared-value",
    strength: best.overlap,
    detail: `#${a.id} slot ${best.sa} and #${b.id} slot ${best.sb} both centre on ${best.value} (${(best.overlap * 100).toFixed(0)}% of each)`,
  };
}
