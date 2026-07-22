import { assertEquals, assertRejects } from "jsr:@std/assert@1";
import {
  anthropicSystemBlocks,
  fromResponsesOutput,
  isRetryable,
  joinedSystem,
  type LlmClient,
  LlmError,
  type LlmParams,
  type LlmResult,
  openaiClient,
  openrouterClient,
  providerFor,
  toResponsesInput,
  withRetries,
} from "./llm.ts";

Deno.test("anthropicSystemBlocks: stable then volatile, a 1h breakpoint on each", () => {
  const blocks = anthropicSystemBlocks({ system: "STABLE", systemVolatile: "VOLATILE" })!;
  assertEquals(blocks.length, 2);
  // Order is the cache economics: the volatile block must NEVER precede the
  // stable one — a breakpoint caches everything before it.
  assertEquals(blocks[0].text, "STABLE");
  assertEquals(blocks[1].text, "VOLATILE");
  for (const b of blocks) {
    assertEquals(
      (b as { cache_control?: unknown }).cache_control,
      { type: "ephemeral", ttl: "1h" },
    );
  }
});

Deno.test("anthropicSystemBlocks: empty tiers are skipped; no system → undefined", () => {
  const only = anthropicSystemBlocks({ system: "S" })!;
  assertEquals(only.length, 1);
  assertEquals(only[0].text, "S");
  assertEquals(anthropicSystemBlocks({}), undefined);
  assertEquals(anthropicSystemBlocks({ system: "", systemVolatile: "" }), undefined);
});

Deno.test("joinedSystem: tiers concatenate stable-first for single-field providers", () => {
  assertEquals(joinedSystem({ system: "A", systemVolatile: "B" }), "AB");
  assertEquals(joinedSystem({ system: "A" }), "A");
  assertEquals(joinedSystem({ systemVolatile: "B" }), "B");
  assertEquals(joinedSystem({}), undefined);
});

Deno.test("providerFor: routes by model-id scheme", () => {
  // "openai:" prefix → OpenAI proper.
  assertEquals(providerFor("openai:gpt-5"), "openai");
  assertEquals(providerFor("openai:gpt-5-mini"), "openai");
  // any other "vendor/model" id → OpenRouter (incl. openrouter's own "openai/…").
  assertEquals(providerFor("openai/gpt-5"), "openrouter");
  assertEquals(providerFor("google/gemini-2.5-pro"), "openrouter");
  // bare id → Anthropic.
  assertEquals(providerFor("claude-opus-4-8"), "anthropic");
  assertEquals(providerFor("claude-haiku-4-5"), "anthropic");
});

Deno.test("toResponsesInput: history → items, reasoning echoed before its call", () => {
  const reasoningItem = { type: "reasoning", id: "rs_1", encrypted_content: "…" };
  const items = toResponsesInput([
    { role: "user", content: [{ type: "text", text: "hi" }] },
    {
      role: "assistant",
      content: [
        { type: "reasoning", text: "", meta: reasoningItem },
        { type: "tool_use", id: "call_1", name: "run_steps", input: { code: "1" } },
      ],
    },
    {
      role: "user",
      content: [{ type: "tool_result", toolUseId: "call_1", content: "ok", isError: false }],
    },
  ]);
  assertEquals(items, [
    { role: "user", content: [{ type: "input_text", text: "hi" }] },
    reasoningItem, // verbatim, and BEFORE the function_call it belongs to
    {
      type: "function_call",
      call_id: "call_1",
      name: "run_steps",
      arguments: '{"code":"1"}',
    },
    { type: "function_call_output", call_id: "call_1", output: "ok" },
  ]);
});

Deno.test("toResponsesInput: meta-less reasoning (cross-turn history) is dropped", () => {
  const items = toResponsesInput([
    { role: "assistant", content: [{ type: "reasoning", text: "old thoughts" }] },
  ]);
  assertEquals(items, []);
});

Deno.test("fromResponsesOutput: output items → normalized blocks", () => {
  const blocks = fromResponsesOutput([
    { type: "reasoning", summary: [{ text: "thinking…" }] },
    { type: "function_call", call_id: "call_9", name: "run_steps", arguments: '{"code":"x"}' },
    { type: "message", content: [{ type: "output_text", text: "done" }] },
  ]);
  assertEquals(blocks, [
    {
      type: "reasoning",
      text: "thinking…",
      meta: { type: "reasoning", summary: [{ text: "thinking…" }] },
    },
    { type: "tool_use", id: "call_9", name: "run_steps", input: { code: "x" } },
    { type: "text", text: "done" },
  ]);
});

Deno.test("fromResponsesOutput: malformed arguments degrade to {}", () => {
  const [b] = fromResponsesOutput([
    { type: "function_call", call_id: "c", name: "run_steps", arguments: "{oops" },
  ]);
  assertEquals(b, { type: "tool_use", id: "c", name: "run_steps", input: {} });
});

// ---- retries ----------------------------------------------------------------

const PARAMS: LlmParams = { model: "m", maxTokens: 10, messages: [], tools: [] };
const ok = (text: string): LlmResult => ({
  content: [{ type: "text", text }],
  stopReason: "end_turn",
});

Deno.test("isRetryable: transport and server faults yes; aborts and 4xx no", () => {
  assertEquals(isRetryable(new LlmError("truncated stream")), true); // no status = transport
  assertEquals(isRetryable(new LlmError("rate limited", 429)), true);
  assertEquals(isRetryable(new LlmError("server", 500)), true);
  assertEquals(isRetryable(new LlmError("bad request", 400)), false);
  assertEquals(isRetryable(new LlmError("bad key", 401)), false);
  // The tool-protocol 400 (assistant tool_calls not followed by a tool message)
  // IS retried — toOpenAIMessages self-heals the wire, so a re-send succeeds.
  assertEquals(
    isRetryable(new LlmError("openrouter: 400 assistant message with 'tool_calls' must be followed by tool messages", 400)),
    true,
  );
  assertEquals(isRetryable(new DOMException("gone", "AbortError")), false);
  assertEquals(isRetryable(new TypeError("network error")), true); // fetch failure
  assertEquals(isRetryable(new Error("plain")), false);
  // Anthropic SDK shape: `.status` rides the error object.
  assertEquals(isRetryable(Object.assign(new Error("overloaded"), { status: 529 })), true);
  assertEquals(isRetryable(Object.assign(new Error("invalid"), { status: 400 })), false);
});

Deno.test("withRetries: transient failures retry, then succeed", async () => {
  let calls = 0;
  const retried: number[] = [];
  const inner: LlmClient = {
    run: () => {
      calls++;
      return calls < 3 ? Promise.reject(new LlmError("blip", 500)) : Promise.resolve(ok("hi"));
    },
  };
  const client = withRetries(inner, { baseDelayMs: 1, onRetry: (i) => retried.push(i.attempt) });
  const result = await client.run(PARAMS, () => {});
  assertEquals(result.content, [{ type: "text", text: "hi" }]);
  assertEquals(calls, 3);
  assertEquals(retried, [1, 2]);
});

Deno.test("withRetries: non-retryable error throws immediately", async () => {
  let calls = 0;
  const inner: LlmClient = {
    run: () => {
      calls++;
      return Promise.reject(new LlmError("bad request", 400));
    },
  };
  await assertRejects(
    () => withRetries(inner, { baseDelayMs: 1 }).run(PARAMS, () => {}),
    LlmError,
    "bad request",
  );
  assertEquals(calls, 1);
});

Deno.test("withRetries: gives up after maxAttempts", async () => {
  let calls = 0;
  const inner: LlmClient = {
    run: () => {
      calls++;
      return Promise.reject(new LlmError("down", 503));
    },
  };
  await assertRejects(
    () => withRetries(inner, { baseDelayMs: 1, maxAttempts: 3 }).run(PARAMS, () => {}),
    LlmError,
    "down",
  );
  assertEquals(calls, 3);
});

Deno.test("withRetries: no retry once the signal is aborted", async () => {
  let calls = 0;
  const c = new AbortController();
  const inner: LlmClient = {
    run: () => {
      calls++;
      c.abort();
      return Promise.reject(new LlmError("cut", 500));
    },
  };
  await assertRejects(() => withRetries(inner, { baseDelayMs: 1 }).run(PARAMS, () => {}, c.signal));
  assertEquals(calls, 1);
});

// ---- stream hardening (fetch stubbed; no network) ---------------------------

function sse(...events: string[]): Response {
  return new Response(events.map((e) => `data: ${e}\n\n`).join(""), { status: 200 });
}

async function withFetch<T>(res: Response, fn: () => Promise<T>): Promise<T> {
  const real = globalThis.fetch;
  globalThis.fetch = () => Promise.resolve(res);
  try {
    return await fn();
  } finally {
    globalThis.fetch = real;
  }
}

Deno.test("openrouter: complete stream returns text + assembled tool calls", async () => {
  Deno.env.set("OPENROUTER_API_KEY", "test");
  const res = sse(
    JSON.stringify({ choices: [{ delta: { content: "hel" } }] }),
    JSON.stringify({ choices: [{ delta: { content: "lo" } }] }),
    JSON.stringify({
      choices: [{
        delta: {
          tool_calls: [{ index: 0, id: "c1", function: { name: "bash", arguments: '{"cm' } }],
        },
      }],
    }),
    JSON.stringify({
      choices: [{
        delta: { tool_calls: [{ index: 0, function: { arguments: 'd":"ls"}' } }] },
        finish_reason: "tool_calls",
      }],
    }),
    "[DONE]",
  );
  const deltas: string[] = [];
  const result = await withFetch(res, () => openrouterClient().run(PARAMS, (d) => deltas.push(d)));
  assertEquals(deltas.join(""), "hello");
  assertEquals(result.stopReason, "tool_use");
  assertEquals(result.content, [
    { type: "text", text: "hello" },
    { type: "tool_use", id: "c1", name: "bash", input: { cmd: "ls" } },
  ]);
});

Deno.test("openrouter: a stream cut before [DONE]/finish_reason throws retryable", async () => {
  Deno.env.set("OPENROUTER_API_KEY", "test");
  const res = sse(JSON.stringify({ choices: [{ delta: { content: "partial" } }] }));
  const err = await assertRejects(
    () => withFetch(res, () => openrouterClient().run(PARAMS, () => {})),
    LlmError,
    "truncated",
  );
  assertEquals(isRetryable(err), true);
});

Deno.test("openrouter: mid-stream error chunk throws instead of partial success", async () => {
  Deno.env.set("OPENROUTER_API_KEY", "test");
  const res = sse(
    JSON.stringify({ choices: [{ delta: { content: "par" } }] }),
    JSON.stringify({ error: { message: "Provider returned error", code: 502 } }),
  );
  const err = await assertRejects(
    () => withFetch(res, () => openrouterClient().run(PARAMS, () => {})),
    LlmError,
    "Provider returned error",
  );
  assertEquals(err.status, 502);
  assertEquals(isRetryable(err), true);
});

Deno.test("openrouter: non-2xx response throws with status + retry-after", async () => {
  Deno.env.set("OPENROUTER_API_KEY", "test");
  const res = new Response("slow down", { status: 429, headers: { "retry-after": "2" } });
  const err = await assertRejects(
    () => withFetch(res, () => openrouterClient().run(PARAMS, () => {})),
    LlmError,
  );
  assertEquals(err.status, 429);
  assertEquals(err.retryAfterMs, 2000);
});

Deno.test("openai: stream ending without response.completed throws retryable", async () => {
  Deno.env.set("OPENAI_API_KEY", "test");
  const res = sse(JSON.stringify({ type: "response.output_text.delta", delta: "x" }));
  const err = await assertRejects(
    () => withFetch(res, () => openaiClient().run({ ...PARAMS, model: "openai:gpt-5" }, () => {})),
    LlmError,
    "response.completed",
  );
  assertEquals(isRetryable(err), true);
});

Deno.test("openai: response.failed event throws retryable, rate limits as 429", async () => {
  Deno.env.set("OPENAI_API_KEY", "test");
  const res = sse(
    JSON.stringify({
      type: "response.failed",
      response: { error: { code: "rate_limit_exceeded", message: "slow down" } },
    }),
  );
  const err = await assertRejects(
    () => withFetch(res, () => openaiClient().run({ ...PARAMS, model: "openai:gpt-5" }, () => {})),
    LlmError,
  );
  assertEquals(err.status, 429);
});

// ---- image blocks (all three providers) -------------------------------------

const IMG = {
  type: "image" as const,
  data:
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
  mediaType: "image/png",
  name: "shot.png",
};

Deno.test("toApiMessage: image block → Anthropic base64 image source", async () => {
  const { toApiMessage } = await import("./llm.ts");
  const msg = toApiMessage({
    role: "user",
    content: [{ type: "text", text: "what's wrong here?" }, IMG],
  });
  assertEquals(msg, {
    role: "user",
    content: [
      { type: "text", text: "what's wrong here?" },
      {
        type: "image",
        source: { type: "base64", media_type: "image/png", data: IMG.data },
      },
    ],
  });
});

Deno.test("toResponsesInput: image block → input_image data URL", async () => {
  const { toResponsesInput } = await import("./llm.ts");
  const items = toResponsesInput([
    { role: "user", content: [{ type: "text", text: "see" }, IMG] },
  ]);
  assertEquals(items, [
    { role: "user", content: [{ type: "input_text", text: "see" }] },
    {
      role: "user",
      content: [{
        type: "input_image",
        image_url: `data:image/png;base64,${IMG.data}`,
      }],
    },
  ]);
});

Deno.test("toOpenAIMessages: image → image_url data URL parts; no image keeps string content", async () => {
  const { toOpenAIMessages } = await import("./llm.ts");
  // With an image, user content becomes the multimodal parts array.
  const withImg = toOpenAIMessages(undefined, [
    { role: "user", content: [{ type: "text", text: "see" }, IMG] },
  ]);
  assertEquals(withImg, [{
    role: "user",
    content: [
      { type: "text", text: "see" },
      { type: "image_url", image_url: { url: `data:image/png;base64,${IMG.data}` } },
    ],
  }]);
  // Without images the wire shape is untouched: plain string content.
  const plain = toOpenAIMessages(undefined, [
    { role: "user", content: [{ type: "text", text: "hi" }] },
  ]);
  assertEquals(plain, [{ role: "user", content: "hi" }]);
});

Deno.test("toOpenAIMessages: orphan assistant tool_call gets a synthesized tool message", async () => {
  const { toOpenAIMessages } = await import("./llm.ts");
  // An assistant tool_call whose matching tool result never arrived (e.g. a crash
  // mid-round) would make OpenRouter/Moonshot reject the whole request with a 400.
  const out = toOpenAIMessages(undefined, [
    {
      role: "assistant",
      content: [{ type: "tool_use", id: "call_1", name: "bash", input: { cmd: "ls" } }],
    },
    // no tool_result for call_1 → the encoder must synthesize one
    { role: "user", content: [{ type: "text", text: "next" }] },
  ]) as Array<{ role: string; tool_call_id?: string; content?: unknown }>;
  // The synthesized tool message immediately follows the assistant, before the user.
  assertEquals(out[0].role, "assistant");
  assertEquals(out[1], { role: "tool", tool_call_id: "call_1", content: "(interrupted)" });
  assertEquals(out[2].role, "user");
  // A tool_call WITH its result is left alone (no duplicate synthesized message).
  const paired = toOpenAIMessages(undefined, [
    {
      role: "assistant",
      content: [{ type: "tool_use", id: "call_2", name: "bash", input: {} }],
    },
    {
      role: "user",
      content: [{ type: "tool_result", toolUseId: "call_2", content: "ok", isError: false }],
    },
  ]) as Array<{ role: string }>;
  assertEquals(paired.length, 2);
  assertEquals(paired[1].role, "tool");
});
