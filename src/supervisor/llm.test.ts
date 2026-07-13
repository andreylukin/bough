import { assertEquals } from "jsr:@std/assert@1";
import { providerFor } from "./llm.ts";

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
