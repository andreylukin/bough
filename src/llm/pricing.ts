/**
 * The vendored cost and context-window catalog.
 *
 * The invariant this module holds is that **a price is a lookup, never a
 * negotiation**: `pricing.json` is a snapshot committed to the repo, so a cost
 * figure never depends on the network being up, on a key being present, or on the
 * provider agreeing to talk about money. A model the snapshot does not know is
 * reported as `null` — an honest "we don't price this" — rather than silently
 * costed at zero, because a zero would read as "free" in the status bar and that
 * is a lie the user cannot detect.
 *
 * Second invariant, and the reason `catalogKey` is exported: **the catalog is
 * keyed by the same routing the client uses.** `client.ts` decides which provider
 * a model id belongs to; this file has to reach the same conclusion to find the
 * row, and the two derivations live in different files. `client.test.ts` pins them
 * together over a table of ids — if they drift, the catalog silently stops pricing
 * a whole provider and every cost quietly becomes `null`.
 *
 * The catalog is auto-derived from the models.dev snapshot: keys are
 * `"provider/model-id"`, values are
 * `[input, output, cacheRead, cacheWrite, contextWindow]` — dollars per million
 * tokens, then a token count. `null` in a rate slot means the catalog has no
 * separate rate for it, which we resolve to the input rate.
 */
import catalog from "./pricing.json" with { type: "json" };

/** `[input, output, cacheRead, cacheWrite, contextWindow]`. */
type Row = [number, number, number | null, number | null, number | null];

const rows = catalog as unknown as Record<string, Row>;

/** USD per million tokens. Cache rates fall back to the input rate when the catalog has none. */
export interface CostRates {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

/** The token counts a round can be billed for. Mirrors `Usage`, nullish included. */
export interface BillableTokens {
  inputTokens: number;
  outputTokens: number;
  /** Included in `inputTokens`; re-priced at the discounted read rate. */
  cacheReadTokens?: number | null;
  /** Included in `inputTokens`; re-priced at the write rate. */
  cacheWriteTokens?: number | null;
}

/**
 * The catalog keys a bough model id could match, most specific first.
 *
 * Mirrors `providerFor` in `client.ts`: an `openai:x` id is OpenAI proper, any
 * other `vendor/model` id is routed through OpenRouter, and a bare id is
 * Anthropic. The OpenRouter case tries the `openrouter/` key first and then the
 * bare `vendor/model` key, because models.dev also lists many of those vendors
 * directly and the direct row is a usable fallback when OpenRouter has not
 * published its own.
 *
 * Exported so the drift test can assert this agrees with the client's routing.
 */
export function catalogKeys(model: string): string[] {
  if (model.startsWith("openai:")) return [`openai/${model.slice("openai:".length)}`];
  if (model.includes("/")) return [`openrouter/${model}`, model];
  return [`anthropic/${model}`];
}

/** The single key a model id resolved to, or `undefined` when nothing matched. */
export function catalogKey(model: string): string | undefined {
  return catalogKeys(model).find((k) => k in rows);
}

function rowFor(model: string): Row | undefined {
  const key = catalogKey(model);
  return key === undefined ? undefined : rows[key];
}

/** Whether the vendored snapshot knows this model at all. */
export function isPriced(model: string): boolean {
  return catalogKey(model) !== undefined;
}

/** Rates for a bough model id; `null` when the catalog does not price it. */
export function ratesFor(model: string): CostRates | null {
  const row = rowFor(model);
  if (!row) return null;
  const [input, output, cacheRead, cacheWrite] = row;
  return { input, output, cacheRead: cacheRead ?? input, cacheWrite: cacheWrite ?? input };
}

/**
 * The model's context window in tokens; `null` when the catalog does not know it.
 *
 * The turn runner uses this to name the limit in a context-overflow error (spec
 * §5) — which is why an unknown window must stay `null` rather than defaulting to
 * some plausible number that would produce a confidently wrong error message.
 */
export function contextWindowFor(model: string): number | null {
  return rowFor(model)?.[4] ?? null;
}

/**
 * Dollar cost of one round. `inputTokens` arrives INCLUSIVE of cache reads and
 * writes — every provider client normalizes it that way so the context meter can
 * show the true prompt size — so the cached share is subtracted out and re-priced
 * at its own rate. Returns `null` when the model is not in the catalog.
 */
export function usageCostUsd(model: string, u: BillableTokens): number | null {
  const r = ratesFor(model);
  if (!r) return null;
  const read = u.cacheReadTokens ?? 0;
  const write = u.cacheWriteTokens ?? 0;
  const fresh = Math.max(0, u.inputTokens - read - write);
  return (fresh * r.input + read * r.cacheRead + write * r.cacheWrite +
    u.outputTokens * r.output) / 1e6;
}
