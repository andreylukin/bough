import { assert, assertEquals } from "jsr:@std/assert@1";
import { contextWindowFor, ratesFor, usageCostUsd } from "./pricing.ts";

Deno.test("ratesFor resolves all three provider id forms", () => {
  // Bare id → anthropic; catalog values drift on regen, so assert shape not price.
  const claude = ratesFor("claude-opus-4-8");
  assert(claude && claude.input > 0 && claude.output > claude.input);
  assert(claude.cacheRead < claude.input); // discounted, not the input fallback

  const openai = ratesFor("openai:gpt-5");
  assert(openai && openai.input > 0);

  // Slash id → openrouter (or the vendor as a models.dev provider).
  assert(ratesFor("moonshotai/kimi-k3"));
  assert(ratesFor("z-ai/glm-5.2"));

  assertEquals(ratesFor("no-such-model-xyz"), null);
  assertEquals(ratesFor("nope/no-such-model-xyz"), null);
});

Deno.test("usageCostUsd re-prices the cached share of inclusive input", () => {
  const r = ratesFor("claude-opus-4-8")!;
  // 1.25M input of which 200k cache-read + 50k cache-write → 1M fresh.
  const cost = usageCostUsd("claude-opus-4-8", {
    inputTokens: 1_250_000,
    outputTokens: 1_000_000,
    cacheReadTokens: 200_000,
    cacheCreationTokens: 50_000,
  })!;
  const expected = r.input + 0.2 * r.cacheRead + 0.05 * r.cacheWrite + r.output;
  assert(Math.abs(cost - expected) < 1e-9);
  // Cached tokens must cost LESS than pricing them at the full input rate.
  assert(cost < 1.25 * r.input + r.output);
});

Deno.test("contextWindowFor reports the catalog window per id form", () => {
  const w = contextWindowFor("claude-opus-4-8");
  assert(w !== null && w >= 200_000);
  assert((contextWindowFor("openai:gpt-5") ?? 0) > 0);
  assert((contextWindowFor("moonshotai/kimi-k3") ?? 0) > 0);
  assertEquals(contextWindowFor("no-such-model-xyz"), null);
});

Deno.test("usageCostUsd is null for unpriced models and handles missing cache fields", () => {
  assertEquals(usageCostUsd("no-such-model-xyz", { inputTokens: 1000, outputTokens: 10 }), null);
  const r = ratesFor("claude-haiku-4-5")!;
  const cost = usageCostUsd("claude-haiku-4-5", { inputTokens: 1_000_000, outputTokens: 0 })!;
  assert(Math.abs(cost - r.input) < 1e-9);
});
