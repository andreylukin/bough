// Model pricing, auto-derived from the models.dev catalog. src/pricing.json is
// a vendored snapshot (scripts/update-pricing.ts regenerates it) keyed
// "provider/model-id" → [input, output, cacheRead, cacheWrite] $/Mtok.
import catalog from "./pricing.json" with { type: "json" };

type Row = [number, number, number | null, number | null];
const rows = catalog as unknown as Record<string, Row>;

/** USD per million tokens. Cache rates fall back to the input rate when the catalog has none. */
export interface CostRates {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

/**
 * Rates for a bough model id, mirroring llm.clientFor routing: "openai:x" →
 * OpenAI, "vendor/model" → OpenRouter (falling back to vendor as a models.dev
 * provider), bare ids → Anthropic. Null when the catalog doesn't price it.
 */
export function ratesFor(model: string): CostRates | null {
  const row = model.startsWith("openai:")
    ? rows[`openai/${model.slice("openai:".length)}`]
    : model.includes("/")
    ? rows[`openrouter/${model}`] ?? rows[model]
    : rows[`anthropic/${model}`];
  if (!row) return null;
  const [input, output, cacheRead, cacheWrite] = row;
  return { input, output, cacheRead: cacheRead ?? input, cacheWrite: cacheWrite ?? input };
}

/**
 * Dollar cost of one LLM round. `inputTokens` arrives inclusive of cache
 * reads/writes (llm.ts normalizes every provider that way), so the cached share
 * is re-priced at its discounted rate. Null when the model isn't in the catalog.
 */
export function usageCostUsd(
  model: string,
  u: {
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens?: number;
    cacheCreationTokens?: number;
  },
): number | null {
  const r = ratesFor(model);
  if (!r) return null;
  const read = u.cacheReadTokens ?? 0;
  const write = u.cacheCreationTokens ?? 0;
  const fresh = Math.max(0, u.inputTokens - read - write);
  return (fresh * r.input + read * r.cacheRead + write * r.cacheWrite +
    u.outputTokens * r.output) / 1e6;
}
