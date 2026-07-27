/**
 * Streaming transport, and the one place a finished round becomes persisted parts.
 *
 * Three invariants live here, all of them learned the hard way and none of them
 * derivable from a provider's docs.
 *
 * **1. A stream that stops without its completion marker is a failure, not a short
 * answer.** Two of the three providers deliver a round as a sequence of SSE frames
 * with no length prefix, so a connection cut halfway through is indistinguishable
 * from a model that finished early — unless the caller insists on seeing the
 * terminal marker. Returning the partial round as success hands the turn runner a
 * half-assembled tool call, which it then executes. `sseEvents` therefore guards
 * every read with a stall timeout, and the callers treat "ended without a
 * completion marker" as a retryable transport fault.
 *
 * **2. A tool call with missing arguments was truncated; it is not a call with no
 * arguments.** `parseToolArgs` refuses to invent `{}` for a tool whose schema has
 * required fields. Swallowing it turns a transport fault into a bogus schema
 * complaint the model then has to make sense of, and the program runs with the
 * wrong input. `{}` stays legitimate for a tool that requires nothing — the
 * schema, not the emptiness, decides.
 *
 * **3. Reasoning is display-only.** `blocksToParts` persists a `reasoning` part so
 * the UI can fold it, and drops the provider `meta` that rode along with it. That
 * meta is replayed verbatim *within* a turn (some providers reject a tool call
 * whose signed thinking was altered), but it never reaches the database, because
 * across turns reasoning is dropped from replay entirely (plan §6.4) and a stored
 * signature would only invite someone to echo it back.
 *
 * Nothing here knows a provider by name — the provider string is a label for error
 * text. That is what lets all three clients in `client.ts` share this file.
 */
import { LlmError } from "../errors.ts";
import type { LlmBlock, LlmToolDef } from "../types.ts";
import type { Part } from "../schema/parts.ts";

/** A stream that sends no bytes for this long is treated as dropped. */
export const STALL_TIMEOUT_MS = 60_000;

/** Knobs the tests turn down so a stall assertion does not take a minute. */
export interface SseOpts {
  stallMs?: number;
}

/**
 * `reader.read()` with a stall guard, so a silently dead connection surfaces as a
 * retryable `LlmError` instead of hanging the turn until the user interrupts.
 */
function readWithStallGuard(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  provider: string,
  stallMs: number,
): Promise<ReadableStreamReadResult<Uint8Array>> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reader.cancel().catch(() => {});
      reject(
        new LlmError(`${provider}: stream stalled (no data for ${Math.round(stallMs / 1000)}s)`),
      );
    }, stallMs);
    reader.read().then(resolve, reject).finally(() => clearTimeout(timer));
  });
}

/**
 * Yield each SSE `data:` payload from a response body — raw, INCLUDING the
 * `[DONE]` sentinel, because the callers treat that one differently.
 *
 * Splits on `\n`, trims, and skips non-`data:` lines (SSE comments and
 * keepalives). A trailing un-newlined fragment at stream end is dropped: it is by
 * definition incomplete, and the caller's "did we see a completion marker?" check
 * is what catches the truncation.
 */
export async function* sseEvents(
  body: ReadableStream<Uint8Array>,
  provider: string,
  opts: SseOpts = {},
): AsyncIterable<string> {
  const stallMs = opts.stallMs ?? STALL_TIMEOUT_MS;
  const reader = body.getReader();
  const dec = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await readWithStallGuard(reader, provider, stallMs);
    if (done) break;
    buffer += dec.decode(value, { stream: true });
    let nl: number;
    while ((nl = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (!line.startsWith("data:")) continue;
      yield line.slice(5).trim();
    }
  }
}

/** Map a non-2xx provider response to a classified `LlmError`. Never returns. */
export async function throwHttpError(provider: string, res: Response): Promise<never> {
  const retryAfter = Number(res.headers.get("retry-after"));
  const body = await res.text().catch(() => "");
  throw new LlmError(
    `${provider}: ${res.status} ${body}`.trimEnd(),
    res.status,
    Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter * 1000 : undefined,
  );
}

/**
 * Decode a tool call's raw `arguments` JSON.
 *
 * A round that streams a call's name but none of (or half of) its payload was cut
 * off mid-call — a transport fault, not a model mistake. Throwing a status-less
 * (and therefore retryable) `LlmError` puts it back through the retry ring, where
 * a re-streamed round usually lands intact. The alternative, which both
 * OpenAI-shaped paths used to do, was to fall back to `{}`: the tool then ran with
 * an empty input and the model was handed a schema complaint about an argument it
 * had in fact written.
 *
 * `{}` is still correct for a tool with no required fields, so `tool` — the
 * declared schema — decides, not the emptiness.
 */
export function parseToolArgs(
  provider: string,
  raw: string | undefined,
  tool: LlmToolDef | undefined,
  name: string,
): unknown {
  if (raw) {
    try {
      return JSON.parse(raw);
    } catch {
      throw new LlmError(
        `${provider}: ${name} call has malformed arguments (truncated mid-call)`,
      );
    }
  }
  const required = tool?.inputSchema?.required;
  if (Array.isArray(required) && required.length > 0) {
    throw new LlmError(`${provider}: ${name} call arrived with no arguments (truncated mid-call)`);
  }
  return {};
}

/**
 * A finished round's blocks → the parts persisted on the supervisor message.
 *
 * Provider `meta` is deliberately not carried across: see invariant 3 above. An
 * empty `reasoning` block (a redacted thinking block has no displayable text) is
 * dropped rather than persisted as an empty fold, which would render as a
 * mysterious blank row in the transcript.
 */
export function blocksToParts(blocks: LlmBlock[]): Part[] {
  const parts: Part[] = [];
  for (const b of blocks) {
    switch (b.type) {
      case "text":
        if (b.text) parts.push({ type: "text", text: b.text });
        break;
      case "reasoning":
        if (b.text.trim()) parts.push({ type: "reasoning", text: b.text });
        break;
      case "tool_use":
        parts.push({ type: "tool_call", id: b.id, name: b.name, input: b.input });
        break;
    }
  }
  return parts;
}
