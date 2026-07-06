import { assertEquals, assertRejects } from "jsr:@std/assert@1";
import { workerComplete, workerCompleteMeta, workerEmbed, workerInfill } from "./client.ts";

/** A scripted local endpoint: one canned {status, body} per request, in order. */
function fakeEndpoint(script: { status: number; body?: unknown; content?: string }[]) {
  let i = 0;
  const requests: { path: string; body: unknown }[] = [];
  const server = Deno.serve({ port: 0, onListen: () => {} }, async (req) => {
    requests.push({ path: new URL(req.url).pathname, body: await req.json() });
    const step = script[Math.min(i++, script.length - 1)];
    if (step.status !== 200) return new Response("loading", { status: step.status });
    if (step.body !== undefined) return Response.json(step.body);
    return Response.json({ choices: [{ message: { content: step.content ?? "" } }] });
  });
  const url = `http://127.0.0.1:${server.addr.port}`;
  return { url, requests, close: () => server.shutdown() };
}

Deno.test("workerComplete sends the OpenAI shape and returns the text", async () => {
  const ep = fakeEndpoint([{ status: 200, content: "a fine title" }]);
  const out = await workerComplete(ep.url, {
    system: "sys",
    user: "usr",
    maxTokens: 64,
    temperature: 0.2,
  });
  assertEquals(out, "a fine title");
  const body = ep.requests[0].body as {
    max_tokens: number;
    temperature: number;
    messages: { role: string; content: string }[];
    logprobs?: boolean;
    response_format?: unknown;
  };
  assertEquals(body.max_tokens, 64);
  assertEquals(body.temperature, 0.2);
  assertEquals(body.messages.map((m) => m.role), ["system", "user"]);
  assertEquals(body.logprobs, undefined);
  assertEquals(body.response_format, undefined);
  await ep.close();
});

Deno.test("503 while the model loads is retried, not fatal", async () => {
  const ep = fakeEndpoint([{ status: 503 }, { status: 503 }, { status: 200, content: "ok" }]);
  const out = await workerComplete(ep.url, { system: "s", user: "u", maxTokens: 8 });
  assertEquals(out, "ok");
  assertEquals(ep.requests.length, 3);
  await ep.close();
});

Deno.test("non-transient error status surfaces after exhausting nothing", async () => {
  const ep = fakeEndpoint([{ status: 400 }]);
  await assertRejects(
    () => workerComplete(ep.url, { system: "s", user: "u", maxTokens: 8 }),
    Error,
    "worker 400",
  );
  assertEquals(ep.requests.length, 1);
  await ep.close();
});

Deno.test("jsonSchema and cachePrompt ride along as llama-server expects", async () => {
  const ep = fakeEndpoint([{ status: 200, content: '{"kind":"write"}' }]);
  const schema = { type: "object", properties: { kind: { type: "string" } } };
  const out = await workerComplete(ep.url, {
    system: "s",
    user: "u",
    maxTokens: 32,
    jsonSchema: schema,
    cachePrompt: true,
  });
  assertEquals(out, '{"kind":"write"}');
  const body = ep.requests[0].body as {
    response_format: { type: string; json_schema: { schema: unknown } };
    cache_prompt: boolean;
  };
  assertEquals(body.response_format.type, "json_schema");
  assertEquals(body.response_format.json_schema.schema, schema);
  assertEquals(body.cache_prompt, true);
  await ep.close();
});

Deno.test("workerCompleteMeta requests logprobs and averages them", async () => {
  const ep = fakeEndpoint([{
    status: 200,
    body: {
      choices: [{
        message: { content: "hi" },
        logprobs: { content: [{ logprob: -0.5 }, { logprob: -1.5 }] },
      }],
    },
  }]);
  const out = await workerCompleteMeta(ep.url, { system: "s", user: "u", maxTokens: 8 });
  assertEquals(out.text, "hi");
  assertEquals(out.avgLogprob, -1.0);
  assertEquals((ep.requests[0].body as { logprobs: boolean }).logprobs, true);
  await ep.close();
});

Deno.test("workerCompleteMeta without server logprobs still returns the text", async () => {
  const ep = fakeEndpoint([{ status: 200, content: "hi" }]);
  const out = await workerCompleteMeta(ep.url, { system: "s", user: "u", maxTokens: 8 });
  assertEquals(out.text, "hi");
  assertEquals(out.avgLogprob, undefined);
  await ep.close();
});

Deno.test("workerInfill hits /infill with prefix/suffix and returns the fill", async () => {
  const ep = fakeEndpoint([{ status: 200, body: { content: "return a + b" } }]);
  const out = await workerInfill(ep.url, {
    prefix: "def add(a, b):\n    ",
    suffix: "\n",
    maxTokens: 32,
  });
  assertEquals(out, "return a + b");
  assertEquals(ep.requests[0].path, "/infill");
  const body = ep.requests[0].body as { input_prefix: string; input_suffix: string };
  assertEquals(body.input_prefix, "def add(a, b):\n    ");
  assertEquals(body.input_suffix, "\n");
  await ep.close();
});

Deno.test("workerEmbed returns vectors in input order", async () => {
  const ep = fakeEndpoint([{
    status: 200,
    body: {
      data: [
        { index: 1, embedding: [0.3, 0.4] },
        { index: 0, embedding: [0.1, 0.2] },
      ],
    },
  }]);
  const out = await workerEmbed(ep.url, ["first", "second"]);
  assertEquals(out, [[0.1, 0.2], [0.3, 0.4]]);
  assertEquals(ep.requests[0].path, "/v1/embeddings");
  await ep.close();
});

Deno.test("workerEmbed rejects a count mismatch", async () => {
  const ep = fakeEndpoint([{ status: 200, body: { data: [{ index: 0, embedding: [0.1] }] } }]);
  await assertRejects(() => workerEmbed(ep.url, ["a", "b"]), Error, "1 vectors for 2 inputs");
  await ep.close();
});
