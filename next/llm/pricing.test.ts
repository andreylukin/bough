/**
 * Pricing is pure lookup and pure arithmetic, so this file is entirely offline
 * string and number math over the vendored snapshot.
 *
 * The case that actually matters is the negative one: an unknown model must come
 * back `null`, never `0`. A zero renders in the status bar as "this round was
 * free", which the user has no way to distinguish from a real zero — so the
 * catalog's ignorance has to stay visible all the way up.
 *
 * `node:assert` rather than `@std/assert`: jsr.io is denied by this environment's
 * egress policy (same constraint the other test files document).
 */

import { deepStrictEqual, ok, strictEqual } from "node:assert";
import {
  catalogKey,
  catalogKeys,
  contextWindowFor,
  isPriced,
  ratesFor,
  usageCostUsd,
} from "./pricing.ts";

Deno.test("catalogKeys mirrors the client's routing rule", () => {
  deepStrictEqual(catalogKeys("claude-opus-5"), ["anthropic/claude-opus-5"]);
  deepStrictEqual(catalogKeys("openai:gpt-5"), ["openai/gpt-5"]);
  // OpenRouter first, then the vendor's own models.dev row as a fallback — many
  // vendors are listed directly and that row is usable when OpenRouter has none.
  deepStrictEqual(catalogKeys("google/gemini-2.5-pro"), [
    "openrouter/google/gemini-2.5-pro",
    "google/gemini-2.5-pro",
  ]);
});

Deno.test("the vendored snapshot prices the models bough ships with", () => {
  for (const model of ["claude-opus-5", "claude-haiku-4-5", "openai:gpt-5"]) {
    ok(isPriced(model), `${model} should be in the catalog`);
    const rates = ratesFor(model);
    ok(rates, model);
    ok(rates.input > 0 && rates.output > 0, model);
  }
});

Deno.test("an unknown model is null everywhere, never zero", () => {
  const model = "no-such-vendor/no-such-model";
  strictEqual(isPriced(model), false);
  strictEqual(catalogKey(model), undefined);
  strictEqual(ratesFor(model), null);
  strictEqual(contextWindowFor(model), null);
  strictEqual(usageCostUsd(model, { inputTokens: 1e6, outputTokens: 1e6 }), null);
});

Deno.test("cache rates fall back to the input rate when the catalog has none", () => {
  // Pick a row the snapshot carries with null cache slots.
  const rates = ratesFor("openai:gpt-5");
  ok(rates);
  ok(rates.cacheRead > 0 && rates.cacheWrite > 0);
  ok(rates.cacheRead <= rates.input, "a cache read is never dearer than fresh input");
});

Deno.test("contextWindowFor reports a real window for a known model", () => {
  const window = contextWindowFor("claude-opus-5");
  ok(typeof window === "number" && window > 100_000, `got ${window}`);
});

Deno.test("usageCostUsd: the cached share is subtracted out and re-priced", () => {
  const model = "claude-opus-5";
  const r = ratesFor(model);
  ok(r);
  // inputTokens arrives INCLUSIVE of reads and writes, so the fresh share here is
  // 1M - 400k - 100k = 500k.
  const cost = usageCostUsd(model, {
    inputTokens: 1_000_000,
    outputTokens: 1_000_000,
    cacheReadTokens: 400_000,
    cacheWriteTokens: 100_000,
  });
  const expected = (500_000 * r.input + 400_000 * r.cacheRead + 100_000 * r.cacheWrite +
    1_000_000 * r.output) / 1e6;
  ok(cost !== null);
  ok(Math.abs(cost - expected) < 1e-9, `${cost} vs ${expected}`);
  // The whole point of the discount: the same tokens billed fresh cost more.
  const uncached = usageCostUsd(model, { inputTokens: 1_000_000, outputTokens: 1_000_000 });
  ok(uncached !== null && uncached > cost);
});

Deno.test("usageCostUsd: nullish cache counts behave like zero, not like NaN", () => {
  const withNulls = usageCostUsd("claude-opus-5", {
    inputTokens: 1000,
    outputTokens: 1000,
    cacheReadTokens: null,
    cacheWriteTokens: null,
  });
  const without = usageCostUsd("claude-opus-5", { inputTokens: 1000, outputTokens: 1000 });
  strictEqual(withNulls, without);
  ok(withNulls !== null && Number.isFinite(withNulls));
});

Deno.test("usageCostUsd: an over-counted cache share cannot drive the fresh share negative", () => {
  // Defensive: a provider that reports reads exceeding the total must not produce
  // a negative bill.
  const cost = usageCostUsd("claude-opus-5", {
    inputTokens: 100,
    outputTokens: 0,
    cacheReadTokens: 900,
  });
  ok(cost !== null && cost >= 0);
});
