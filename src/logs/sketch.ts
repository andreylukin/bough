/**
 * The three bounded accumulators the log pipeline is built on.
 *
 * WHY SKETCHES AT ALL. The pipeline's promise is that a 10-million-line file costs
 * the same memory as a 10-thousand-line one, and every naive way to compute what
 * the output shows breaks that promise. A p99 wants every value sorted; a unique
 * count wants every value in a set; an example line wants the line kept. All three
 * are O(n) in the input, and all three are answerable to within a few percent in
 * constant space — which is the whole trade this module makes. A p99 that is right
 * to 1% is worth exactly as much as an exact one when you are deciding which
 * pattern to investigate, and it is worth infinitely more when the exact one would
 * have exhausted memory and produced nothing.
 *
 * NOTHING HERE IS PROBABILISTIC ABOUT *WHICH* ANSWER IT GIVES. Each structure is
 * deterministic given the same input sequence — `Reservoir` takes a seed rather
 * than reaching for `Math.random`, so a test asserts on exact sampled lines and a
 * rerun on the same file prints the same examples. A formatter that shuffled its
 * examples between runs would make two analyses of the same log impossible to
 * diff, which is the main thing anyone does with them.
 *
 * Clean-room implementations from the published algorithms:
 *   - DDSketch — Masson, Rim & Lee, VLDB 2019: a logarithmic bucket mapping whose
 *     relative error is bounded by construction rather than estimated after.
 *   - HyperLogLog — Flajolet et al. 2007, with the bias corrections from Heule,
 *     Nunkesser & Hall 2013 at the two ends where the raw estimator is known bad.
 *   - Reservoir sampling — Vitter's Algorithm R, 1985.
 *
 * PURE AND DEPENDENCY-FREE. No clock, no filesystem, no npm. Every class takes its
 * configuration in the constructor and its data through `add`.
 */

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/**
 * MurmurHash3 x86 32-bit, over the UTF-16 code units of a string.
 *
 * HyperLogLog's whole error analysis assumes hash bits are independent and
 * uniform, so this cannot be a convenience hash like summing char codes — a
 * multiplicative-sum hash puts `10.0.1.15` and `10.0.1.16` in adjacent buckets and
 * the register maxima stop being independent, which shows up as a cardinality
 * estimate that is confidently wrong rather than noisy. Murmur3 is the standard
 * choice, it is thirty lines, and it has no dependencies.
 *
 * Code units rather than UTF-8 bytes: the input is JS strings throughout, the two
 * agree for the ASCII that dominates log values, and disagreeing on non-ASCII only
 * means a different — equally uniform — bucket assignment.
 */
export function murmur3(key: string, seed = 0): number {
  const c1 = 0xcc9e2d51;
  const c2 = 0x1b873593;
  let h = seed >>> 0;
  const len = key.length;
  const blocks = len & ~0x3;

  for (let i = 0; i < blocks; i += 4) {
    let k =
      (key.charCodeAt(i) & 0xff) |
      ((key.charCodeAt(i + 1) & 0xff) << 8) |
      ((key.charCodeAt(i + 2) & 0xff) << 16) |
      ((key.charCodeAt(i + 3) & 0xff) << 24);
    k = Math.imul(k, c1);
    k = (k << 15) | (k >>> 17);
    k = Math.imul(k, c2);
    h ^= k;
    h = (h << 13) | (h >>> 19);
    h = (Math.imul(h, 5) + 0xe6546b64) | 0;
  }

  let k1 = 0;
  switch (len & 3) {
    case 3:
      k1 ^= (key.charCodeAt(blocks + 2) & 0xff) << 16;
    // falls through
    case 2:
      k1 ^= (key.charCodeAt(blocks + 1) & 0xff) << 8;
    // falls through
    case 1:
      k1 ^= key.charCodeAt(blocks) & 0xff;
      k1 = Math.imul(k1, c1);
      k1 = (k1 << 15) | (k1 >>> 17);
      k1 = Math.imul(k1, c2);
      h ^= k1;
  }

  h ^= len;
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  h ^= h >>> 16;
  return h >>> 0;
}

// ---------------------------------------------------------------------------
// DDSketch — relative-error quantiles
// ---------------------------------------------------------------------------

/** What `DDSketch.summary()` reports. `null` when nothing was ever added. */
export interface Quantiles {
  count: number;
  min: number;
  max: number;
  mean: number;
  p50: number;
  p90: number;
  p99: number;
}

/**
 * Quantiles with a bounded RELATIVE error, in space bounded by the input's range.
 *
 * The mapping is the whole idea: bucket `i` holds every value in
 * `(gamma^(i-1), gamma^i]`, so a bucket's width grows with the values it holds and
 * the error is proportional rather than absolute. That is the property log data
 * actually needs — `alpha = 0.01` means a reported p99 of 5s is within 50ms of the
 * true one AND a reported p50 of 20ms is within 0.2ms, from the same structure. A
 * fixed-width histogram has to choose which of those two to be wrong about, and
 * durations in one log routinely span microseconds to minutes.
 *
 * Bucket count is logarithmic in the range, not linear in the count: values from
 * 1µs to 10 minutes at 1% accuracy is roughly 2,300 buckets — a few tens of KB
 * worst case, and far less in practice because real durations cluster.
 *
 * NEGATIVES AND ZERO ARE NOT VALUES THE MAPPING HANDLES. `log(0)` is `-Infinity`
 * and negatives have no logarithm at all, so zeros are counted separately and
 * negatives go into a mirrored store keyed on `-v`. Log data has both — a delta
 * that went backwards, a `0ms` fast path — and silently dropping them would bias
 * every quantile toward the slow end, which is the direction that gets
 * investigated.
 */
export class DDSketch {
  /** `(1+alpha)/(1-alpha)`: the ratio between consecutive bucket boundaries. */
  private readonly gamma: number;
  private readonly logGamma: number;
  private readonly positive = new Map<number, number>();
  private readonly negative = new Map<number, number>();
  private zeros = 0;
  private total = 0;
  private minSeen = Number.POSITIVE_INFINITY;
  private maxSeen = Number.NEGATIVE_INFINITY;
  /**
   * Running sum, kept exactly — the mean is the one statistic that does NOT need a
   * sketch, and reconstructing it from bucket midpoints would import the sketch's
   * error into a number that could have been exact for the cost of one float.
   */
  private sum = 0;

  constructor(alpha = 0.01) {
    if (!(alpha > 0 && alpha < 1)) throw new RangeError(`alpha must be in (0,1), got ${alpha}`);
    this.gamma = (1 + alpha) / (1 - alpha);
    this.logGamma = Math.log(this.gamma);
  }

  private index(v: number): number {
    return Math.ceil(Math.log(v) / this.logGamma);
  }

  /** The representative value for a bucket: its geometric-ish midpoint. */
  private value(i: number): number {
    return (2 * Math.pow(this.gamma, i)) / (this.gamma + 1);
  }

  add(v: number): void {
    if (!Number.isFinite(v)) return;
    this.total++;
    this.sum += v;
    if (v < this.minSeen) this.minSeen = v;
    if (v > this.maxSeen) this.maxSeen = v;
    if (v === 0) {
      this.zeros++;
      return;
    }
    const store = v > 0 ? this.positive : this.negative;
    const i = this.index(Math.abs(v));
    store.set(i, (store.get(i) ?? 0) + 1);
  }

  get count(): number {
    return this.total;
  }

  /**
   * The value at rank `ceil(q * n)`, walking negatives descending, then zeros, then
   * positives ascending — which is the sorted order the ranks are defined over.
   */
  quantile(q: number): number {
    if (this.total === 0) return Number.NaN;
    // Rank is 1-based and clamped: q=0 must select the first element, not the
    // zeroth, and floating point makes `q*n` land just under an integer often
    // enough that an unclamped floor would report the wrong bucket at q=1.
    let rank = Math.ceil(q * this.total);
    if (rank < 1) rank = 1;
    if (rank > this.total) rank = this.total;

    const negKeys = [...this.negative.keys()].sort((a, b) => b - a);
    let seen = 0;
    for (const i of negKeys) {
      seen += this.negative.get(i) as number;
      if (seen >= rank) return -this.value(i);
    }
    seen += this.zeros;
    if (seen >= rank) return 0;

    const posKeys = [...this.positive.keys()].sort((a, b) => a - b);
    for (const i of posKeys) {
      seen += this.positive.get(i) as number;
      if (seen >= rank) return this.value(i);
    }
    return this.maxSeen;
  }

  /**
   * Min and max are the values actually seen, not bucket representatives.
   *
   * They are free to track exactly, and a `min` that read `4.97ms` when the log
   * plainly contains `5ms` reads as a bug in the tool rather than as the sketch
   * working as designed. Reserve the approximation for the numbers that need it.
   */
  summary(): Quantiles | null {
    if (this.total === 0) return null;
    // Quantiles are CLAMPED into the observed range, because mixing an approximate
    // statistic with two exact ones otherwise produces output that contradicts
    // itself: a bucket representative rounds up past the true maximum and the
    // report reads `p99=1.86s max=1.85s`. That is a real error of about 1% — well
    // inside what the sketch promises — but it does not read as one. It reads as
    // arithmetic that does not work, and a reader who notices stops believing the
    // other numbers too. Clamping cannot make an answer worse: the true quantile is
    // in range by definition, so the clamp only ever moves an estimate toward it.
    const clamp = (v: number) => Math.min(Math.max(v, this.minSeen), this.maxSeen);
    return {
      count: this.total,
      min: this.minSeen,
      max: this.maxSeen,
      mean: this.sum / this.total,
      p50: clamp(this.quantile(0.5)),
      p90: clamp(this.quantile(0.9)),
      p99: clamp(this.quantile(0.99)),
    };
  }

  /**
   * Bucket occupancy, ascending by value — the input the bimodality test reads.
   *
   * Exposed as a copy rather than the live map so a detector cannot corrupt the
   * sketch by iterating it destructively.
   */
  buckets(): { value: number; count: number }[] {
    const out: { value: number; count: number }[] = [];
    for (const i of [...this.negative.keys()].sort((a, b) => b - a)) {
      out.push({ value: -this.value(i), count: this.negative.get(i) as number });
    }
    if (this.zeros > 0) out.push({ value: 0, count: this.zeros });
    for (const i of [...this.positive.keys()].sort((a, b) => a - b)) {
      out.push({ value: this.value(i), count: this.positive.get(i) as number });
    }
    return out;
  }
}

// ---------------------------------------------------------------------------
// HyperLogLog — cardinality
// ---------------------------------------------------------------------------

/**
 * Approximate distinct-count in fixed space, from the leading-zero distribution.
 *
 * The intuition: hash each value uniformly, and the longest run of leading zeros
 * you observe is a witness to how many distinct things you hashed — seeing 10
 * leading zeros suggests roughly 2^10 distinct values, because that is how rare
 * such a hash is. One such witness is far too noisy to use, so the first `p` bits
 * select one of `m = 2^p` registers and the estimate averages their witnesses
 * harmonically. Duplicates cannot move a register, which is exactly why the count
 * is of DISTINCT values and why a value can be fed in a million times for free.
 *
 * `p = 12` (4,096 registers, one byte each) gives a standard error of about 1.6%
 * in 4KB. That is the right point on the curve here: the number is rendered as
 * `unique=1,847` next to a top-values list, where 1.6% is invisible, and the
 * alternative — an exact `Set` — is unbounded in exactly the case the field is
 * most interesting, a high-cardinality variable like a request ID.
 *
 * BOTH ENDS OF THE RANGE GET A CORRECTION, because the raw harmonic estimator is
 * known to be biased at both and log data lives at the low end. Most variables have
 * three distinct values, not three million, and uncorrected HLL reports small
 * cardinalities badly enough to turn `unique=3` into `unique=7` — which would break
 * the enum detection downstream, since that test is a threshold on this number.
 */
export class HyperLogLog {
  private readonly p: number;
  private readonly m: number;
  private readonly registers: Uint8Array;
  private readonly alphaMM: number;

  constructor(p = 12) {
    if (!Number.isInteger(p) || p < 4 || p > 16) {
      throw new RangeError(`p must be an integer in [4,16], got ${p}`);
    }
    this.p = p;
    this.m = 1 << p;
    this.registers = new Uint8Array(this.m);
    // Flajolet's bias constant. The general form is used above 128 registers; the
    // three small sizes have their own tabulated values.
    const alpha =
      this.m === 16
        ? 0.673
        : this.m === 32
          ? 0.697
          : this.m === 64
            ? 0.709
            : 0.7213 / (1 + 1.079 / this.m);
    this.alphaMM = alpha * this.m * this.m;
  }

  add(value: string): void {
    const h = murmur3(value);
    // Top `p` bits pick the register; the remaining bits supply the zero run. The
    // run length is counted over those remaining bits ONLY — reusing the index bits
    // would correlate a register with the values that land in it.
    const idx = h >>> (32 - this.p);
    const rest = (h << this.p) >>> this.p;
    // Math.clz32 counts across all 32 bits, so the `p` bits shifted out read as
    // leading zeros and have to be discounted. +1 because the rank is 1-based: the
    // first bit being 1 is a run of length one, not zero.
    const rank = rest === 0 ? 32 - this.p + 1 : Math.clz32(rest) - this.p + 1;
    if (rank > (this.registers[idx] as number)) this.registers[idx] = rank;
  }

  count(): number {
    let inverseSum = 0;
    let empty = 0;
    for (let i = 0; i < this.m; i++) {
      const r = this.registers[i] as number;
      if (r === 0) empty++;
      inverseSum += 1 / Math.pow(2, r);
    }
    const estimate = this.alphaMM / inverseSum;

    // Small range: with registers still empty, linear counting is strictly better
    // than the harmonic estimator, and it is exact until collisions start.
    if (estimate <= 2.5 * this.m && empty > 0) {
      return Math.round(this.m * Math.log(this.m / empty));
    }
    // Large range: a 32-bit hash starts colliding near 2^32, and past ~143M the
    // estimator saturates. This inverts the collision probability to recover.
    const TWO32 = 4294967296;
    if (estimate > TWO32 / 30) {
      return Math.round(-TWO32 * Math.log(1 - estimate / TWO32));
    }
    return Math.round(estimate);
  }
}

// ---------------------------------------------------------------------------
// Reservoir sampling — examples
// ---------------------------------------------------------------------------

/**
 * A uniform sample of `k` items from a stream of unknown length, in `k` space.
 *
 * Vitter's Algorithm R: the first `k` items are kept outright; item `n` after that
 * replaces a random one of them with probability `k/n`. The invariant is that every
 * item seen so far is equally likely to be held, which is what makes the printed
 * examples representative of the pattern rather than of its beginning.
 *
 * TAKING THE FIRST `k` WOULD HAVE BEEN WRONG, not merely lazier. Logs are ordered
 * by time and a pattern's first occurrences are its startup ones — the connection
 * errors from before the pool warmed, the requests before the cache filled. A
 * "first k" example set shows the boot sequence of every pattern and never shows
 * steady state, which is the opposite of the sample anyone wants.
 *
 * SEEDED, NOT `Math.random`. Two runs over one file must produce byte-identical
 * output or the analyses cannot be diffed, and a test cannot assert on a sampled
 * line otherwise. The generator is xorshift32 — trivial, well-distributed enough
 * for a sampling decision, and reproducible across platforms in a way that
 * `Math.random` explicitly does not promise.
 */
export class Reservoir<T> {
  private readonly items: T[] = [];
  private seen = 0;
  private state: number;

  constructor(
    private readonly k: number,
    seed = 0x9e3779b9,
  ) {
    // A zero state is xorshift's fixed point — it emits zero forever, so every
    // replacement would target index 0 and the sample would collapse to two items.
    this.state = seed === 0 ? 0x9e3779b9 : seed >>> 0;
  }

  private next(): number {
    let x = this.state;
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    this.state = x;
    return x;
  }

  add(item: T): void {
    this.seen++;
    if (this.items.length < this.k) {
      this.items.push(item);
      return;
    }
    // `next() % seen` is biased when `seen` does not divide 2^32, but the bias is
    // on the order of 2^-32 per draw and it perturbs which representative example
    // is shown — not any statistic. Rejection sampling here would buy nothing.
    const j = this.next() % this.seen;
    if (j < this.k) this.items[j] = item;
  }

  /** The sample, in insertion order of the slots. Never longer than `k`. */
  sample(): T[] {
    return [...this.items];
  }

  get total(): number {
    return this.seen;
  }
}

// ---------------------------------------------------------------------------
// Top-k
// ---------------------------------------------------------------------------

/**
 * Exact counts for the most frequent values, with a hard cap on tracked keys.
 *
 * This is deliberately NOT a Space-Saving or Count-Min sketch, even though the rest
 * of the module is sketch-based. Frequency estimators earn their keep by finding
 * heavy hitters among millions of keys; here the consumer prints three values with
 * percentages, and the cases that matter — an enum with four values, a status code
 * with three — are exactly the ones an exact map handles in a few hundred bytes
 * while a sketch would report `200 (91%)` as `200 (89%)` for no benefit.
 *
 * The cap is what keeps it bounded. Once `capacity` distinct keys are tracked, new
 * ones are counted only in `overflow` rather than admitted. That biases toward
 * values seen EARLY, which for a genuinely high-cardinality variable makes the
 * top-k list meaningless — so `saturated` is exposed, and the formatter suppresses
 * the list rather than printing a lie. The cardinality estimate is what carries the
 * information in that case, and it is unaffected.
 */
export class TopK {
  private readonly counts = new Map<string, number>();
  private overflow = 0;

  constructor(private readonly capacity = 1024) {}

  add(value: string): void {
    const existing = this.counts.get(value);
    if (existing !== undefined) {
      this.counts.set(value, existing + 1);
      return;
    }
    if (this.counts.size >= this.capacity) {
      this.overflow++;
      return;
    }
    this.counts.set(value, 1);
  }

  /** True once values were dropped, meaning the ranking is no longer trustworthy. */
  get saturated(): boolean {
    return this.overflow > 0;
  }

  get tracked(): number {
    return this.counts.size;
  }

  /**
   * The `n` most frequent tracked values, ties broken by value so the output is
   * stable across runs rather than dependent on Map insertion order.
   */
  top(n: number): { value: string; count: number }[] {
    return [...this.counts.entries()]
      .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
      .slice(0, n)
      .map(([value, count]) => ({ value, count }));
  }
}
