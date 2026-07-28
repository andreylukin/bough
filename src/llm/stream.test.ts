/**
 * The streaming plumbing, exercised over in-memory streams — no socket, no clock
 * dependency beyond a stall timeout the tests turn down to milliseconds.
 *
 * The assertions worth having here are the pessimistic ones: a frame split across
 * two chunks still parses, a keepalive comment is not mistaken for data, a stalled
 * reader fails instead of hanging, and a truncated tool call refuses to become
 * `{}`. Each of those, gone wrong, presents as a *successful* round carrying
 * garbage rather than as an error — which is exactly the failure mode a test has
 * to catch, because nothing downstream can.
 *
 * `node:assert` rather than `@std/assert`: jsr.io is denied by this environment's
 * egress policy (same constraint the other test files document).
 */

import { test } from "bun:test";
import { deepStrictEqual, ok, strictEqual } from "node:assert";
import { LlmError } from "../errors.ts";
import type { LlmBlock, LlmToolDef } from "../types.ts";
import { blocksToParts, parseToolArgs, sseEvents, throwHttpError } from "./stream.ts";

/** A body that emits the given chunks, byte-for-byte, in order. */
function bodyOf(chunks: string[]): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const c of chunks) controller.enqueue(enc.encode(c));
      controller.close();
    },
  });
}

async function collect(stream: AsyncIterable<string>): Promise<string[]> {
  const out: string[] = [];
  for await (const p of stream) out.push(p);
  return out;
}

test("sseEvents: yields data payloads and passes [DONE] through untouched", async () => {
  const events = await collect(
    sseEvents(bodyOf(['data: {"a":1}\n', "data: [DONE]\n"]), "test"),
  );
  deepStrictEqual(events, ['{"a":1}', "[DONE]"]);
});

test("sseEvents: a frame split across chunk boundaries still parses", async () => {
  // The transport decides where the packets break; a payload cut mid-JSON must be
  // reassembled, not dropped or half-parsed.
  const events = await collect(
    sseEvents(bodyOf(['data: {"de', 'lta":"hi"}\n', 'data: {"x":2}\n']), "test"),
  );
  deepStrictEqual(events, ['{"delta":"hi"}', '{"x":2}']);
});

test("sseEvents: comments, blank lines and event: lines are skipped", async () => {
  const events = await collect(
    sseEvents(
      bodyOf([": keepalive\n", "\n", "event: ping\n", 'data: {"real":true}\n']),
      "test",
    ),
  );
  deepStrictEqual(events, ['{"real":true}']);
});

test("sseEvents: a trailing un-newlined fragment is dropped, not half-yielded", async () => {
  // It is by definition incomplete. The caller's "did I see a completion marker?"
  // check is what turns this into a retryable transport fault.
  const events = await collect(sseEvents(bodyOf(['data: {"ok":1}\n', 'data: {"cut":']), "test"));
  deepStrictEqual(events, ['{"ok":1}']);
});

test("sseEvents: a stalled stream fails instead of hanging the turn", async () => {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode('data: {"a":1}\n'));
      // and then never again, and never closes
    },
    cancel() {},
  });
  const iter = sseEvents(stream, "openrouter", { stallMs: 10 })[Symbol.asyncIterator]();
  strictEqual((await iter.next()).value, '{"a":1}');
  try {
    await iter.next();
    ok(false, "expected a stall error");
  } catch (err) {
    ok(err instanceof LlmError);
    ok(/stream stalled/.test(err.message), err.message);
    // No status set → defaults to 502 → the retry ring will try again.
    strictEqual(err.status, 502);
  }
});

test("throwHttpError: status, body text and Retry-After all survive", async () => {
  const res = new Response("quota exhausted", {
    status: 429,
    headers: { "retry-after": "7" },
  });
  try {
    await throwHttpError("openrouter", res);
    ok(false, "expected a throw");
  } catch (err) {
    ok(err instanceof LlmError);
    strictEqual(err.status, 429);
    strictEqual(err.retryAfterMs, 7000);
    ok(err.message.includes("openrouter: 429"));
    ok(err.message.includes("quota exhausted"));
  }
});

test("throwHttpError: no Retry-After leaves the hint undefined", async () => {
  try {
    await throwHttpError("openai", new Response("nope", { status: 500 }));
    ok(false, "expected a throw");
  } catch (err) {
    ok(err instanceof LlmError);
    strictEqual(err.retryAfterMs, undefined);
  }
});

// ---- tool-argument truncation ----------------------------------------------

const runSteps: LlmToolDef = {
  name: "run_steps",
  description: "Run one JavaScript program in the workspace.",
  inputSchema: {
    type: "object",
    properties: { code: { type: "string" } },
    required: ["code"],
    additionalProperties: false,
  },
};

const stop: LlmToolDef = {
  name: "stop",
  description: "End the turn.",
  inputSchema: { type: "object", properties: {}, additionalProperties: false },
};

test("parseToolArgs: well-formed arguments decode", () => {
  deepStrictEqual(
    parseToolArgs("openai", '{"code":"console.log(1)"}', runSteps, "run_steps"),
    { code: "console.log(1)" },
  );
});

test("parseToolArgs: the schema decides whether emptiness is legitimate", () => {
  // `stop` requires nothing, so no arguments is a real call.
  deepStrictEqual(parseToolArgs("openai", undefined, stop, "stop"), {});
  // `run_steps` requires `code`, so no arguments means the stream was cut.
  try {
    parseToolArgs("openai", undefined, runSteps, "run_steps");
    ok(false, "expected a truncation error");
  } catch (err) {
    ok(err instanceof LlmError);
    ok(/no arguments \(truncated mid-call\)/.test(err.message), err.message);
  }
});

test("parseToolArgs: an unknown tool with no arguments is not assumed truncated", () => {
  // No schema to judge by — `{}` is the only defensible reading, and the tool
  // dispatcher will report the unknown name properly.
  deepStrictEqual(parseToolArgs("openrouter", undefined, undefined, "mystery"), {});
});

test("parseToolArgs: half a JSON object is a truncation, not a parse bug", () => {
  try {
    parseToolArgs("openrouter", '{"code":"a', runSteps, "run_steps");
    ok(false, "expected a truncation error");
  } catch (err) {
    ok(err instanceof LlmError);
    ok(/malformed arguments \(truncated mid-call\)/.test(err.message), err.message);
    ok(err.message.startsWith("openrouter:"), "the provider must be named");
  }
});

// ---- blocks → parts ---------------------------------------------------------

test("blocksToParts: text, reasoning and tool calls map across in order", () => {
  const blocks: LlmBlock[] = [
    { type: "reasoning", text: "weighing options", meta: { signature: "sig" } },
    { type: "text", text: "here goes" },
    { type: "tool_use", id: "t1", name: "run_steps", input: { code: "1" } },
  ];
  deepStrictEqual(blocksToParts(blocks), [
    { type: "reasoning", text: "weighing options" },
    { type: "text", text: "here goes" },
    { type: "tool_call", id: "t1", name: "run_steps", input: { code: "1" } },
  ]);
});

test("blocksToParts: provider meta never reaches the database", () => {
  // Reasoning is display-only and dropped from cross-turn replay (plan §6.4).
  // Persisting a signature would only invite someone to echo it back.
  const parts = blocksToParts([
    { type: "reasoning", text: "hmm", meta: { type: "thinking", signature: "secret" } },
  ]);
  strictEqual(JSON.stringify(parts).includes("secret"), false);
  deepStrictEqual(parts, [{ type: "reasoning", text: "hmm" }]);
});

test("blocksToParts: empty text and empty reasoning produce no part at all", () => {
  // A redacted thinking block has nothing displayable; persisting it would render
  // as a blank fold in the transcript.
  deepStrictEqual(
    blocksToParts([
      { type: "reasoning", text: "", meta: { type: "redacted_thinking" } },
      { type: "reasoning", text: "   \n " },
      { type: "text", text: "" },
    ]),
    [],
  );
});

test("blocksToParts: a tool call with no input still yields a part", () => {
  // `stop` takes no arguments; the call is the whole message.
  deepStrictEqual(
    blocksToParts([{ type: "tool_use", id: "t9", name: "stop", input: {} }]),
    [{ type: "tool_call", id: "t9", name: "stop", input: {} }],
  );
});
