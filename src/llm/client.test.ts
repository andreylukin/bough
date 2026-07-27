/**
 * The provider boundary, proved offline.
 *
 * Two things this file has to establish, and they are the whole acceptance
 * criterion for T2.1:
 *
 *   1. **`LlmClient` is drivable by a fake.** If a scripted in-memory client can
 *      stand in for all three providers everywhere the tree consumes an LLM, then
 *      the turn runner genuinely cannot tell them apart — and every test upstream
 *      of here runs without a key.
 *   2. **All three real encodings are exercised without a socket.** `fetch` and the
 *      env reader are injected, so the OpenAI and OpenRouter paths run against a
 *      stub transport that serves canned SSE, and the Anthropic path is checked
 *      through its pure encoders. Nothing here opens a connection or reads a key.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json`
 * cannot resolve. `node:assert` is built into the runtime and needs no fetch.
 * (Same constraint `hostfn/patch.test.ts` and `bus.test.ts` document.)
 */

import { deepStrictEqual, ok, rejects, strictEqual } from "node:assert";
import { LlmError } from "../errors.ts";
import type { LlmClient, LlmParams, LlmResult, LlmToolDef } from "../types.ts";
import { catalogKeys } from "./pricing.ts";
import {
  anthropicSystemBlocks,
  API_KEY_ENV,
  clientFor,
  completeText,
  discoverOpenAIModels,
  effortParams,
  type Env,
  errName,
  filterOpenAIModels,
  fromResponsesOutput,
  isRetryable,
  isToolProtocol400,
  joinedSystem,
  mergeModels,
  type ModelRow,
  MODELS,
  openaiClient,
  openrouterClient,
  type Provider,
  providerFor,
  toApiMessage,
  toOpenAIMessages,
  toResponsesInput,
  withPricing,
  withRetries,
} from "./client.ts";

// ---- the fake ---------------------------------------------------------------

/**
 * A scripted `LlmClient`: hand it the rounds it should return, in order, and it
 * records what it was asked. This is the shape every upstream test uses — the turn
 * runner, the subagent launcher, the history operations — so if this fake is
 * sufficient, provider knowledge really has stayed inside `llm/`.
 */
function fakeClient(script: (LlmResult | Error)[]) {
  const calls: LlmParams[] = [];
  const client: LlmClient = {
    run(params, onText, signal) {
      calls.push(params);
      if (signal?.aborted) return Promise.reject(new DOMException("aborted", "AbortError"));
      const next = script.shift();
      if (next === undefined) return Promise.reject(new Error("fake: script exhausted"));
      if (next instanceof Error) return Promise.reject(next);
      for (const b of next.content) if (b.type === "text") onText(b.text);
      return Promise.resolve(next);
    },
  };
  return { client, calls };
}

const TOOLS: LlmToolDef[] = [{
  name: "run_steps",
  description: "Run one JavaScript program in the workspace.",
  inputSchema: {
    type: "object",
    properties: { code: { type: "string" }, done: { type: "boolean" } },
    required: ["code"],
    additionalProperties: false,
  },
}, {
  name: "stop",
  description: "End the turn.",
  inputSchema: { type: "object", properties: {}, additionalProperties: false },
}];

const params = (over: Partial<LlmParams> = {}): LlmParams => ({
  model: "claude-opus-5",
  maxTokens: 1024,
  messages: [{ role: "user", content: [{ type: "text", text: "hello" }] }],
  tools: TOOLS,
  ...over,
});

Deno.test("a fake satisfies LlmClient: streamed deltas, blocks and stop reason", async () => {
  const { client, calls } = fakeClient([{
    content: [
      { type: "reasoning", text: "thinking about it" },
      { type: "text", text: "on it" },
      { type: "tool_use", id: "t1", name: "run_steps", input: { code: "1" } },
    ],
    stopReason: "tool_use",
    usage: { inputTokens: 10, outputTokens: 5 },
  }]);

  const deltas: string[] = [];
  const result = await client.run(params(), (d) => deltas.push(d));

  deepStrictEqual(deltas, ["on it"]);
  strictEqual(result.stopReason, "tool_use");
  strictEqual(result.content.length, 3);
  strictEqual(calls.length, 1);
  strictEqual(calls[0].model, "claude-opus-5");
});

Deno.test("completeText drives the interface with no tools and no consumer", async () => {
  const { client, calls } = fakeClient([{
    content: [{ type: "text", text: "a " }, { type: "reasoning", text: "hm" }, {
      type: "text",
      text: "title",
    }],
    stopReason: "end_turn",
  }]);

  const text = await completeText(client, {
    model: "claude-haiku-4-5",
    system: "name it",
    maxTokens: 32,
    prompt: "the transcript",
  });

  strictEqual(text, "a title");
  deepStrictEqual(calls[0].tools, []);
  strictEqual(calls[0].messages.length, 1);
});

// ---- routing ----------------------------------------------------------------

Deno.test("providerFor: openai: prefix, vendor/model, bare id", () => {
  const table: [string, Provider][] = [
    ["claude-opus-5", "anthropic"],
    ["claude-haiku-4-5", "anthropic"],
    ["openai:gpt-5", "openai"],
    ["openai:gpt-5-mini", "openai"],
    // The prefix wins over the slash: "openai:" is OpenAI proper even though the
    // bare id could look routable.
    ["openai:ft/custom-model", "openai"],
    ["openai/gpt-5", "openrouter"],
    ["google/gemini-2.5-pro", "openrouter"],
    ["moonshotai/kimi-k3", "openrouter"],
  ];
  for (const [model, provider] of table) strictEqual(providerFor(model), provider, model);
});

Deno.test("every catalog entry routes to the provider it claims", () => {
  for (const m of MODELS) strictEqual(providerFor(m.id), m.provider, m.id);
});

Deno.test("pricing keys and client routing cannot drift apart", () => {
  // pricing.ts derives its catalog key from the model id independently of
  // client.ts. If the two rules diverge, an entire provider silently stops being
  // priced and every cost quietly becomes null — so pin them together here.
  const expectedPrefix: Record<Provider, string> = {
    anthropic: "anthropic/",
    openai: "openai/",
    openrouter: "openrouter/",
  };
  for (const m of MODELS) {
    const keys = catalogKeys(m.id);
    ok(
      keys[0].startsWith(expectedPrefix[providerFor(m.id)]),
      `${m.id}: catalog key ${keys[0]} does not match provider ${providerFor(m.id)}`,
    );
  }
});

Deno.test("clientFor routes without a key and only fails when asked to run", async () => {
  // Construction must not read a key or touch the network — the server builds a
  // client per model id long before anyone runs a round.
  const env: Env = () => undefined;
  for (const model of ["claude-opus-5", "openai:gpt-5", "openai/gpt-5"]) {
    const client = clientFor(model, { env, retry: { maxAttempts: 2, baseDelayMs: 0 } });
    const err = await client.run(params({ model }), () => {}).then(
      () => null,
      (e: unknown) => e as LlmError,
    );
    ok(err instanceof LlmError, `${model}: expected an LlmError`);
    strictEqual(err.status, 401, `${model}: a missing key must not be retried`);
    ok(
      err.message.includes(API_KEY_ENV[providerFor(model)]),
      `${model}: the message must name the env var, got ${err.message}`,
    );
  }
});

// ---- retries ----------------------------------------------------------------

Deno.test("withRetries: a transient 500 is re-attempted and then succeeds", async () => {
  const good: LlmResult = { content: [{ type: "text", text: "ok" }], stopReason: "end_turn" };
  const { client } = fakeClient([new LlmError("openrouter: 500 upstream", 500), good]);
  const seen: number[] = [];
  const wrapped = withRetries(client, {
    baseDelayMs: 0,
    maxAttempts: 3,
    onRetry: (i) => seen.push(i.attempt),
  });

  const result = await wrapped.run(params(), () => {});
  strictEqual(result.stopReason, "end_turn");
  deepStrictEqual(seen, [1]);
});

Deno.test("withRetries: a 400 is a caller mistake and is never re-attempted", async () => {
  const { client, calls } = fakeClient([new LlmError("openai: 400 bad schema", 400)]);
  const wrapped = withRetries(client, { baseDelayMs: 0, maxAttempts: 4 });
  await rejects(() => wrapped.run(params(), () => {}), /400 bad schema/);
  strictEqual(calls.length, 1);
});

Deno.test("withRetries: exhausting the attempts rethrows the last failure", async () => {
  const boom = () => new LlmError("openrouter: stream truncated before completion");
  const { client, calls } = fakeClient([boom(), boom(), boom()]);
  const wrapped = withRetries(client, { baseDelayMs: 0, maxAttempts: 3 });
  await rejects(() => wrapped.run(params(), () => {}), /truncated before completion/);
  strictEqual(calls.length, 3);
});

Deno.test("withRetries: an aborted signal stops the loop instead of backing off", async () => {
  const ac = new AbortController();
  const client: LlmClient = {
    run() {
      ac.abort();
      return Promise.reject(new LlmError("openrouter: 503", 503));
    },
  };
  const wrapped = withRetries(client, { baseDelayMs: 50_000, maxAttempts: 6 });
  await rejects(() => wrapped.run(params(), () => {}, ac.signal), /503/);
});

Deno.test("isRetryable: transport faults yes, aborts and caller mistakes no", () => {
  ok(isRetryable(new LlmError("transport fault"))); // defaults to 502
  ok(isRetryable(new LlmError("rate limited", 429)));
  ok(isRetryable(new LlmError("slow", 408)));
  ok(isRetryable(new TypeError("error sending request"))); // a fetch network failure
  ok(!isRetryable(new LlmError("bad request", 400)));
  ok(!isRetryable(new LlmError("no key", 401)));
  ok(!isRetryable(new DOMException("aborted", "AbortError")));
  // An SDK error class that never sets `.name` still classifies by status.
  class APIConnectionError extends Error {}
  ok(isRetryable(new APIConnectionError("connection error")));
  strictEqual(errName(new APIConnectionError("x")), "APIConnectionError");
});

Deno.test("isToolProtocol400: the self-healed encoding is the one 400 worth retrying", () => {
  ok(isToolProtocol400(new LlmError("openrouter: 400 tool_call_id not found", 400)));
  ok(isRetryable(new LlmError("openrouter: 400 must be followed by tool messages", 400)));
  ok(!isToolProtocol400(new LlmError("openrouter: 400 model not found", 400)));
});

// ---- pricing ----------------------------------------------------------------

Deno.test("withPricing stamps costUsd from the vendored catalog", async () => {
  const { client } = fakeClient([{
    content: [{ type: "text", text: "hi" }],
    stopReason: "end_turn",
    usage: {
      inputTokens: 1_000_000,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
    },
  }]);
  const result = await withPricing(client).run(params({ model: "claude-opus-5" }), () => {});
  ok(result.usage);
  ok(typeof result.usage.costUsd === "number" && result.usage.costUsd > 0);
});

Deno.test("withPricing leaves an unpriced model at null rather than zero", async () => {
  const { client } = fakeClient([{
    content: [],
    stopReason: "end_turn",
    usage: { inputTokens: 100, outputTokens: 100 },
  }]);
  const result = await withPricing(client).run(
    params({ model: "no-such-vendor/no-such-model" }),
    () => {},
  );
  strictEqual(result.usage?.costUsd, null);
});

// ---- system tiers -----------------------------------------------------------

Deno.test("anthropicSystemBlocks: stable first, a 1h breakpoint on each", () => {
  const blocks = anthropicSystemBlocks({ system: "STABLE", systemVolatile: "VOLATILE" });
  ok(blocks);
  strictEqual(blocks.length, 2);
  strictEqual(blocks[0].text, "STABLE");
  strictEqual(blocks[1].text, "VOLATILE");
  for (const b of blocks) {
    deepStrictEqual(b.cache_control, { type: "ephemeral", ttl: "1h" });
  }
});

Deno.test("anthropicSystemBlocks: undefined when there is no system text", () => {
  strictEqual(anthropicSystemBlocks({}), undefined);
  strictEqual(anthropicSystemBlocks({ system: "" }), undefined);
  strictEqual(anthropicSystemBlocks({ systemVolatile: "only volatile" })?.length, 1);
});

Deno.test("joinedSystem: stable first, undefined when both are empty", () => {
  strictEqual(joinedSystem({ system: "A", systemVolatile: "B" }), "AB");
  strictEqual(joinedSystem({}), undefined);
});

// ---- Anthropic encoding -----------------------------------------------------

Deno.test("toApiMessage: a thinking block replays verbatim, signature included", () => {
  const raw = { type: "thinking", thinking: "step one", signature: "sig-abc" };
  const msg = toApiMessage({
    role: "assistant",
    content: [
      { type: "reasoning", text: "step one", meta: raw },
      { type: "tool_use", id: "t1", name: "run_steps", input: { code: "x" } },
    ],
  });
  const content = msg.content as unknown as Record<string, unknown>[];
  deepStrictEqual(content[0], raw);
  strictEqual(content[1].type, "tool_use");
});

Deno.test("toApiMessage: foreign reasoning degrades to prose, empty reasoning vanishes", () => {
  const withText = toApiMessage({
    role: "assistant",
    content: [{ type: "reasoning", text: "a summary", meta: { type: "reasoning" } }],
  });
  deepStrictEqual(withText.content, [{ type: "text", text: "a summary" }]);

  // A summary-less item would become an empty text block, which the API rejects.
  const empty = toApiMessage({
    role: "assistant",
    content: [{ type: "reasoning", text: "   ", meta: { type: "reasoning" } }],
  });
  deepStrictEqual(empty.content, []);
});

Deno.test("toApiMessage: tool results and images take their native shapes", () => {
  const msg = toApiMessage({
    role: "user",
    content: [
      { type: "tool_result", toolUseId: "t1", content: "out", isError: false },
      { type: "image", data: "AAAA", mediaType: "image/png", name: "shot.png" },
    ],
  });
  const content = msg.content as unknown as Record<string, unknown>[];
  deepStrictEqual(content[0], {
    type: "tool_result",
    tool_use_id: "t1",
    content: "out",
    is_error: false,
  });
  deepStrictEqual(content[1], {
    type: "image",
    source: { type: "base64", media_type: "image/png", data: "AAAA" },
  });
});

Deno.test("effortParams: only sent to models that accept adaptive thinking", () => {
  deepStrictEqual(effortParams("high", "claude-opus-5"), {
    thinking: { type: "adaptive", display: "summarized" },
    output_config: { effort: "high" },
  });
  ok("thinking" in effortParams("low", "claude-opus-4-8"));
  // Haiku 4.5 hard-400s on the param: an effort setting must not kill the turn.
  deepStrictEqual(effortParams("high", "claude-haiku-4-5"), {});
  // No effort at all leaves the request shape untouched.
  deepStrictEqual(effortParams(undefined, "claude-opus-5"), {});
});

// ---- a stub transport -------------------------------------------------------

/** Build an SSE `Response` from already-framed `data:` payloads. */
function sse(payloads: string[], init: ResponseInit = {}): Response {
  const body = payloads.map((p) => `data: ${p}\n`).join("");
  return new Response(body, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
    ...init,
  });
}

/** A `fetch` that answers from a queue and records every request body. */
function stubFetch(responses: Response[]) {
  const requests: { url: string; body: unknown; headers: Headers }[] = [];
  const f = ((input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    requests.push({
      url,
      body: init?.body ? JSON.parse(init.body as string) : undefined,
      headers: new Headers(init?.headers),
    });
    const next = responses.shift();
    if (!next) return Promise.reject(new Error("stubFetch: no response queued"));
    return Promise.resolve(next);
  }) as unknown as typeof fetch;
  return { fetch: f, requests };
}

const keyed: Env = (k) => (k.endsWith("_API_KEY") ? "test-key" : undefined);

// ---- OpenAI (Responses API) -------------------------------------------------

Deno.test("toResponsesInput: reasoning items ride through verbatim before their call", () => {
  const reasoning = { type: "reasoning", id: "rs_1", encrypted_content: "enc" };
  const input = toResponsesInput([
    { role: "user", content: [{ type: "text", text: "go" }] },
    {
      role: "assistant",
      content: [
        { type: "reasoning", text: "", meta: reasoning },
        { type: "tool_use", id: "call_1", name: "run_steps", input: { code: "1" } },
      ],
    },
    {
      role: "user",
      content: [{ type: "tool_result", toolUseId: "call_1", content: "1", isError: false }],
    },
  ]) as Record<string, unknown>[];

  deepStrictEqual(input[0], { role: "user", content: [{ type: "input_text", text: "go" }] });
  deepStrictEqual(input[1], reasoning);
  deepStrictEqual(input[2], {
    type: "function_call",
    call_id: "call_1",
    name: "run_steps",
    arguments: JSON.stringify({ code: "1" }),
  });
  deepStrictEqual(input[3], { type: "function_call_output", call_id: "call_1", output: "1" });
});

Deno.test("toResponsesInput: reasoning with no meta is dropped, not sent bare", () => {
  const input = toResponsesInput([
    { role: "assistant", content: [{ type: "reasoning", text: "prose only" }] },
  ]);
  deepStrictEqual(input, []);
});

Deno.test("fromResponsesOutput: a call missing required arguments is a truncation", () => {
  // The tool requires `code`, so an argument-less call was cut off mid-stream.
  // Inventing `{}` here would run the wrong program.
  try {
    fromResponsesOutput([{ type: "function_call", call_id: "c1", name: "run_steps" }], TOOLS);
    ok(false, "expected a truncation error");
  } catch (err) {
    ok(err instanceof LlmError);
    ok(/truncated mid-call/.test(err.message), err.message);
  }
});

Deno.test("fromResponsesOutput: an argument-less call is fine when nothing is required", () => {
  const blocks = fromResponsesOutput(
    [{ type: "function_call", call_id: "c1", name: "stop" }],
    TOOLS,
  );
  deepStrictEqual(blocks, [{ type: "tool_use", id: "c1", name: "stop", input: {} }]);
});

Deno.test("fromResponsesOutput: malformed argument JSON is a truncation too", () => {
  try {
    fromResponsesOutput(
      [{ type: "function_call", call_id: "c1", name: "run_steps", arguments: '{"code":"a' }],
      TOOLS,
    );
    ok(false, "expected a truncation error");
  } catch (err) {
    ok(err instanceof LlmError);
    ok(/malformed arguments/.test(err.message), err.message);
  }
});

Deno.test("openai: a full round over a stub transport strips the prefix and normalizes usage", async () => {
  const { fetch: f, requests } = stubFetch([
    sse([
      JSON.stringify({ type: "response.output_text.delta", delta: "wor" }),
      JSON.stringify({ type: "response.output_text.delta", delta: "king" }),
      JSON.stringify({
        type: "response.completed",
        response: {
          status: "completed",
          output: [
            { type: "reasoning", summary: [{ text: "plan" }], id: "rs_1" },
            { type: "message", content: [{ type: "output_text", text: "working" }] },
            {
              type: "function_call",
              call_id: "call_9",
              name: "run_steps",
              arguments: '{"code":"1"}',
            },
          ],
          usage: {
            input_tokens: 100,
            output_tokens: 20,
            input_tokens_details: { cached_tokens: 40 },
            output_tokens_details: { reasoning_tokens: 7 },
          },
        },
      }),
      "[DONE]",
    ]),
  ]);

  const deltas: string[] = [];
  const result = await openaiClient({ env: keyed, fetch: f }).run(
    params({ model: "openai:gpt-5", system: "S", systemVolatile: "V", effort: "max" }),
    (d) => deltas.push(d),
  );

  const req = requests[0].body as Record<string, unknown>;
  strictEqual(requests[0].url, "https://api.openai.com/v1/responses");
  strictEqual(req.model, "gpt-5", "the openai: prefix is a routing token, not a model name");
  strictEqual(req.instructions, "SV");
  strictEqual(req.store, false);
  // The Responses API caps reasoning effort at "high".
  deepStrictEqual(req.reasoning, { effort: "high" });

  deepStrictEqual(deltas, ["wor", "king"]);
  strictEqual(result.stopReason, "tool_use");
  deepStrictEqual(result.content[0], {
    type: "reasoning",
    text: "plan",
    meta: { type: "reasoning", summary: [{ text: "plan" }], id: "rs_1" },
  });
  deepStrictEqual(result.content[1], { type: "text", text: "working" });
  deepStrictEqual(result.usage, {
    inputTokens: 100,
    outputTokens: 20,
    reasoningTokens: 7,
    cacheReadTokens: 40,
    cacheWriteTokens: 0,
  });
});

Deno.test("openai: max_output_tokens shows up as max_tokens, not as a finished turn", async () => {
  const { fetch: f } = stubFetch([
    sse([
      JSON.stringify({
        type: "response.incomplete",
        response: {
          status: "incomplete",
          incomplete_details: { reason: "max_output_tokens" },
          output: [{ type: "message", content: [{ type: "output_text", text: "half" }] }],
        },
      }),
      "[DONE]",
    ]),
  ]);
  const result = await openaiClient({ env: keyed, fetch: f }).run(
    params({ model: "openai:gpt-5" }),
    () => {},
  );
  strictEqual(result.stopReason, "max_tokens");
});

Deno.test("openai: a stream that ends without response.completed is a transport fault", async () => {
  const { fetch: f } = stubFetch([
    sse([JSON.stringify({ type: "response.output_text.delta", delta: "half a th" })]),
  ]);
  const err = await openaiClient({ env: keyed, fetch: f })
    .run(params({ model: "openai:gpt-5" }), () => {})
    .then(() => null, (e: unknown) => e as LlmError);
  ok(err instanceof LlmError);
  ok(/stream ended without response.completed/.test(err.message), err.message);
  ok(isRetryable(err), "a cut stream must be retryable");
});

Deno.test("openai: a non-2xx carries its status and Retry-After into the error", async () => {
  const { fetch: f } = stubFetch([
    new Response("slow down", { status: 429, headers: { "retry-after": "3" } }),
  ]);
  const err = await openaiClient({ env: keyed, fetch: f })
    .run(params({ model: "openai:gpt-5" }), () => {})
    .then(() => null, (e: unknown) => e as LlmError);
  ok(err instanceof LlmError);
  strictEqual(err.status, 429);
  strictEqual(err.retryAfterMs, 3000);
  ok(isRetryable(err));
});

Deno.test("openai: a mid-stream failure event is classified, not swallowed", async () => {
  const { fetch: f } = stubFetch([
    sse([
      JSON.stringify({
        type: "response.failed",
        response: { error: { code: "rate_limit_exceeded", message: "slow down" } },
      }),
    ]),
  ]);
  const err = await openaiClient({ env: keyed, fetch: f })
    .run(params({ model: "openai:gpt-5" }), () => {})
    .then(() => null, (e: unknown) => e as LlmError);
  ok(err instanceof LlmError);
  strictEqual(err.status, 429);
});

// ---- OpenRouter (chat completions) ------------------------------------------

Deno.test("toOpenAIMessages: an orphaned tool_call is repaired, not left to 400", () => {
  const msgs = toOpenAIMessages(undefined, [
    {
      role: "assistant",
      content: [
        { type: "text", text: "running" },
        { type: "tool_use", id: "c1", name: "run_steps", input: { code: "1" } },
      ],
    },
    { role: "user", content: [{ type: "text", text: "actually, stop" }] },
  ]) as Record<string, unknown>[];

  strictEqual(msgs.length, 3);
  strictEqual(msgs[0].role, "assistant");
  deepStrictEqual(msgs[1], { role: "tool", tool_call_id: "c1", content: "(interrupted)" });
  strictEqual(msgs[2].role, "user");
});

Deno.test("toOpenAIMessages: a satisfied tool_call is left exactly as it was", () => {
  const msgs = toOpenAIMessages("SYS", [
    {
      role: "assistant",
      content: [{ type: "tool_use", id: "c1", name: "stop", input: {} }],
    },
    {
      role: "user",
      content: [{ type: "tool_result", toolUseId: "c1", content: "done", isError: false }],
    },
  ]) as Record<string, unknown>[];

  deepStrictEqual(msgs[0], { role: "system", content: "SYS" });
  strictEqual(msgs.length, 3, "no synthesized result should have been added");
  deepStrictEqual(msgs[2], { role: "tool", tool_call_id: "c1", content: "done" });
});

Deno.test("toOpenAIMessages: images become multimodal parts, text alone stays a string", () => {
  const plain = toOpenAIMessages(undefined, [
    { role: "user", content: [{ type: "text", text: "hi" }] },
  ]) as Record<string, unknown>[];
  strictEqual(plain[0].content, "hi");

  const withImage = toOpenAIMessages(undefined, [{
    role: "user",
    content: [
      { type: "text", text: "look" },
      { type: "image", data: "AAAA", mediaType: "image/png", name: "s.png" },
    ],
  }]) as Record<string, unknown>[];
  deepStrictEqual(withImage[0].content, [
    { type: "text", text: "look" },
    { type: "image_url", image_url: { url: "data:image/png;base64,AAAA" } },
  ]);
});

Deno.test("openrouter: a full round assembles streamed tool-call fragments in order", async () => {
  const chunk = (delta: unknown, finish?: string) =>
    JSON.stringify({ choices: [{ delta, ...(finish ? { finish_reason: finish } : {}) }] });
  const { fetch: f, requests } = stubFetch([
    sse([
      chunk({ content: "one moment" }),
      chunk({ tool_calls: [{ index: 0, id: "c1", function: { name: "run_steps" } }] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: '{"co' } }] }),
      chunk({ tool_calls: [{ index: 0, function: { arguments: 'de":"1"}' } }] }),
      chunk({}, "tool_calls"),
      JSON.stringify({
        choices: [],
        usage: {
          prompt_tokens: 200,
          completion_tokens: 30,
          prompt_tokens_details: { cached_tokens: 50 },
          completion_tokens_details: { reasoning_tokens: 3 },
        },
      }),
      "[DONE]",
    ]),
  ]);

  const deltas: string[] = [];
  const result = await openrouterClient({ env: keyed, fetch: f }).run(
    params({ model: "google/gemini-2.5-pro" }),
    (d) => deltas.push(d),
  );

  strictEqual(requests[0].url, "https://openrouter.ai/api/v1/chat/completions");
  strictEqual((requests[0].body as Record<string, unknown>).model, "google/gemini-2.5-pro");
  strictEqual(requests[0].headers.get("x-title"), "bough");
  deepStrictEqual(deltas, ["one moment"]);
  strictEqual(result.stopReason, "tool_use");
  deepStrictEqual(result.content, [
    { type: "text", text: "one moment" },
    { type: "tool_use", id: "c1", name: "run_steps", input: { code: "1" } },
  ]);
  deepStrictEqual(result.usage, {
    inputTokens: 200,
    outputTokens: 30,
    reasoningTokens: 3,
    cacheReadTokens: 50,
    cacheWriteTokens: 0,
  });
});

Deno.test("openrouter: a stream that closes without a finish_reason is a transport fault", async () => {
  const { fetch: f } = stubFetch([
    sse([JSON.stringify({ choices: [{ delta: { content: "partial" } }] })]),
  ]);
  const err = await openrouterClient({ env: keyed, fetch: f })
    .run(params({ model: "z-ai/glm-5.2" }), () => {})
    .then(() => null, (e: unknown) => e as LlmError);
  ok(err instanceof LlmError);
  ok(/truncated before completion/.test(err.message), err.message);
  ok(isRetryable(err));
});

Deno.test("openrouter: a terminal error chunk on a 200 stream is not passed off as success", async () => {
  const { fetch: f } = stubFetch([
    sse([
      JSON.stringify({ choices: [{ delta: { content: "start" } }] }),
      JSON.stringify({ error: { message: "upstream is down", code: 502 } }),
    ]),
  ]);
  const err = await openrouterClient({ env: keyed, fetch: f })
    .run(params({ model: "z-ai/glm-5.2" }), () => {})
    .then(() => null, (e: unknown) => e as LlmError);
  ok(err instanceof LlmError);
  strictEqual(err.status, 502);
  ok(/upstream is down/.test(err.message));
});

Deno.test("openrouter: a truncated tool call is retried rather than run with {}", async () => {
  const { fetch: f } = stubFetch([
    sse([
      JSON.stringify({
        choices: [{
          delta: { tool_calls: [{ index: 0, id: "c1", function: { name: "run_steps" } }] },
          finish_reason: "tool_calls",
        }],
      }),
      "[DONE]",
    ]),
  ]);
  const err = await openrouterClient({ env: keyed, fetch: f })
    .run(params({ model: "z-ai/glm-5.2" }), () => {})
    .then(() => null, (e: unknown) => e as LlmError);
  ok(err instanceof LlmError);
  ok(/no arguments \(truncated mid-call\)/.test(err.message), err.message);
  ok(isRetryable(err));
});

Deno.test("openrouter: finish_reason length normalizes to max_tokens", async () => {
  const { fetch: f } = stubFetch([
    sse([
      JSON.stringify({ choices: [{ delta: { content: "cut" }, finish_reason: "length" }] }),
      "[DONE]",
    ]),
  ]);
  const result = await openrouterClient({ env: keyed, fetch: f })
    .run(params({ model: "z-ai/glm-5.2" }), () => {});
  strictEqual(result.stopReason, "max_tokens");
});

// ---- the model catalog ------------------------------------------------------

Deno.test("filterOpenAIModels: chat ids only, dated snapshots dropped, newest first", () => {
  const rows = filterOpenAIModels([
    "gpt-5",
    "gpt-5-2026-01-01",
    "gpt-4o-audio-preview",
    "text-embedding-3-large",
    "o3",
    "dall-e-3",
    "chatgpt-4o-latest",
    "gpt-3.5-turbo-instruct",
  ]);
  deepStrictEqual(rows.map((r) => r.id), ["openai:o3", "openai:gpt-5", "openai:chatgpt-4o-latest"]);
  for (const r of rows) strictEqual(providerFor(r.id), "openai");
});

Deno.test("mergeModels: the static table wins on id collisions", () => {
  const dynamic: ModelRow[] = [
    { id: "openai:gpt-5", label: "gpt-5 (OpenAI)", provider: "openai" },
    { id: "openai:o4", label: "o4 (OpenAI)", provider: "openai" },
  ];
  const merged = mergeModels(MODELS, dynamic);
  strictEqual(merged.filter((m) => m.id === "openai:gpt-5").length, 1);
  strictEqual(merged.at(-1)?.id, "openai:o4");
});

Deno.test("discoverOpenAIModels: no key and a failing request both yield an empty list", async () => {
  deepStrictEqual(await discoverOpenAIModels({ env: () => undefined }), []);

  const { fetch: failing } = stubFetch([new Response("nope", { status: 401 })]);
  deepStrictEqual(await discoverOpenAIModels({ env: keyed, fetch: failing }), []);

  const throwing = (() => Promise.reject(new TypeError("offline"))) as unknown as typeof fetch;
  deepStrictEqual(await discoverOpenAIModels({ env: keyed, fetch: throwing }), []);
});

Deno.test("discoverOpenAIModels: a good response maps into picker rows", async () => {
  const { fetch: f, requests } = stubFetch([
    new Response(JSON.stringify({ data: [{ id: "gpt-5" }, { id: 42 }, { id: "whisper-1" }] })),
  ]);
  const rows = await discoverOpenAIModels({ env: keyed, fetch: f });
  deepStrictEqual(rows, [{ id: "openai:gpt-5", label: "gpt-5 (OpenAI)", provider: "openai" }]);
  strictEqual(requests[0].url, "https://api.openai.com/v1/models");
});
