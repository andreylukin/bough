//! Port of `src/logs/sketch.ts` — the three bounded accumulators the log
//! pipeline is built on, plus the hash they depend on.
//!
//! "NOTHING HERE IS PROBABILISTIC ABOUT *WHICH* ANSWER IT GIVES. Each structure
//! is deterministic given the same input sequence" — `Reservoir` takes a seed
//! rather than reaching for a thread RNG, so a test asserts on exact sampled
//! lines and a rerun on the same file prints the same examples.
//!
//! Clean-room implementations from the published algorithms: DDSketch (Masson,
//! Rim & Lee, VLDB 2019), HyperLogLog (Flajolet et al. 2007 + Heule et al. 2013
//! corrections), Reservoir sampling (Vitter's Algorithm R, 1985).

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// MurmurHash3 x86 32-bit, over the UTF-16 code units of a string.
///
/// The TS original hashes `key.charCodeAt(i) & 0xff` — the LOW BYTE of each
/// UTF-16 code unit, not UTF-8 bytes. An off-the-shelf murmur3 over UTF-8 would
/// not reproduce the published vectors for non-ASCII input, and would silently
/// change every cardinality estimate. The length fed into the finalizer is the
/// UTF-16 length for the same reason.
pub fn murmur3(key: &str, seed: u32) -> u32 {
    let bytes: Vec<u8> = key.encode_utf16().map(|u| (u & 0xff) as u8).collect();
    let c1: u32 = 0xcc9e_2d51;
    let c2: u32 = 0x1b87_3593;
    let mut h: u32 = seed;
    let len = bytes.len();
    let blocks = len & !0x3;

    let mut i = 0usize;
    while i < blocks {
        let mut k = (bytes[i] as u32)
            | ((bytes[i + 1] as u32) << 8)
            | ((bytes[i + 2] as u32) << 16)
            | ((bytes[i + 3] as u32) << 24);
        k = k.wrapping_mul(c1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(c2);
        h ^= k;
        h = h.rotate_left(13);
        h = h.wrapping_mul(5).wrapping_add(0xe654_6b64);
        i += 4;
    }

    let mut k1: u32 = 0;
    let tail = len & 3;
    if tail == 3 {
        k1 ^= (bytes[blocks + 2] as u32) << 16;
    }
    if tail >= 2 {
        k1 ^= (bytes[blocks + 1] as u32) << 8;
    }
    if tail >= 1 {
        k1 ^= bytes[blocks] as u32;
        k1 = k1.wrapping_mul(c1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(c2);
        h ^= k1;
    }

    h ^= len as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

// ---------------------------------------------------------------------------
// DDSketch — relative-error quantiles
// ---------------------------------------------------------------------------

/// What `DDSketch::summary()` reports. `None` when nothing was ever added.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantiles {
    pub count: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
}

/// Quantiles with a bounded RELATIVE error, in space bounded by the input's
/// range. Bucket `i` holds every value in `(gamma^(i-1), gamma^i]`.
///
/// Zeros are counted separately and negatives go into a mirrored store keyed on
/// `-v`: log data has both, and dropping them would bias every quantile toward
/// the slow end, which is the direction that gets investigated.
#[derive(Debug, Clone)]
pub struct DDSketch {
    gamma: f64,
    log_gamma: f64,
    positive: HashMap<i64, u64>,
    negative: HashMap<i64, u64>,
    zeros: u64,
    total: u64,
    min_seen: f64,
    max_seen: f64,
    /// Running sum, kept exactly — the mean is the one statistic that does NOT
    /// need a sketch.
    sum: f64,
}

impl Default for DDSketch {
    fn default() -> Self {
        Self::new(0.01).expect("0.01 is in (0,1)")
    }
}

impl DDSketch {
    /// `Err` where the TS constructor throws `RangeError`.
    pub fn new(alpha: f64) -> Result<Self, String> {
        if !(alpha > 0.0 && alpha < 1.0) {
            return Err(format!("alpha must be in (0,1), got {alpha}"));
        }
        let gamma = (1.0 + alpha) / (1.0 - alpha);
        Ok(Self {
            gamma,
            log_gamma: gamma.ln(),
            positive: HashMap::new(),
            negative: HashMap::new(),
            zeros: 0,
            total: 0,
            min_seen: f64::INFINITY,
            max_seen: f64::NEG_INFINITY,
            sum: 0.0,
        })
    }

    fn index(&self, v: f64) -> i64 {
        (v.ln() / self.log_gamma).ceil() as i64
    }

    /// The representative value for a bucket: its geometric-ish midpoint.
    fn value(&self, i: i64) -> f64 {
        (2.0 * self.gamma.powi(i as i32)) / (self.gamma + 1.0)
    }

    pub fn add(&mut self, v: f64) {
        if !v.is_finite() {
            return;
        }
        self.total += 1;
        self.sum += v;
        if v < self.min_seen {
            self.min_seen = v;
        }
        if v > self.max_seen {
            self.max_seen = v;
        }
        if v == 0.0 {
            self.zeros += 1;
            return;
        }
        let i = self.index(v.abs());
        let store = if v > 0.0 {
            &mut self.positive
        } else {
            &mut self.negative
        };
        *store.entry(i).or_insert(0) += 1;
    }

    pub fn count(&self) -> u64 {
        self.total
    }

    /// The value at rank `ceil(q * n)`, walking negatives descending, then
    /// zeros, then positives ascending.
    pub fn quantile(&self, q: f64) -> f64 {
        if self.total == 0 {
            return f64::NAN;
        }
        // Rank is 1-based and clamped: q=0 must select the first element, and
        // floating point makes `q*n` land just under an integer often enough
        // that an unclamped floor would report the wrong bucket at q=1.
        let mut rank = (q * self.total as f64).ceil() as i64;
        if rank < 1 {
            rank = 1;
        }
        if rank > self.total as i64 {
            rank = self.total as i64;
        }
        let rank = rank as u64;

        let mut neg_keys: Vec<i64> = self.negative.keys().copied().collect();
        neg_keys.sort_unstable_by(|a, b| b.cmp(a));
        let mut seen: u64 = 0;
        for i in neg_keys {
            seen += self.negative[&i];
            if seen >= rank {
                return -self.value(i);
            }
        }
        seen += self.zeros;
        if seen >= rank {
            return 0.0;
        }
        let mut pos_keys: Vec<i64> = self.positive.keys().copied().collect();
        pos_keys.sort_unstable();
        for i in pos_keys {
            seen += self.positive[&i];
            if seen >= rank {
                return self.value(i);
            }
        }
        self.max_seen
    }

    /// Min, max and mean are exact; the quantiles are CLAMPED into the observed
    /// range so an approximate p99 can never read above an exact max.
    pub fn summary(&self) -> Option<Quantiles> {
        if self.total == 0 {
            return None;
        }
        let clamp = |v: f64| v.max(self.min_seen).min(self.max_seen);
        Some(Quantiles {
            count: self.total,
            min: self.min_seen,
            max: self.max_seen,
            mean: self.sum / self.total as f64,
            p50: clamp(self.quantile(0.5)),
            p90: clamp(self.quantile(0.9)),
            p99: clamp(self.quantile(0.99)),
        })
    }

    /// Bucket occupancy, ascending by value — the input the bimodality test
    /// reads. A copy, not the live map.
    pub fn buckets(&self) -> Vec<(f64, u64)> {
        let mut out = Vec::new();
        let mut neg: Vec<i64> = self.negative.keys().copied().collect();
        neg.sort_unstable_by(|a, b| b.cmp(a));
        for i in neg {
            out.push((-self.value(i), self.negative[&i]));
        }
        if self.zeros > 0 {
            out.push((0.0, self.zeros));
        }
        let mut pos: Vec<i64> = self.positive.keys().copied().collect();
        pos.sort_unstable();
        for i in pos {
            out.push((self.value(i), self.positive[&i]));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// HyperLogLog — cardinality
// ---------------------------------------------------------------------------

/// Approximate distinct-count in fixed space, from the leading-zero
/// distribution. `p = 12` (4,096 registers) gives ~1.6% standard error in 4KB.
///
/// Both ends of the range get a correction, because the raw harmonic estimator
/// is biased at both and log data lives at the low end — enum detection
/// downstream is a threshold on this number.
#[derive(Debug, Clone)]
pub struct HyperLogLog {
    p: u32,
    m: usize,
    registers: Vec<u8>,
    alpha_mm: f64,
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self::new(12).expect("12 is in [4,16]")
    }
}

impl HyperLogLog {
    /// `Err` where the TS constructor throws `RangeError`.
    pub fn new(p: u32) -> Result<Self, String> {
        if !(4..=16).contains(&p) {
            return Err(format!("p must be an integer in [4,16], got {p}"));
        }
        let m = 1usize << p;
        let alpha = match m {
            16 => 0.673,
            32 => 0.697,
            64 => 0.709,
            _ => 0.7213 / (1.0 + 1.079 / m as f64),
        };
        Ok(Self {
            p,
            m,
            registers: vec![0u8; m],
            alpha_mm: alpha * m as f64 * m as f64,
        })
    }

    pub fn add(&mut self, value: &str) {
        let h = murmur3(value, 0);
        // Top `p` bits pick the register; the remaining bits supply the zero
        // run. Reusing the index bits would correlate a register with the
        // values that land in it.
        let idx = (h >> (32 - self.p)) as usize;
        let rest = (h << self.p) >> self.p;
        let rank = if rest == 0 {
            32 - self.p + 1
        } else {
            rest.leading_zeros() - self.p + 1
        } as u8;
        if rank > self.registers[idx] {
            self.registers[idx] = rank;
        }
    }

    pub fn count(&self) -> u64 {
        let mut inverse_sum = 0.0f64;
        let mut empty = 0usize;
        for &r in &self.registers {
            if r == 0 {
                empty += 1;
            }
            inverse_sum += 1.0 / 2f64.powi(r as i32);
        }
        let estimate = self.alpha_mm / inverse_sum;
        let m = self.m as f64;

        // Small range: with registers still empty, linear counting is strictly
        // better than the harmonic estimator.
        if estimate <= 2.5 * m && empty > 0 {
            return js_round(m * (m / empty as f64).ln()) as u64;
        }
        // Large range: a 32-bit hash starts colliding near 2^32.
        const TWO32: f64 = 4_294_967_296.0;
        if estimate > TWO32 / 30.0 {
            return js_round(-TWO32 * (1.0 - estimate / TWO32).ln()) as u64;
        }
        js_round(estimate) as u64
    }
}

/// `Math.round`: half UP, not half away from zero and not banker's rounding.
/// The estimator only ever feeds it positives, but spelling it out keeps the
/// difference from `f64::round` (half away from zero) visible.
fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

// ---------------------------------------------------------------------------
// Reservoir sampling — examples
// ---------------------------------------------------------------------------

/// A uniform sample of `k` items from a stream of unknown length, in `k` space
/// (Vitter's Algorithm R).
///
/// Seeded rather than randomly initialized: two runs over one file must produce
/// byte-identical output or the analyses cannot be diffed. The generator is
/// xorshift32.
#[derive(Debug, Clone)]
pub struct Reservoir<T> {
    items: Vec<T>,
    seen: u64,
    state: u32,
    k: usize,
}

impl<T: Clone> Reservoir<T> {
    pub fn new(k: usize, seed: u32) -> Self {
        Self {
            items: Vec::new(),
            seen: 0,
            // A zero state is xorshift's fixed point — it emits zero forever,
            // so every replacement would target index 0 and the sample would
            // collapse to two items.
            state: if seed == 0 { 0x9e37_79b9 } else { seed },
            k,
        }
    }

    fn next(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    pub fn add(&mut self, item: T) {
        self.seen += 1;
        if self.items.len() < self.k {
            self.items.push(item);
            return;
        }
        // `next() % seen` is biased on the order of 2^-32 per draw, and it
        // perturbs which representative example is shown — not any statistic.
        let j = (self.next() as u64) % self.seen;
        if (j as usize) < self.k {
            self.items[j as usize] = item;
        }
    }

    /// The sample, in insertion order of the slots. Never longer than `k`.
    pub fn sample(&self) -> Vec<T> {
        self.items.clone()
    }

    pub fn total(&self) -> u64 {
        self.seen
    }
}

// ---------------------------------------------------------------------------
// Top-k
// ---------------------------------------------------------------------------

/// Exact counts for the most frequent values, with a hard cap on tracked keys.
///
/// Deliberately NOT a Space-Saving or Count-Min sketch: the consumer prints
/// three values with percentages, and the cases that matter — an enum with four
/// values, a status code with three — are exactly the ones an exact map handles
/// in a few hundred bytes.
#[derive(Debug, Clone)]
pub struct TopK {
    counts: HashMap<String, u64>,
    overflow: u64,
    capacity: usize,
}

impl Default for TopK {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl TopK {
    pub fn new(capacity: usize) -> Self {
        Self {
            counts: HashMap::new(),
            overflow: 0,
            capacity,
        }
    }

    pub fn add(&mut self, value: &str) {
        if let Some(existing) = self.counts.get_mut(value) {
            *existing += 1;
            return;
        }
        // Once `capacity` distinct keys are tracked, new ones are counted only
        // in `overflow`. That biases toward values seen EARLY, so `saturated`
        // is exposed and the formatter suppresses the list rather than printing
        // a lie.
        if self.counts.len() >= self.capacity {
            self.overflow += 1;
            return;
        }
        self.counts.insert(value.to_string(), 1);
    }

    /// True once values were dropped, meaning the ranking is untrustworthy.
    pub fn saturated(&self) -> bool {
        self.overflow > 0
    }

    pub fn tracked(&self) -> usize {
        self.counts.len()
    }

    /// The `n` most frequent tracked values, ties broken by value so the output
    /// is stable across runs rather than dependent on map iteration order.
    pub fn top(&self, n: usize) -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> =
            self.counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        entries.truncate(n);
        entries
    }
}

#[cfg(test)]
mod tests {
    //! Port of `src/logs/sketch.test.ts`. Every assertion is a bound on the
    //! error, not an exact value — an approximate structure asserted exactly
    //! would pin an implementation detail.
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn murmur3_matches_published_vectors() {
        // If this drifts, every HLL bound below is being met by accident.
        assert_eq!(murmur3("", 0), 0);
        assert_eq!(murmur3("", 1), 0x514e_28b7);
        assert_eq!(murmur3("hello", 0), 0x248b_fa47);
        // The extra vectors small.md pins.
        assert_eq!(murmur3("a", 0), 0x3c25_69b2);
        assert_eq!(murmur3("abcd", 0), 0x43ed_676a);
        assert_eq!(murmur3("hello, world", 0), 0x149b_bb7f);
    }

    #[test]
    fn murmur3_spreads_adjacent_keys_across_the_register_space() {
        // The property HLL depends on: near-identical inputs must not land in
        // near-identical buckets.
        let mut top = HashSet::new();
        for i in 0..64 {
            top.insert(murmur3(&format!("10.0.1.{i}"), 0) >> 20);
        }
        assert!(
            top.len() > 48,
            "adjacent keys collided into {} of 64",
            top.len()
        );
    }

    #[test]
    fn ddsketch_holds_relative_error_across_four_orders_of_magnitude() {
        let mut s = DDSketch::new(0.01).unwrap();
        let mut values: Vec<f64> = Vec::new();
        for i in 1..=10_000 {
            values.push(i as f64);
            s.add(i as f64);
        }
        for q in [0.5f64, 0.9, 0.99] {
            let truth = values[(q * values.len() as f64).ceil() as usize - 1];
            let got = s.quantile(q);
            assert!(
                (got - truth).abs() / truth <= 0.01,
                "q={q}: got {got}, truth {truth} — outside 1% relative error"
            );
        }
    }

    #[test]
    fn ddsketch_reports_min_max_and_mean_exactly() {
        let mut s = DDSketch::default();
        for v in [5.0, 10.0, 15.0, 20.0] {
            s.add(v);
        }
        let sum = s.summary().unwrap();
        assert_eq!(sum.min, 5.0);
        assert_eq!(sum.max, 20.0);
        assert_eq!(sum.mean, 12.5);
        assert_eq!(sum.count, 4);
    }

    #[test]
    fn ddsketch_handles_zero_and_negative_values() {
        let mut s = DDSketch::default();
        for v in [-100.0, -50.0, 0.0, 0.0, 50.0, 100.0] {
            s.add(v);
        }
        let sum = s.summary().unwrap();
        assert_eq!(sum.count, 6);
        assert_eq!(sum.min, -100.0);
        assert_eq!(sum.max, 100.0);
        assert!(
            s.quantile(0.01) < 0.0,
            "low quantile should land among the negatives"
        );
        assert_eq!(s.quantile(0.5), 0.0, "the median of this set is a zero");
    }

    #[test]
    fn ddsketch_reports_nothing_when_nothing_was_added() {
        assert!(DDSketch::default().summary().is_none());
        assert!(DDSketch::default().quantile(0.5).is_nan());
    }

    #[test]
    fn ddsketch_stays_bounded_under_a_repeated_value() {
        // The memory claim: cost tracks the value RANGE, not the input count.
        let mut s = DDSketch::default();
        for _ in 0..100_000 {
            s.add(42.0);
        }
        assert_eq!(s.buckets().len(), 1);
        assert_eq!(s.count(), 100_000);
    }

    #[test]
    fn ddsketch_quantiles_never_escape_the_observed_range() {
        let mut s = DDSketch::new(0.05).unwrap();
        for v in [
            28.0, 29.0, 31.0, 33.0, 40.0, 45.0, 55.0, 120.0, 314.0, 1847.0,
        ] {
            s.add(v);
        }
        let sum = s.summary().unwrap();
        for q in [sum.p50, sum.p90, sum.p99] {
            assert!(
                q >= sum.min && q <= sum.max,
                "{q} outside [{}, {}]",
                sum.min,
                sum.max
            );
        }
    }

    #[test]
    fn ddsketch_rejects_an_out_of_range_alpha() {
        assert!(DDSketch::new(0.0).is_err());
        assert!(DDSketch::new(1.0).is_err());
    }

    #[test]
    fn hll_is_near_exact_at_the_low_cardinalities_logs_actually_have() {
        // Enum detection downstream is a threshold on this number.
        for n in [1u64, 3, 10, 50] {
            let mut h = HyperLogLog::default();
            for i in 0..n {
                h.add(&format!("value-{i}"));
            }
            assert_eq!(h.count(), n, "{n} distinct values misreported");
        }
    }

    #[test]
    fn hll_stays_within_a_few_percent_at_high_cardinality() {
        let mut h = HyperLogLog::default();
        let n = 100_000u64;
        for i in 0..n {
            h.add(&format!("req-{i}"));
        }
        let err = (h.count() as f64 - n as f64).abs() / n as f64;
        assert!(err < 0.05, "estimated {} for {n}", h.count());
    }

    #[test]
    fn hll_ignores_duplicates_entirely() {
        let mut h = HyperLogLog::default();
        for _ in 0..10_000 {
            h.add("same");
        }
        assert_eq!(h.count(), 1);
    }

    #[test]
    fn hll_rejects_an_out_of_range_precision() {
        assert!(HyperLogLog::new(2).is_err());
        assert!(HyperLogLog::new(20).is_err());
    }

    #[test]
    fn reservoir_keeps_everything_until_it_is_full() {
        let mut r: Reservoir<i32> = Reservoir::new(5, 0x9e37_79b9);
        for v in [1, 2, 3] {
            r.add(v);
        }
        assert_eq!(r.sample(), vec![1, 2, 3]);
    }

    #[test]
    fn reservoir_samples_the_whole_stream_not_its_beginning() {
        let mut r: Reservoir<i32> = Reservoir::new(10, 12345);
        for i in 0..1000 {
            r.add(i);
        }
        let s = r.sample();
        assert_eq!(s.len(), 10);
        assert_eq!(r.total(), 1000);
        assert!(
            s.iter().any(|&v| v > 500),
            "sample {s:?} drew nothing from the second half of the stream"
        );
    }

    #[test]
    fn reservoir_is_deterministic_for_a_given_seed() {
        let fill = || {
            let mut r: Reservoir<i32> = Reservoir::new(8, 999);
            for i in 0..500 {
                r.add(i);
            }
            r.sample()
        };
        assert_eq!(fill(), fill());
    }

    #[test]
    fn reservoir_survives_a_zero_seed() {
        let mut r: Reservoir<i32> = Reservoir::new(5, 0);
        for i in 0..200 {
            r.add(i);
        }
        let unique: HashSet<i32> = r.sample().into_iter().collect();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn topk_ranks_by_frequency_with_a_stable_tiebreak() {
        let mut t = TopK::default();
        for v in ["200", "200", "200", "404", "404", "500"] {
            t.add(v);
        }
        assert_eq!(
            t.top(2),
            vec![("200".to_string(), 3), ("404".to_string(), 2)]
        );
        let mut tie = TopK::default();
        for v in ["b", "a"] {
            tie.add(v);
        }
        let values: Vec<String> = tie.top(2).into_iter().map(|e| e.0).collect();
        assert_eq!(values, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn topk_caps_tracked_keys_and_admits_it() {
        let mut t = TopK::new(10);
        for i in 0..100 {
            t.add(&format!("v{i}"));
        }
        assert_eq!(t.tracked(), 10);
        assert!(t.saturated());
        let mut small = TopK::new(10);
        for i in 0..10 {
            small.add(&format!("v{i}"));
        }
        assert!(!small.saturated());
    }
}
