import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import type { LlmClient, LlmParams, LlmResult } from "../supervisor/llm.ts";
import { defaultConfig } from "./config.ts";
import { suggestPolicy } from "./suggest.ts";

function fakeLlm(script: string[]): LlmClient & { calls: LlmParams[] } {
  let i = 0;
  const calls: LlmParams[] = [];
  return {
    calls,
    run(params: LlmParams): Promise<LlmResult> {
      calls.push(params);
      return Promise.resolve({
        content: [{ type: "text", text: script[Math.min(i++, script.length - 1)] }],
        stopReason: "end_turn",
      });
    },
  };
}

const good = JSON.stringify({
  config: { ...defaultConfig(), allowHosts: ["api.exa.ai"], holdVerbs: ["graphql:mutation"] },
  rationale: "exa reads only",
});

Deno.test("suggestPolicy: parses a clean JSON answer and passes context to the model", async () => {
  const llm = fakeLlm([good]);
  const s = await suggestPolicy({
    llm,
    model: "m",
    intent: "research chocolate shops with exa",
    base: defaultConfig(),
    recent: [{
      id: "r1",
      host: "api.exa.ai",
      action: "POST /search",
      verdict: "pending",
      ts: 1,
    }],
  });
  assertEquals(s.config.allowHosts, ["api.exa.ai"]);
  assertEquals(s.config.holdVerbs, ["graphql:mutation"]);

  const sent = (llm.calls[0].messages[0].content[0] as { text: string }).text;
  assertStringIncludes(sent, "chocolate shops");
  assertStringIncludes(sent, "PENDING"); // the live hold rides along as context
  assertStringIncludes(sent, "api.exa.ai");
  assertStringIncludes(sent, "BASE CONFIG");
});

Deno.test("suggestPolicy: unwraps markdown fences", async () => {
  const llm = fakeLlm(["```json\n" + good + "\n```"]);
  const s = await suggestPolicy({ llm, model: "m", intent: "x", base: defaultConfig() });
  assertEquals(s.rationale, "exa reads only");
});

Deno.test("suggestPolicy: retries once with the parse error, then succeeds", async () => {
  const llm = fakeLlm(["Sure! Here's my plan:", good]);
  const s = await suggestPolicy({ llm, model: "m", intent: "x", base: defaultConfig() });
  assertEquals(llm.calls.length, 2);
  assertStringIncludes(
    (llm.calls[1].messages[0].content[0] as { text: string }).text,
    "failed to parse",
  );
  assertEquals(s.config.allowHosts, ["api.exa.ai"]);
});

Deno.test("suggestPolicy: two bad answers reject", async () => {
  const llm = fakeLlm(["nope", "still nope"]);
  await assertRejects(
    () => suggestPolicy({ llm, model: "m", intent: "x", base: defaultConfig() }),
    Error,
    "no valid suggestion",
  );
});

Deno.test("suggestPolicy: selected requests get their own must-cover section", async () => {
  const llm = fakeLlm([good]);
  await suggestPolicy({
    llm,
    model: "m",
    intent: "group these",
    base: defaultConfig(),
    selected: [
      { id: "a", host: "registry.npmjs.org", action: "GET /react", verdict: "allowed", ts: 1 },
      { id: "b", host: "api.exa.ai", action: "POST /search", verdict: "allowed", ts: 2 },
    ],
    recent: [{ id: "c", host: "evil.example.com", action: "GET /", verdict: "denied", ts: 3 }],
  });
  const sent = (llm.calls[0].messages[0].content[0] as { text: string }).text;
  assertStringIncludes(sent, "SELECTED REQUESTS");
  assertStringIncludes(sent, "registry.npmjs.org");
  // ambient recent traffic still rides along, in its own section after selected
  assertEquals(sent.indexOf("SELECTED REQUESTS") < sent.indexOf("RECENT REQUESTS"), true);
});
