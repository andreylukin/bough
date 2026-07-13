import { assertEquals } from "jsr:@std/assert@1";
import {
  filterOpenAIModels,
  mergeModels,
  openaiModels,
  refreshOpenAIModels,
} from "./openai_models.ts";
import type { ModelRow } from "../turn.ts";

Deno.test("filterOpenAIModels: chat models in, everything else out", () => {
  const ids = [
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5-mini",
    "o4",
    "chatgpt-4o-latest",
    "gpt-5.2-2026-03-11", // dated snapshot — alias covers it
    "gpt-4o-audio-preview",
    "gpt-4o-realtime-preview",
    "tts-1",
    "whisper-1",
    "text-embedding-3-large",
    "dall-e-3",
    "omni-moderation-latest",
    "gpt-4o-transcribe",
    "gpt-4o-search-preview",
    "gpt-3.5-turbo-instruct",
    "davinci-002",
  ];
  const got = filterOpenAIModels(ids).map((m) => m.id);
  assertEquals(got, [
    "openai:o4",
    "openai:gpt-5.2-codex",
    "openai:gpt-5.2",
    "openai:gpt-5-mini",
    "openai:chatgpt-4o-latest",
  ]);
});

Deno.test("filterOpenAIModels: maps to prefixed openai rows", () => {
  const [row] = filterOpenAIModels(["gpt-5.2"]);
  assertEquals(row, { id: "openai:gpt-5.2", label: "gpt-5.2 (OpenAI)", provider: "openai" });
});

Deno.test("mergeModels: static first, dynamic deduped by id", () => {
  const stat: ModelRow[] = [
    { id: "claude-fable-5", label: "Fable 5", provider: "anthropic" },
    { id: "openai:gpt-5", label: "GPT-5 (OpenAI)", provider: "openai" },
  ];
  const dyn: ModelRow[] = [
    { id: "openai:gpt-5", label: "gpt-5 (OpenAI)", provider: "openai" }, // dupe — dropped
    { id: "openai:gpt-5.2", label: "gpt-5.2 (OpenAI)", provider: "openai" },
  ];
  assertEquals(mergeModels(stat, dyn).map((m) => m.id), [
    "claude-fable-5",
    "openai:gpt-5",
    "openai:gpt-5.2",
  ]);
});

Deno.test("refreshOpenAIModels: pulls, filters, and caches from /v1/models", async () => {
  const srv = Deno.serve({ port: 0, onListen: () => {} }, (req) => {
    if (new URL(req.url).pathname === "/v1/models") {
      if (req.headers.get("authorization") !== "Bearer sk-test") {
        return new Response("{}", { status: 401 });
      }
      return Response.json({ data: [{ id: "gpt-5.2" }, { id: "whisper-1" }, { id: 42 }] });
    }
    return new Response("nope", { status: 404 });
  });
  const prevKey = Deno.env.get("OPENAI_API_KEY");
  const prevBase = Deno.env.get("OPENAI_API_BASE");
  try {
    Deno.env.set("OPENAI_API_KEY", "sk-test");
    Deno.env.set("OPENAI_API_BASE", `http://127.0.0.1:${srv.addr.port}`);
    const got = await refreshOpenAIModels();
    assertEquals(got.map((m) => m.id), ["openai:gpt-5.2"]);
    assertEquals(openaiModels().map((m) => m.id), ["openai:gpt-5.2"]);

    // A failed refresh (bad key → 401) keeps the previous cache.
    Deno.env.set("OPENAI_API_KEY", "sk-wrong");
    assertEquals((await refreshOpenAIModels()).map((m) => m.id), ["openai:gpt-5.2"]);

    // No key at all empties it.
    Deno.env.delete("OPENAI_API_KEY");
    assertEquals(await refreshOpenAIModels(), []);
  } finally {
    if (prevKey === undefined) Deno.env.delete("OPENAI_API_KEY");
    else Deno.env.set("OPENAI_API_KEY", prevKey);
    if (prevBase === undefined) Deno.env.delete("OPENAI_API_BASE");
    else Deno.env.set("OPENAI_API_BASE", prevBase);
    await srv.shutdown();
  }
});
