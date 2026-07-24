/**
 * extract(): pull a fact — or a schema-shaped object — out of a blob of text with
 * the cheap worker model, so the supervisor doesn't have to read the blob itself.
 * The program already holds the text (a file it read, output it captured); paying
 * frontier context for "which version is pinned in this lockfile" is the waste this
 * closes. When a JSON Schema is supplied it is compiled to a decoding grammar by
 * llama-server (client.ts `jsonSchema`), so the shape is structural, not prompted.
 *
 * Privacy tier: identical to digest.ts — the worker path is LOCAL-only by design,
 * because the text handed in is arbitrary file/command content that must not ride to
 * a remote API as a side effect. BOUGH_WORKER_FRONTIER is the one explicit opt-out.
 *
 * Unlike digestOutput this THROWS rather than degrading: extraction is a value the
 * program is about to branch on, so a silent "" or a fabricated shape is worse than
 * a catchable error telling the supervisor to read the text itself.
 */
import { workerIfRunning } from "./runtime.ts";
import { workerComplete } from "./client.ts";
import { frontierComplete, frontierWorkerModel } from "./frontier.ts";

/** Injectable completion for tests: (system, user, jsonSchema?) → reply text. */
export type ExtractCompleter = (
  system: string,
  user: string,
  jsonSchema?: Record<string, unknown>,
) => Promise<string>;

/** The worker's slot context is ~8K tokens — bigger input must be sliced by the caller. */
const MAX_CHARS = 12_000;
/** Extraction must never stall a turn; the 3-minute program cap is not a deadline. */
const DEADLINE_MS = 30_000;

const SYSTEM = [
  "You extract requested information from text for a coding agent. Answer ONLY from the",
  "text given — never guess, never add commentary, preamble, or explanation. If the text",
  "does not contain the answer, reply exactly: NOT FOUND.",
].join(" ");

/**
 * Ask the worker for `instruction` over `text`. With `schema` the reply is decoded
 * under that JSON Schema and returned parsed; without one it is returned as trimmed
 * text. Throws on oversized input, no reachable worker, the deadline, or an
 * unparseable reply.
 */
export async function extractFrom(
  text: string,
  instruction: string,
  schema?: Record<string, unknown>,
  complete: ExtractCompleter = defaultComplete,
): Promise<unknown> {
  if (!text.trim()) throw new Error("extract: empty text");
  if (!instruction.trim()) throw new Error("extract: empty instruction");
  if (text.length > MAX_CHARS) {
    throw new Error(
      `extract: text is ${text.length} chars, over the ${MAX_CHARS}-char worker limit — ` +
        `slice it (grep/slice the relevant region) and call extract again`,
    );
  }
  const user = `TEXT:\n${text}\n\nEXTRACT: ${instruction}`;

  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`extract: worker timed out after ${DEADLINE_MS}ms`)),
      DEADLINE_MS,
    );
  });
  let reply: string;
  try {
    reply = await Promise.race([complete(SYSTEM, user, schema), deadline]);
  } finally {
    clearTimeout(timer);
  }

  if (!schema) return reply.trim();
  try {
    return JSON.parse(reply);
  } catch {
    throw new Error(`extract: worker reply did not parse as JSON: ${reply.slice(0, 300)}`);
  }
}

/** Live path: an already-running worker only, and strictly local — no fallback. */
async function defaultComplete(
  system: string,
  user: string,
  jsonSchema?: Record<string, unknown>,
): Promise<string> {
  if (frontierWorkerModel()) {
    return await frontierComplete({ system, user, maxTokens: 512, jsonSchema });
  }
  const url = await workerIfRunning();
  if (!url) {
    throw new Error("extract: no local worker running — read the text yourself instead");
  }
  return await workerComplete(url, {
    system,
    user,
    maxTokens: 512,
    temperature: 0.2,
    jsonSchema,
    cachePrompt: true,
  });
}
