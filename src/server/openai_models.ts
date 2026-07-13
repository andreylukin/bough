/**
 * Dynamic OpenAI model discovery. When an OpenAI key is present we pull
 * GET /v1/models and offer the chat-capable ids in the picker alongside the
 * static MODELS table. Cached in-process: refreshed when the key is (re)set via
 * PUT /config/keys, and lazily once per process from getConfig. A failed fetch
 * (bad key, offline) just leaves the dynamic list empty — the static entries
 * still work.
 */
import type { ModelRow } from "../turn.ts";

// Chat models only: completions/embeddings/audio/image/etc. ids would either 404
// on chat/completions or make no sense in a coding-agent picker. Dated snapshots
// (-YYYY-MM-DD) are dropped — the alias id always exists and tracks the latest.
const INCLUDE = /^(gpt-|o\d|chatgpt-)/;
const EXCLUDE =
  /audio|realtime|tts|whisper|embed|dall|image|moderation|transcribe|search-preview|instruct/;
const DATED = /-\d{4}-\d{2}-\d{2}$/;
const CAP = 25;

/** Pure filter/mapper so the selection rules are testable without the network. */
export function filterOpenAIModels(ids: string[]): ModelRow[] {
  return ids
    .filter((id) => INCLUDE.test(id) && !EXCLUDE.test(id) && !DATED.test(id))
    .sort((a, b) => b.localeCompare(a)) // newer families first (gpt-5.2 before gpt-5)
    .slice(0, CAP)
    .map((id) => ({ id: `openai:${id}`, label: `${id} (OpenAI)`, provider: "openai" as const }));
}

let cache: ModelRow[] = [];
let fetchedOnce = false;

export function openaiModels(): ModelRow[] {
  return cache;
}

export async function refreshOpenAIModels(): Promise<ModelRow[]> {
  fetchedOnce = true;
  const key = Deno.env.get("OPENAI_API_KEY")?.trim();
  if (!key) {
    cache = [];
    return cache;
  }
  const base = Deno.env.get("OPENAI_API_BASE") ?? "https://api.openai.com";
  try {
    const res = await fetch(`${base}/v1/models`, {
      headers: { authorization: `Bearer ${key}` },
      signal: AbortSignal.timeout(10_000),
    });
    if (!res.ok) throw new Error(`${res.status}`);
    const body = await res.json() as { data?: { id?: unknown }[] };
    cache = filterOpenAIModels(
      (body.data ?? []).map((m) => m.id).filter((id): id is string => typeof id === "string"),
    );
  } catch {
    // keep whatever we had — a transient failure shouldn't empty the picker
  }
  return cache;
}

/** Kick one background refresh per process once a key exists (getConfig calls this). */
export function ensureOpenAIModels(): void {
  if (fetchedOnce || !Deno.env.get("OPENAI_API_KEY")?.trim()) return;
  refreshOpenAIModels().catch(() => {});
}

/** Static table first, dynamic entries after, deduped by id. */
export function mergeModels(staticModels: ModelRow[], dynamic: ModelRow[]): ModelRow[] {
  const seen = new Set(staticModels.map((m) => m.id));
  return [...staticModels, ...dynamic.filter((m) => !seen.has(m.id))];
}
