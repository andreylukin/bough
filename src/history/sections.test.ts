/**
 * Topic sections. Two things are worth testing here and the LLM call is neither of
 * them:
 *
 *   1. **The pass is stateless.** Nothing is read from or written to the database (the
 *      handler's 404 check aside), so labeling the same history twice leaves the tree
 *      exactly as it was — asserted against a full snapshot, not a spot check.
 *   2. **The reply is forced into a clean partition of the turns the CLIENT sent.** The
 *      returned ranges are rendered directly and offered as selections, so a gap is
 *      turns the user can see and cannot select, and an overlap is one turn wearing two
 *      labels. `normalizeSections` is pure and gets the heaviest coverage (plan §7).
 *
 * Offline and hermetic: an in-memory database and a scripted fake `LlmClient`.
 * `node:assert/strict`, because jsr.io is unreachable and `@std/assert` cannot resolve.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { SectionsError } from "../errors.ts";
import type { Session } from "../schema/parts.ts";
import type { AppCtx, LlmClient, LlmParams, LlmResult } from "../types.ts";
import { createHandler } from "../server/app.ts";
import {
  normalizeSections,
  parseSections,
  type Section,
  sectionize,
  SECTIONS_MODEL,
} from "./sections.ts";

// ---- fixtures ---------------------------------------------------------------

interface FakeLlm extends LlmClient {
  prompts: string[];
  models: string[];
}

function fakeLlm(reply: string): FakeLlm {
  const prompts: string[] = [];
  const models: string[] = [];
  return {
    prompts,
    models,
    run(params: LlmParams): Promise<LlmResult> {
      const block = params.messages[0].content[0];
      prompts.push(block.type === "text" ? block.text : "");
      models.push(params.model);
      return Promise.resolve({
        content: [{ type: "text", text: reply }],
        stopReason: "end_turn",
      });
    },
  };
}

function gists(...lines: string[]): { gist: string }[] {
  return lines.map((gist) => ({ gist }));
}

function fixture(reply: string) {
  const db = openDb(":memory:");
  const bus = new Bus();
  const llm = fakeLlm(reply);
  const ctx = { db, bus, llm } as unknown as AppCtx;
  return { db, bus, llm, ctx };
}

function session(db: SqliteDb, title: string): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    title,
  });
}

function ranges(sections: Section[]): [number, number, string][] {
  return sections.map((s) => [s.start, s.end, s.label]);
}

// ---- parsing ----------------------------------------------------------------

test("parseSections tolerates code fences and surrounding prose", () => {
  const wanted = [{ start: 0, end: 1, label: "auth" }];
  assert.deepEqual(parseSections('[{"start":0,"end":1,"label":"auth"}]'), wanted);
  assert.deepEqual(
    parseSections('```json\n[{"start":0,"end":1,"label":"auth"}]\n```'),
    wanted,
  );
  assert.deepEqual(
    parseSections('Here you go: [{"start":0,"end":1,"label":"auth"}] — hope that helps!'),
    wanted,
  );
});

test("parseSections returns null on anything it cannot read", () => {
  assert.equal(parseSections("I'd rather not."), null);
  assert.equal(parseSections("[not json]"), null);
  assert.equal(parseSections('[{"start":"zero","end":1,"label":"auth"}]'), null, "wrong types");
  assert.equal(parseSections('[{"start":0,"end":1}]'), null, "missing label");
  assert.equal(parseSections("]["), null, "closing bracket before the opening one");
});

// ---- normalization ----------------------------------------------------------

test("normalizeSections fills gaps so every turn is selectable", () => {
  const out = normalizeSections([{ start: 2, end: 3, label: "theme picker" }], 6);
  assert.deepEqual(ranges(out), [
    [0, 1, "…"],
    [2, 3, "theme picker"],
    [4, 5, "…"],
  ]);
});

test("normalizeSections trims overlaps so no turn wears two labels", () => {
  const out = normalizeSections([
    { start: 0, end: 3, label: "first" },
    { start: 2, end: 5, label: "second" },
    { start: 1, end: 2, label: "swallowed" },
  ], 6);
  assert.deepEqual(ranges(out), [[0, 3, "first"], [4, 5, "second"]]);
});

test("normalizeSections sorts, clips to bounds, and drops the impossible", () => {
  const out = normalizeSections([
    { start: 3, end: 99, label: "past the end" },
    { start: 0, end: 2, label: "first" },
    { start: 7, end: 8, label: "entirely past the end" },
    { start: 2, end: 1, label: "backwards" },
  ], 5);
  assert.deepEqual(ranges(out), [[0, 2, "first"], [3, 4, "past the end"]]);
});

test("normalizeSections always covers exactly [0, n)", () => {
  for (
    const raw of [
      [],
      [{ start: 0, end: 0, label: "a" }],
      [{ start: 1, end: 1, label: "b" }],
      [{ start: 0, end: 9, label: "everything" }],
    ]
  ) {
    const out = normalizeSections(raw, 4);
    assert.equal(out[0].start, 0);
    assert.equal(out.at(-1)?.end, 3);
    for (let i = 1; i < out.length; i++) {
      assert.equal(out[i].start, out[i - 1].end + 1, "contiguous, no gap and no overlap");
    }
  }
});

test("normalizeSections clips a runaway label", () => {
  const [only] = normalizeSections([{ start: 0, end: 0, label: "x".repeat(500) }], 1);
  assert.equal(only.label.length, 60);
});

// ---- the pass ---------------------------------------------------------------

test("sectionize numbers the turns it sends and labels them on the cheap model", async () => {
  const llm = fakeLlm(
    '[{"start":0,"end":1,"label":"token refresh race"},{"start":2,"end":2,"label":"theme picker"}]',
  );

  const out = await sectionize(
    { llm },
    gists("fix the refresh", "still failing\nsecond line", "pick a theme"),
  );

  assert.deepEqual(ranges(out), [
    [0, 1, "token refresh race"],
    [2, 2, "theme picker"],
  ]);
  assert.deepEqual(llm.models, [SECTIONS_MODEL], "never the session's frontier model");
  assert.deepEqual(llm.prompts[0].split("\n"), [
    "0. fix the refresh",
    "1. still failing second line",
    "2. pick a theme",
  ], "one line per turn — a newline in a gist would shift every index after it");
});

test("an unparseable reply is a 502 that says nothing was stored", async () => {
  const llm = fakeLlm("I can't do that.");

  const err = await sectionize({ llm }, gists("a", "b")).then(
    () => null,
    (e: unknown) => e,
  );

  assert.ok(err instanceof SectionsError);
  assert.equal(err.status, 502);
  assert.match(err.message, /nothing was stored/);
});

test("a sloppy reply still comes back as a usable partition", async () => {
  const llm = fakeLlm('[{"start":1,"end":99,"label":"the rest"}]');

  const out = await sectionize({ llm }, gists("a", "b", "c"));

  assert.deepEqual(ranges(out), [[0, 0, "…"], [1, 2, "the rest"]]);
});

// ---- the route --------------------------------------------------------------

test("POST /sessions/:id/sections is reachable, returns ranges, and stores nothing", async () => {
  const f = fixture('[{"start":0,"end":1,"label":"auth token refresh"}]');
  const s = session(f.db, "the work");
  f.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: s.id,
    role: "user",
    parts: [{ type: "text", text: "hello" }],
    pending: false,
    createdAt: 1_700_000_000_000,
  });
  const before = JSON.stringify({
    sessions: f.db.listSessions(),
    messages: f.db.messagesFor(s.id),
  });
  const handler = createHandler(f.ctx);

  const res = await handler(
    new Request(`http://localhost/sessions/${s.id}/sections`, {
      method: "POST",
      body: JSON.stringify({ turns: gists("fix the refresh", "still failing") }),
    }),
  );

  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), {
    sections: [{ start: 0, end: 1, label: "auth token refresh" }],
  });
  assert.equal(
    JSON.stringify({ sessions: f.db.listSessions(), messages: f.db.messagesFor(s.id) }),
    before,
    "a labeling pass is read-only — no session, no message, nothing stored",
  );
});

test("the route refuses an unknown session and an empty turn list", async () => {
  const f = fixture("[]");
  const handler = createHandler(f.ctx);

  const missing = await handler(
    new Request("http://localhost/sessions/nope/sections", {
      method: "POST",
      body: JSON.stringify({ turns: gists("a") }),
    }),
  );
  assert.equal(missing.status, 404);
  assert.equal(f.llm.prompts.length, 0, "a stale id must not buy an LLM call");

  const s = session(f.db, "the work");
  const empty = await handler(
    new Request(`http://localhost/sessions/${s.id}/sections`, {
      method: "POST",
      body: JSON.stringify({ turns: [] }),
    }),
  );
  assert.equal(empty.status, 400);
});
