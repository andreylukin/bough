/**
 * The clustering tree. What is asserted here is behaviour under the variability
 * masking cannot type — hostnames, error strings, exception classes — because that
 * is the only thing this stage exists to handle.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { type Cluster, Drain, WILDCARD } from "./drain.ts";

const tok = (s: string) => s.split(" ");

test("identical lines form one cluster", () => {
  const d = new Drain();
  for (let i = 0; i < 5; i++) d.add(tok("server started on port <int>"));
  const cs = d.clusters();
  assert.equal(cs.length, 1);
  assert.equal(cs[0]?.count, 5);
});

test("a varying word generalizes to a wildcard", () => {
  // The whole point of the stage: `db-primary` and `db-replica` are values that no
  // regex could have called values.
  const d = new Drain();
  d.add(tok("connect to db-primary failed"));
  d.add(tok("connect to db-replica failed"));
  const cs = d.clusters();
  assert.equal(cs.length, 1);
  assert.deepEqual(cs[0]?.tokens, ["connect", "to", WILDCARD, "failed"]);
  assert.equal(cs[0]?.count, 2);
});

test("lines of different length never merge", () => {
  // Token count is the first tree level, so this is structural rather than a
  // threshold outcome.
  const d = new Drain();
  d.add(tok("a b c d e"));
  d.add(tok("a b c d e f"));
  assert.equal(d.clusters().length, 2);
});

test("unrelated lines of equal length stay apart", () => {
  // The failure this guards is a template that generalizes into `<*> <*> <*> <*>`
  // and then swallows everything of that length.
  const d = new Drain();
  d.add(tok("user alice logged in"));
  d.add(tok("disk sda3 nearly full"));
  assert.equal(d.clusters().length, 2);
});

test("a template only ever loses specificity", () => {
  // One pass is only sufficient because generalization is monotonic — a position
  // that varied once is assumed to vary again.
  const d = new Drain();
  d.add(tok("job <int> on host alpha done"));
  d.add(tok("job <int> on host beta done"));
  const after = [...(d.clusters()[0]?.tokens as string[])];
  d.add(tok("job <int> on host alpha done"));
  assert.deepEqual(d.clusters()[0]?.tokens, after, "a repeat re-specialized the template");
});

test("a wildcard position credits neither side of the similarity test", () => {
  // Otherwise a template that has generalized twice absorbs anything of the right
  // length, and each new line makes it worse.
  const d = new Drain({ threshold: 0.6 });
  d.add(tok("a b c d e"));
  d.add(tok("a b c d X")); // 4/5 = 0.8, merges; position 4 generalizes
  d.add(tok("q r s t u")); // shares nothing; must not join
  const cs = d.clusters();
  assert.equal(cs.length, 2);
});

test("a digit-bearing token does not index the tree", () => {
  // `worker-1` and `worker-2` are values wearing a word's clothes. Indexed on, they
  // give every worker its own subtree and fragment one statement into N clusters.
  const d = new Drain();
  for (let i = 0; i < 20; i++) d.add(tok(`worker-${i} finished cleanly now`));
  const cs = d.clusters();
  assert.equal(cs.length, 1);
  assert.equal(cs[0]?.count, 20);
  assert.equal(cs[0]?.tokens[0], WILDCARD);
});

test("the threshold is respected in both directions", () => {
  const strict = new Drain({ threshold: 0.9 });
  strict.add(tok("a b c d"));
  strict.add(tok("a b X Y"));
  assert.equal(strict.clusters().length, 2, "0.5 similarity cleared a 0.9 threshold");

  const loose = new Drain({ threshold: 0.4 });
  loose.add(tok("a b c d"));
  loose.add(tok("a b X Y"));
  assert.equal(loose.clusters().length, 1, "0.5 similarity failed a 0.4 threshold");
});

test("node fan-out is capped without losing lines", () => {
  const d = new Drain({ maxChildren: 5 });
  for (let i = 0; i < 50; i++) d.add(tok(`verb${String.fromCharCode(97 + (i % 26))}${i} noun thing here`));
  const total = d.clusters().reduce((n, c) => n + c.count, 0);
  assert.equal(total, 50, "lines went missing past the fan-out limit");
});

test("the cluster cap evicts least-recently-used and says so", () => {
  const evicted: Cluster[] = [];
  const d = new Drain({ maxClusters: 3, onEvict: (c) => evicted.push(c) });
  // Four mutually dissimilar shapes, so nothing merges.
  d.add(tok("alpha one two three"));
  d.add(tok("bravo four five six"));
  d.add(tok("charlie seven eight nine"));
  assert.ok(!d.truncated);
  d.add(tok("delta ten eleven twelve"));

  assert.ok(d.truncated, "eviction happened but was not reported");
  assert.equal(d.clusters().length, 3);
  assert.equal(evicted.length, 1);
  assert.equal(evicted[0]?.tokens[0], "alpha", "evicted the wrong cluster");
});

test("touching a cluster spares it from eviction", () => {
  const evicted: Cluster[] = [];
  const d = new Drain({ maxClusters: 2, onEvict: (c) => evicted.push(c) });
  d.add(tok("alpha one two three"));
  d.add(tok("bravo four five six"));
  d.add(tok("alpha one two three")); // alpha is now the most recent
  d.add(tok("charlie seven eight nine"));
  assert.equal(evicted[0]?.tokens[0], "bravo");
});

test("an evicted cluster stops being matched against", () => {
  // The bug this pins: a victim left in its leaf keeps absorbing lines while no
  // longer being counted anywhere, which corrupts later lines rather than merely
  // losing the old one.
  const d = new Drain({ maxClusters: 2 });
  d.add(tok("alpha one two three"));
  d.add(tok("bravo four five six"));
  d.add(tok("charlie seven eight nine")); // evicts alpha
  d.add(tok("alpha one two three")); // must create fresh, not resurrect a dead one
  const alpha = d.clusters().filter((c) => c.tokens[0] === "alpha");
  assert.equal(alpha.length, 1);
  assert.equal(alpha[0]?.count, 1, "line joined an evicted cluster");
});

test("an empty token list does not crash the tree", () => {
  const d = new Drain();
  d.add([]);
  d.add([]);
  assert.equal(d.clusters().length, 1);
  assert.equal(d.clusters()[0]?.count, 2);
});

test("a masked token does not index the tree either", () => {
  // The bug this pins: `[<hex>]` has no digit, so a digit-only rule indexes it
  // literally while the same statement's shorter ids key as wildcard. The two never
  // meet, and one log statement silently becomes two patterns whose statistics each
  // describe half the data.
  const d = new Drain();
  d.add(tok("ERROR [<hex>] connect to <ipv4> failed"));
  d.add(tok("ERROR [a1b2c3d] connect to <ipv4> failed"));
  assert.equal(d.clusters().length, 1);
  assert.equal(d.clusters()[0]?.count, 2);
});
