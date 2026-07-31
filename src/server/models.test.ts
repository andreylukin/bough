/**
 * The catalog route, proved offline.
 *
 * Two things this has to establish: **a discovered model reaches the picker at all**
 * (the regression that started this — the discovery code existed and nothing called
 * it), and **a slow provider cannot delay the terminal**, because the TUI awaits this
 * before its first frame.
 *
 * `discover` is injected everywhere, so nothing here reads a key or opens a socket.
 */
import assert from "node:assert/strict";
import { test } from "bun:test";
import { MODELS, type ModelRow } from "../llm/client.ts";
import { modelCatalog, resetModelCatalog } from "./models.ts";

const LUNA: ModelRow = { id: "openai:gpt-5.6-luna", label: "gpt-5.6-luna (OpenAI)", provider: "openai" };

/** A discovery that never settles — what a hung `api.openai.com` looks like from here. */
const hung = () => new Promise<ModelRow[]>(() => {});

test("discovered rows land after the static table, which keeps its ids", async () => {
  resetModelCatalog();
  const rows = await modelCatalog({ discover: () => Promise.resolve([LUNA]) });
  assert.deepEqual(rows.slice(0, MODELS.length), MODELS);
  assert.deepEqual(rows.at(-1), LUNA);
});

test("a hung provider answers from the static table instead of blocking the boot", async () => {
  resetModelCatalog();
  const rows = await modelCatalog({ discover: hung, deadlineMs: 1 });
  assert.deepEqual(rows, MODELS);
});

test("the discovery a deadline gave up on still warms the cache for the next ask", async () => {
  resetModelCatalog();
  let release: (rows: ModelRow[]) => void = () => {};
  const slow = () => new Promise<ModelRow[]>((resolve) => (release = resolve));

  assert.deepEqual(await modelCatalog({ discover: slow, deadlineMs: 1 }), MODELS);
  release([LUNA]);
  await Promise.resolve(); // let the abandoned discovery settle into the cache
  assert.deepEqual((await modelCatalog({ discover: hung })).at(-1), LUNA);
});

test("one discovery in flight, however many callers ask at once", async () => {
  resetModelCatalog();
  let calls = 0;
  const counted = () => {
    calls++;
    return Promise.resolve([LUNA]);
  };
  await Promise.all([1, 2, 3].map(() => modelCatalog({ discover: counted })));
  assert.equal(calls, 1);
});

test("a fresh cache is not re-discovered; an expired one is", async () => {
  resetModelCatalog();
  let calls = 0;
  const counted = () => {
    calls++;
    return Promise.resolve([LUNA]);
  };
  let clock = 1_000_000;
  const now = () => clock;

  await modelCatalog({ discover: counted, now });
  await modelCatalog({ discover: counted, now });
  assert.equal(calls, 1);

  clock += 11 * 60_000; // past the TTL
  await modelCatalog({ discover: counted, now });
  assert.equal(calls, 2);
});

test("a discovery that throws degrades to the static table", async () => {
  // `discoverOpenAIModels` documents that it never throws. If that ever stops being
  // true, the picker loses rows — it does not take the server down with an unhandled
  // rejection on a module-level promise.
  resetModelCatalog();
  const rows = await modelCatalog({ discover: () => Promise.reject(new Error("boom")) });
  assert.deepEqual(rows, MODELS);
});
