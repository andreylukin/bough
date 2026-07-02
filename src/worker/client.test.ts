import { assertEquals, assertRejects } from "jsr:@std/assert@1";
import { workerComplete } from "./client.ts";

/** A scripted local endpoint: one canned {status, content} per request, in order. */
function fakeEndpoint(script: { status: number; content?: string }[]) {
  let i = 0;
  const requests: unknown[] = [];
  const server = Deno.serve({ port: 0, onListen: () => {} }, async (req) => {
    requests.push(await req.json());
    const step = script[Math.min(i++, script.length - 1)];
    if (step.status !== 200) return new Response("loading", { status: step.status });
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
  const body = ep.requests[0] as {
    max_tokens: number;
    temperature: number;
    messages: { role: string; content: string }[];
  };
  assertEquals(body.max_tokens, 64);
  assertEquals(body.temperature, 0.2);
  assertEquals(body.messages.map((m) => m.role), ["system", "user"]);
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
