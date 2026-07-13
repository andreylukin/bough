import { assertEquals } from "jsr:@std/assert@1";
import { fromResponsesOutput, providerFor, toResponsesInput } from "./llm.ts";

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
