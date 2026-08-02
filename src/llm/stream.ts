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
 * **3. Reasoning is persisted WITH its provider payload.** `blocksToParts` keeps
 * the `reasoning` text so the UI can fold it open, and keeps the opaque `meta`
 * that rode along with it, stamped with the model that produced it. That payload
 * is what makes the block replayable — providers hand back a thinking block whole
 * or not at all, and they reject one whose content was altered rather than one
 * that was merely read. `turn/replay.ts` decides when it goes back out; storing it
 * is what gives that decision anything to work with.
 *
 * Nothing here knows a provider by name — the provider string is a label for error
 * text, and `meta` is never opened, only carried. That is what lets all three
 * clients in `client.ts` share this file, and it is why the replay rule needs no
 * per-provider branch either: each mapper opens its own payload, and no one else's.
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
  // Whatever shape the runtime's `read()` resolves to — Bun's `done: true` result
  // leaves `value` optional where the DOM lib requires it, and spelling the type
  // out by hand picks the wrong one.
): ReturnType<ReadableStreamDefaultReader<Uint8Array>["read"]> {
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
 * `model` stamps the reasoning parts, because a provider signature is only valid
 * for the model that produced it and replay is gated on that (`turn/replay.ts`).
 * Callers that have no model to stamp get display-only reasoning, which is what
 * the old behaviour was for everyone.
 *
 * A reasoning block with NO displayable text is still persisted when it carries
 * `meta` — that is a redacted thinking block, and the provider's rule is that a
 * block comes back exactly as it was received or not at all. The UI skips the
 * empty fold rather than rendering a mysterious blank row.
 */
export function blocksToParts(blocks: LlmBlock[], model?: string): Part[] {
  const parts: Part[] = [];
  for (const b of blocks) {
    switch (b.type) {
      case "text":
        if (b.text) parts.push({ type: "text", text: b.text });
        break;
      case "reasoning":
        if (b.text.trim() || b.meta !== undefined) {
          parts.push({ type: "reasoning", text: b.text, meta: b.meta, model });
        }
        break;
      case "tool_use":
        parts.push({ type: "tool_call", id: b.id, name: b.name, input: b.input });
        break;
    }
  }
  return parts;
}
