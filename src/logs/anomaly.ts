/**
 * Stage four: point at the handful of things in the output a reader should look at
 * first.
 *
 * WHY DETECTION AT ALL, WHEN THE STATISTICS ARE RIGHT THERE. Compression is the
 * feature, but forty-three patterns with six variables each is still two hundred
 * numbers, and the reason anyone ran this was to find the one that is wrong. These
 * detectors are the difference between a report that CONTAINS the answer and one
 * that SHOWS it.
 *
 * THE BAR FOR FIRING IS DELIBERATELY HIGH, and the reason is that this output is
 * mostly read by a language model, which will dutifully investigate whatever it is
 * told is anomalous. A false positive does not merely add a line — it sends the
 * reader down a hole. So every detector here requires a minimum sample before it
 * will speak, and the thresholds are set where a human would agree on sight rather
 * than where they maximize recall. A missed anomaly costs a reader one scan of a
 * table they already have. A phantom one costs them the investigation.
 *
 * NOTHING HERE IS STATISTICAL INFERENCE. There is no distributional assumption, no
 * p-value, no fitted model — a log's inter-arrival times are not Poisson, its
 * durations are not normal, and a test built on either would be confidently wrong
 * in a way that is very hard to notice. Each detector is a plain, explicable rule,
 * and each one produces a sentence that says what it saw rather than what it
 * concluded.
 *
 * Pure: takes patterns, returns annotations.
 */
import type { Anomaly, Pattern } from "./types.ts";

/** Lines a pattern needs before any detector will describe its shape. */
const MIN_SAMPLE = 20;

/** How far above its own median a bucket must be to count as a spike. */
const SPIKE_FACTOR = 5;

/** Share of all lines below which a pattern is called rare. */
const RARE_SHARE = 0.001;

/**
 * Everything worth saying about one pattern.
 *
 * Ordered by how much they should influence what a reader does next: a burst of
 * errors outranks a merely odd distribution.
 */
export function detect(p: Pattern, totalLines: number): Anomaly[] {
  const found: Anomaly[] = [];

  // --- Temporal shape ------------------------------------------------------
  //
  // Compared against the pattern's OWN median bucket rather than against a global
  // rate, because patterns differ in frequency by orders of magnitude and any
  // shared threshold would either fire on every rare pattern or on none.
  const active = p.buckets.filter((n) => n > 0);
  if (active.length >= 5 && p.count >= MIN_SAMPLE) {
    const sorted = [...active].sort((a, b) => a - b);
    const median = sorted[Math.floor(sorted.length / 2)] as number;
    const peak = Math.max(...p.buckets);
    if (median > 0 && peak >= median * SPIKE_FACTOR) {
      found.push({
        kind: "frequency-spike",
        detail: `burst: peak bucket held ${peak} lines against a median of ${median}`,
      });
    }
    // Concentration is the other shape worth naming, and it is not the same as a
    // spike: a pattern can put 90% of its lines in three adjacent buckets without
    // any single one clearing the spike factor. That reads as an episode — a
    // deploy, a restart, an outage — rather than as background noise.
    const top = [...p.buckets].sort((a, b) => b - a);
    const concentrated = top.slice(0, 3).reduce((s, n) => s + n, 0);
    if (active.length >= 10 && concentrated / p.count >= 0.9) {
      found.push({
        kind: "error-burst",
        detail: `episodic: ${Math.round((concentrated / p.count) * 100)}% of its lines fall in 3 of ${active.length} active buckets`,
      });
    }
  }

  // --- Rarity --------------------------------------------------------------
  //
  // Rare is only interesting when it is also bad. A handful of DEBUG lines is not
  // news; a handful of FATAL lines is the most important thing in the file, and
  // sorting by count would bury it below a million INFO lines.
  if (
    p.count <= Math.max(5, totalLines * RARE_SHARE) &&
    (p.severity === "error" || p.severity === "fatal")
  ) {
    found.push({
      kind: "rare",
      detail: `rare but severe: only ${p.count} ${p.count === 1 ? "line" : "lines"}, at ${p.severity.toUpperCase()}`,
    });
  }

  // --- Variable distributions ---------------------------------------------
  for (const v of p.vars) {
    if (v.count < MIN_SAMPLE) continue;

    // A slot that never varies is not a variable. It usually means the masker was
    // too eager — a version number, a fixed port, a constant that happens to be
    // numeric — and saying so is more useful than showing a p99 of a constant.
    if (v.unique === 1 && v.top && v.top.length > 0) {
      found.push({
        kind: "single-value",
        detail: `slot ${v.slot} never varies — always ${v.top[0]?.value}`,
      });
      continue;
    }

    // Every line brought a new value. Worth naming because it changes how the slot
    // should be read: as an identifier to join on, not as a quantity to trend.
    if (v.kind === "id") {
      found.push({
        kind: "high-cardinality",
        detail: `slot ${v.slot} is an identifier — ~${v.unique.toLocaleString()} distinct values in ${v.count.toLocaleString()} lines`,
      });
      continue;
    }

    const n = v.numeric;
    if (!n) continue;

    // Two clusters of magnitude, not one. This is the shape of a fast path and a
    // slow path sharing a code path — a cache hit and a cache miss, a local call
    // and a remote one — and a mean sitting between them describes neither.
    //
    // The test is a wide gap between the median and the tail combined with a tight
    // median: p99 an order of magnitude above p50 means the tail is a different
    // population, whereas a merely skewed distribution keeps them within a factor
    // of a few.
    if (n.p50 > 0 && n.p99 >= n.p50 * 10 && n.p90 <= n.p50 * 3) {
      found.push({
        kind: "bimodal",
        detail: `slot ${v.slot} is bimodal — p50 ${fmt(n.p50, n.unit)} and p90 ${fmt(n.p90, n.unit)} sit together, p99 is ${fmt(n.p99, n.unit)}`,
      });
      continue;
    }

    // A long tail without the bimodal signature: still worth flagging, because the
    // worst case is what pages someone and the mean hides it entirely.
    if (n.p50 > 0 && n.max >= n.p50 * 100) {
      found.push({
        kind: "long-tail",
        detail: `slot ${v.slot} has a long tail — worst ${fmt(n.max, n.unit)} against a median of ${fmt(n.p50, n.unit)}`,
      });
    }
  }

  // Capped, and the cap is a readability decision rather than a performance one. A
  // pattern with eight constant slots produces eight `single-value` lines that say
  // the same thing eight times, and the effect is to bury the one detector that
  // fired for an interesting reason. `detect` already emits in priority order, so
  // truncating keeps the most consequential.
  return found.slice(0, MAX_PER_PATTERN);
}

/** Annotations rendered per pattern, past which they stop informing and start burying. */
const MAX_PER_PATTERN = 4;

/** A magnitude with its unit, rounded to something a person would say out loud. */
export function fmt(value: number, unit?: string): string {
  if (unit === "ms") {
    if (value >= 60000) return `${(value / 60000).toFixed(1)}min`;
    if (value >= 1000) return `${(value / 1000).toFixed(2)}s`;
    if (value >= 1) return `${round(value)}ms`;
    return `${round(value * 1000)}µs`;
  }
  if (unit === "bytes") {
    const units = ["B", "KB", "MB", "GB", "TB"];
    let v = value;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${round(v)}${units[i]}`;
  }
  return String(round(value));
}

/** Three significant-ish figures, without trailing zeros. */
function round(v: number): number {
  if (!Number.isFinite(v)) return v;
  const abs = Math.abs(v);
  if (abs >= 100) return Math.round(v);
  if (abs >= 10) return Math.round(v * 10) / 10;
  return Math.round(v * 100) / 100;
}
