import { assertEquals, assertRejects } from "jsr:@std/assert@1";
import type { LlmClient, LlmParams, LlmResult } from "./supervisor/llm.ts";
import { normalizeSections, parseSections, sectionize } from "./sections.ts";

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

Deno.test("parseSections tolerates fences and prose around the JSON", () => {
  const raw = 'Here you go:\n```json\n[{"start":0,"end":1,"label":"auth refresh race"}]\n```';
  assertEquals(parseSections(raw), [{ start: 0, end: 1, label: "auth refresh race" }]);
  assertEquals(parseSections("no json here"), null);
  assertEquals(parseSections('[{"start":"x","end":1,"label":"bad types"}]'), null);
});

Deno.test("normalizeSections clips, fills gaps, and drops overlaps", () => {
  const out = normalizeSections(
    [
      { start: 4, end: 9, label: "shipping the feature" }, // clipped to n-1
      { start: 0, end: 1, label: "auth refresh race" },
      { start: 1, end: 2, label: "overlap trimmed" },
      // gap at 3 gets filled
    ],
    6,
  );
  assertEquals(out, [
    { start: 0, end: 1, label: "auth refresh race" },
    { start: 2, end: 2, label: "overlap trimmed" },
    { start: 3, end: 3, label: "…" },
    { start: 4, end: 5, label: "shipping the feature" },
  ]);
});

Deno.test("normalizeSections covers everything when the model returns nothing usable", () => {
  assertEquals(normalizeSections([], 3), [{ start: 0, end: 2, label: "…" }]);
});

Deno.test("sectionize prompts with numbered gists and normalizes the reply", async () => {
  const llm = fakeLlm(['[{"start":0,"end":0,"label":"dependency setup"}]']);
  const out = await sectionize({ llm }, [{ gist: "install deps" }, { gist: "fix the test" }]);
  assertEquals(out, [
    { start: 0, end: 0, label: "dependency setup" },
    { start: 1, end: 1, label: "…" },
  ]);
  const prompt = (llm.calls[0].messages[0].content[0] as { text: string }).text;
  assertEquals(prompt, "0. install deps\n1. fix the test");
});

Deno.test("sectionize rejects unparseable model output with a 502", async () => {
  const llm = fakeLlm(["sorry, I cannot"]);
  await assertRejects(
    () => sectionize({ llm }, [{ gist: "x" }]),
    Error,
    "section labeling failed",
  );
});
