#!/usr/bin/env -S deno run --allow-net=models.dev --allow-write=src
// Regenerate src/llm/pricing.json from models.dev — the community model catalog
// (same upstream pi/opencode generate their model defs from). Run when pricing
// drifts or new models appear:
//
//   deno run --allow-net=models.dev --allow-write=src scripts/update-pricing.ts
//
// Output is a flat map "provider/model-id" → [input, output, cacheRead, cacheWrite,
// contextWindow]: rates in USD per million tokens (null = rate not published),
// context window in tokens (null = unknown). One line per model so regens diff
// reviewably. Lookup semantics live in src/llm/pricing.ts.

interface CatalogModel {
  cost?: { input?: number; output?: number; cache_read?: number; cache_write?: number };
  limit?: { context?: number; output?: number };
}
interface CatalogProvider {
  models?: Record<string, CatalogModel>;
}

const res = await fetch("https://models.dev/api.json");
if (!res.ok) throw new Error(`models.dev fetch failed: ${res.status}`);
const catalog = await res.json() as Record<string, CatalogProvider>;

const out: Record<string, [number, number, number | null, number | null, number | null]> = {};
for (const [provider, p] of Object.entries(catalog)) {
  for (const [id, m] of Object.entries(p.models ?? {})) {
    const c = m.cost;
    if (c?.input === undefined || c.output === undefined) continue; // unpriced (open weights etc.)
    out[`${provider}/${id}`] = [
      c.input,
      c.output,
      c.cache_read ?? null,
      c.cache_write ?? null,
      m.limit?.context ?? null,
    ];
  }
}

const keys = Object.keys(out).sort();
const body = keys.map((k) => `${JSON.stringify(k)}: ${JSON.stringify(out[k])}`).join(",\n");
await Deno.writeTextFile("src/llm/pricing.json", `{\n${body}\n}\n`);
console.log(`src/llm/pricing.json: ${keys.length} priced models from ${Object.keys(catalog).length} providers`);
