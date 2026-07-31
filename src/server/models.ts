/**
 * `GET /models` — the picker's catalog, static rows plus whatever the key can reach.
 *
 * THE BUG THIS EXISTS FOR: `discoverOpenAIModels` and `mergeModels` were written,
 * tested, and called by nothing. The only catalog consumer is `tui/main.tsx`, which
 * passed the hardcoded `MODELS` array straight into `App` — so a user with a working
 * `OPENAI_API_KEY` still saw the same seven ids that were compiled in, and every model
 * OpenAI shipped after this file was written was unreachable from the product. The
 * discovery code was not broken; nothing asked it anything.
 *
 * SERVER-SIDE, and that is the load-bearing half. The key lives in `~/.bough/env`,
 * which the SERVER sources — the TUI is a separate process launched from a shell that
 * has no `OPENAI_API_KEY`, so discovery run there would find no key on a machine that
 * has one. Worse than useless: a TUI that found its own key would offer rows the
 * server cannot bill, and picking one would fail a turn instead of failing a fetch.
 * The catalog is therefore answered by the process that holds the credential.
 *
 * NEVER SLOWER THAN THE TERMINAL IT BLOCKS. The TUI awaits this before the first
 * frame (the precedent is the theme fetch), and `discoverOpenAIModels` allows itself
 * ten seconds, so a hung `api.openai.com` would be ten seconds of blank terminal.
 * A caller waits `deadlineMs` and no longer: past it the request is answered from the
 * static table, the discovery it started keeps running, and the NEXT ask — the next
 * TUI launch, or the next time the tab is opened — is served from the warm cache.
 * The list arriving one launch late is a cost nobody notices; a boot that hangs is.
 */
import {
  discoverOpenAIModels,
  mergeModels,
  type ModelRow,
  MODELS,
} from "../llm/client.ts";
import { type Handler, json } from "./http.ts";

/** What `GET /models` answers. An envelope, so a later field is not a breaking change. */
export interface ModelCatalog {
  models: ModelRow[];
}

/** How long a discovered list is trusted. Model lists change on OpenAI's schedule, not ours. */
const TTL_MS = 10 * 60_000;

/** How long a request waits on a cold discovery before answering without it. */
const DEADLINE_MS = 2_500;

/**
 * Module state, and deliberately not a `ctx` field: the cache is per PROCESS, the same
 * scope `cheapModel()` and the theme already use, and threading it through `AppCtx`
 * would put a mutable cache in a type that every handler destructures.
 */
let cached: { at: number; rows: ModelRow[] } | null = null;
let inflight: Promise<ModelRow[]> | null = null;

/** Drop the cache. For tests, so one test's stub does not answer the next one's. */
export function resetModelCatalog(): void {
  cached = null;
  inflight = null;
}

/**
 * The merged catalog. `now` and `deadlineMs` are injected for the same reason
 * `loadDefaults` takes a path: a test that waited two and a half real seconds on a
 * real socket would be a test nobody runs.
 *
 * One discovery in flight at a time. Without `inflight` a cold server answering three
 * simultaneous asks would open three requests to `/v1/models` and keep the last one's
 * answer, which is a rate limit waiting for a busy morning.
 */
export async function modelCatalog(
  opts: { now?: () => number; deadlineMs?: number; discover?: typeof discoverOpenAIModels } = {},
): Promise<ModelRow[]> {
  const now = opts.now ?? Date.now;
  const discover = opts.discover ?? discoverOpenAIModels;
  if (cached && now() - cached.at < TTL_MS) return mergeModels(MODELS, cached.rows);

  // `discoverOpenAIModels` documents that it never throws; the catch is here because
  // an unhandled rejection on a module-level promise would take the server down over a
  // model list, which is the one outcome worse than an incomplete picker.
  inflight ??= discover()
    .catch(() => [] as ModelRow[])
    .then((rows) => {
      cached = { at: now(), rows };
      inflight = null;
      return rows;
    });

  let timer: ReturnType<typeof setTimeout> | undefined;
  const rows = await Promise.race([
    inflight,
    // The stale rows, not an empty list: a cache past its TTL is still a better answer
    // than the static table alone, and it is what the user saw a minute ago.
    new Promise<ModelRow[]>((resolve) => {
      timer = setTimeout(() => resolve(cached?.rows ?? []), opts.deadlineMs ?? DEADLINE_MS);
    }),
  ]);
  // Cleared on both paths: a pending timer holds the event loop open, and `bough exec`
  // exits when its work is done rather than when a timeout it never needed fires.
  clearTimeout(timer);
  return mergeModels(MODELS, rows);
}

export const getModelsH: Handler = async () => json({ models: await modelCatalog() } as ModelCatalog);
