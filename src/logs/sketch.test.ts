/**
 * The sketches, checked against distributions whose true answers are known in
 * closed form. Every assertion is a bound on the error, not an exact value — an
 * approximate structure asserted exactly would pin an implementation detail and
 * break on any legitimate retune of `alpha` or `p`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { DDSketch, HyperLogLog, murmur3, Reservoir, TopK } from "./sketch.ts";

// ---------------------------------------------------------------------------
// murmur3
// ---------------------------------------------------------------------------

test("murmur3 matches published vectors", () => {
  // The reference values for MurmurHash3_x86_32; if this drifts, every HLL bound
  // below is being met by accident.
  assert.equal(murmur3(""), 0);
  assert.equal(murmur3("", 1), 0x514e28b7);
  assert.equal(murmur3("hello"), 0x248bfa47);
});

test("murmur3 spreads adjacent keys across the register space", () => {
  // The property HLL actually depends on: near-identical inputs must not land in
  // near-identical buckets. A summing hash fails this and silently degrades the
  // cardinality estimate rather than erroring.
  const top = new Set<number>();
  for (let i = 0; i < 64; i++) top.add(murmur3(`10.0.1.${i}`) >>> 20);
  assert.ok(top.size > 48, `adjacent keys collided into ${top.size} of 64 buckets`);
});

// ---------------------------------------------------------------------------
// DDSketch
// ---------------------------------------------------------------------------

test("DDSketch holds relative error across four orders of magnitude", () => {
  // The property that motivates a logarithmic mapping: one structure is accurate
  // at 1µs and at 10s simultaneously. A fixed-width histogram cannot do this.
  const s = new DDSketch(0.01);
  const values: number[] = [];
  for (let i = 1; i <= 10000; i++) {
    values.push(i);
    s.add(i);
  }
  values.sort((a, b) => a - b);
  for (const q of [0.5, 0.9, 0.99]) {
    const truth = values[Math.ceil(q * values.length) - 1] as number;
    const got = s.quantile(q);
    assert.ok(
      Math.abs(got - truth) / truth <= 0.01,
      `q=${q}: got ${got}, truth ${truth} — outside 1% relative error`,
    );
  }
});

test("DDSketch reports min, max and mean exactly", () => {
  const s = new DDSketch();
  for (const v of [5, 10, 15, 20]) s.add(v);
  const sum = s.summary();
  assert.ok(sum);
  // These are tracked, not derived from buckets — an approximate min would read as
  // a bug against a log the user can see with their own eyes.
  assert.equal(sum.min, 5);
  assert.equal(sum.max, 20);
  assert.equal(sum.mean, 12.5);
  assert.equal(sum.count, 4);
});

test("DDSketch handles zero and negative values", () => {
  // Real logs contain both — a 0ms fast path and a delta that went backwards.
  // Dropping them would bias every quantile toward the slow end.
  const s = new DDSketch();
  for (const v of [-100, -50, 0, 0, 50, 100]) s.add(v);
  const sum = s.summary();
  assert.ok(sum);
  assert.equal(sum.count, 6);
  assert.equal(sum.min, -100);
  assert.equal(sum.max, 100);
  assert.ok(s.quantile(0.01) < 0, "low quantile should land among the negatives");
  assert.equal(s.quantile(0.5), 0, "the median of this set is a zero");
});

test("DDSketch reports nothing when nothing was added", () => {
  assert.equal(new DDSketch().summary(), null);
  assert.ok(Number.isNaN(new DDSketch().quantile(0.5)));
});

test("DDSketch stays bounded under a repeated value", () => {
  // The memory claim: cost tracks the value RANGE, not the input count.
  const s = new DDSketch();
  for (let i = 0; i < 100000; i++) s.add(42);
  assert.equal(s.buckets().length, 1);
  assert.equal(s.count, 100000);
});

// ---------------------------------------------------------------------------
// HyperLogLog
// ---------------------------------------------------------------------------

test("HyperLogLog is near-exact at the low cardinalities logs actually have", () => {
  // This is the range that matters most: enum detection downstream is a threshold
  // on this number, so an uncorrected estimator reporting 3 uniques as 7 would
  // silently reclassify variables.
  for (const n of [1, 3, 10, 50]) {
    const h = new HyperLogLog();
    for (let i = 0; i < n; i++) h.add(`value-${i}`);
    assert.equal(h.count(), n, `${n} distinct values misreported as ${h.count()}`);
  }
});

test("HyperLogLog stays within a few percent at high cardinality", () => {
  const h = new HyperLogLog();
  const n = 100000;
  for (let i = 0; i < n; i++) h.add(`req-${i}`);
  const err = Math.abs(h.count() - n) / n;
  // p=12 gives a ~1.6% standard error; 5% is a generous multiple that will not
  // flake while still catching a genuinely broken estimator.
  assert.ok(err < 0.05, `estimated ${h.count()} for ${n} — ${(err * 100).toFixed(1)}% off`);
});

test("HyperLogLog ignores duplicates entirely", () => {
  // The reason a value can be fed in a million times for free.
  const h = new HyperLogLog();
  for (let i = 0; i < 10000; i++) h.add("same");
  assert.equal(h.count(), 1);
});

test("HyperLogLog rejects an out-of-range precision", () => {
  assert.throws(() => new HyperLogLog(2), RangeError);
  assert.throws(() => new HyperLogLog(20), RangeError);
});

// ---------------------------------------------------------------------------
// Reservoir
// ---------------------------------------------------------------------------

test("Reservoir keeps everything until it is full", () => {
  const r = new Reservoir<number>(5);
  for (const v of [1, 2, 3]) r.add(v);
  assert.deepEqual(r.sample(), [1, 2, 3]);
});

test("Reservoir samples the whole stream, not its beginning", () => {
  // The behaviour that distinguishes this from `slice(0, k)`: a pattern's first
  // occurrences are its startup ones, and an example set that only ever shows the
  // boot sequence is the opposite of representative.
  const r = new Reservoir<number>(10, 12345);
  for (let i = 0; i < 1000; i++) r.add(i);
  const s = r.sample();
  assert.equal(s.length, 10);
  assert.equal(r.total, 1000);
  assert.ok(
    s.some((v) => v > 500),
    `sample ${JSON.stringify(s)} drew nothing from the second half of the stream`,
  );
});

test("Reservoir is deterministic for a given seed", () => {
  // Two analyses of one file must be diffable, which they are not if the examples
  // shuffle between runs.
  const fill = () => {
    const r = new Reservoir<number>(8, 999);
    for (let i = 0; i < 500; i++) r.add(i);
    return r.sample();
  };
  assert.deepEqual(fill(), fill());
});

test("Reservoir survives a zero seed", () => {
  // Zero is xorshift's fixed point: unhandled, every replacement targets index 0
  // and the sample collapses to two distinct items.
  const r = new Reservoir<number>(5, 0);
  for (let i = 0; i < 200; i++) r.add(i);
  assert.equal(new Set(r.sample()).size, 5);
});

// ---------------------------------------------------------------------------
// TopK
// ---------------------------------------------------------------------------

test("TopK ranks by frequency with a stable tiebreak", () => {
  const t = new TopK();
  for (const v of ["200", "200", "200", "404", "404", "500"]) t.add(v);
  assert.deepEqual(t.top(2), [
    { value: "200", count: 3 },
    { value: "404", count: 2 },
  ]);
  // Ties break on the value so output does not depend on Map insertion order.
  const tie = new TopK();
  for (const v of ["b", "a"]) tie.add(v);
  assert.deepEqual(
    tie.top(2).map((e) => e.value),
    ["a", "b"],
  );
});

test("TopK caps tracked keys and admits it", () => {
  // Past the cap the ranking is biased toward early values, so the formatter needs
  // to know to suppress it rather than print a confident lie.
  const t = new TopK(10);
  for (let i = 0; i < 100; i++) t.add(`v${i}`);
  assert.equal(t.tracked, 10);
  assert.ok(t.saturated);
  const small = new TopK(10);
  for (let i = 0; i < 10; i++) small.add(`v${i}`);
  assert.ok(!small.saturated);
});

test("DDSketch quantiles never escape the observed range", () => {
  // Mixing an approximate statistic with two exact ones must not produce output
  // that contradicts itself. A bucket representative rounds up past the true
  // maximum and the report reads `p99=1.86s max=1.85s` — a 1% error that reads as
  // arithmetic that does not work.
  const s = new DDSketch(0.05);
  for (const v of [28, 29, 31, 33, 40, 45, 55, 120, 314, 1847]) s.add(v);
  const sum = s.summary();
  assert.ok(sum);
  for (const q of [sum.p50, sum.p90, sum.p99]) {
    assert.ok(q >= sum.min && q <= sum.max, `${q} outside [${sum.min}, ${sum.max}]`);
  }
});
