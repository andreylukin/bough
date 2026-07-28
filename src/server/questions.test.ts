/**
 * Tests for the `ask()` REST surface.
 *
 * The acceptance criterion (plan T6.1) is the first test: **a fresh client rebuilds
 * the hold card from `GET /questions`.** Events never replay (plan §6.16), so a
 * client that attaches while a program is parked has no other way to learn the
 * question exists — if this list were empty or wrong, the only visible symptom would
 * be a turn that appears hung, in a client nobody had open at the time.
 *
 * The rest is what happens when two things race, because two things always do here:
 * a question that settled between the read and the write must not answer silently,
 * and a question from another session must not be answerable by guessing a uuid.
 *
 * These drive `createHandler(ctx)` over a fabricated ctx with an in-memory database
 * and no socket bound (plan §7). They use the PROCESS registry, because that is what
 * the routes reach — every test sweeps it afterwards, so nothing leaks into the next.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable from this environment, and a test that cannot run offline does not belong
 * in `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { askHolds, expireAsks, type RaisedAsk } from "../hostfn/ask.ts";
import type { AskQuestion } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { createHandler, type Route, route } from "./app.ts";
import { answerQuestion, listQuestions } from "./questions.ts";

// ---- fixtures ---------------------------------------------------------------

/** The two entries this task appends, isolated from whatever else the table holds. */
const TABLE: Route[] = [
  route("GET", "/questions", listQuestions),
  route("POST", "/sessions/:id/questions/:qid", answerQuestion),
];

function fixture() {
  const db = openDb(":memory:");
  const bus = new Bus({ onListenerError: () => {} });
  const ctx: AppCtx = { db, bus, model: "test-model" };
  return { call: createHandler(ctx, { routes: TABLE }), bus, db };
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;

const get = (path: string) => new Request(url(path));
const post = (path: string, body: unknown) =>
  new Request(url(path), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });

/** Settle everything this file raised, whatever the test did. */
async function sweep(...raised: RaisedAsk[]): Promise<void> {
  expireAsks();
  await Promise.allSettled(raised.map((r) => r.answer));
}

// ---- GET /questions ---------------------------------------------------------

test("AC: a fresh client rebuilds its hold cards from GET /questions", async () => {
  const f = fixture();
  const a = askHolds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "Which env?" });
  const b = askHolds.raise(f.bus, {
    sessionId: "sB",
    messageId: "m2",
    question: "Proceed?",
    options: ["yes", "no"],
  });

  const all = await (await f.call(get("/questions"))).json() as AskQuestion[];
  // A bare array, like GET /sessions: the list IS the resource.
  assert.ok(Array.isArray(all));
  assert.deepEqual(all.map((q) => q.id), [a.record.id, b.record.id]);

  // Everything the card needs is on the record — the question, its options, the
  // message it anchors to — so nothing has to be reconstructed from missed events.
  const card = all[1];
  assert.equal(card.sessionId, "sB");
  assert.equal(card.messageId, "m2");
  assert.equal(card.question, "Proceed?");
  assert.deepEqual(card.options, ["yes", "no"]);
  assert.equal(card.status, "pending");

  // Scoped, so a client watching one session does not render another's hold.
  const scoped = await (await f.call(get("/questions?sessionId=sA"))).json() as AskQuestion[];
  assert.deepEqual(scoped.map((q) => q.id), [a.record.id]);

  await sweep(a, b);

  // Settled holds leave the list, which is what closes the card.
  assert.deepEqual(await (await f.call(get("/questions"))).json(), []);
});

// ---- POST /sessions/:id/questions/:qid --------------------------------------

test("an answer settles the parked program", async () => {
  const f = fixture();
  const q = askHolds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "Which env?" });

  const res = await f.call(post(`/sessions/sA/questions/${q.record.id}`, { answer: "prod" }));
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { ok: true, id: q.record.id, status: "answered" });
  assert.equal(await q.answer, "prod");

  await sweep();
});

test("a decline rejects it catchably", async () => {
  const f = fixture();
  const q = askHolds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "Drop it?" });

  const res = await f.call(post(`/sessions/sA/questions/${q.record.id}`, { decline: true }));
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { ok: true, id: q.record.id, status: "declined" });
  await assert.rejects(() => q.answer, /user declined/);

  await sweep();
});

test("an unknown or restarted question is a 404 that says why", async () => {
  const f = fixture();
  const res = await f.call(post(`/sessions/sA/questions/nope`, { answer: "x" }));
  assert.equal(res.status, 404);
  const body = await res.json() as { error: string };
  // Memory-only is the reason, and saying so is what stops a client treating a
  // post-restart 404 as a bug in the id rather than as the design (spec §6).
  assert.match(body.error, /memory-only/);
});

test("another session's question cannot be answered by guessing its id", async () => {
  const f = fixture();
  const q = askHolds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "Which env?" });

  const res = await f.call(post(`/sessions/sB/questions/${q.record.id}`, { answer: "prod" }));
  assert.equal(res.status, 404);
  // Still parked: the wrong-session post did nothing at all.
  assert.equal(askHolds.get(q.record.id)?.status, "pending");

  await sweep(q);
});

test("an empty answer is a 400, not a resolution with nothing in it", async () => {
  const f = fixture();
  const q = askHolds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "Which env?" });

  for (const body of [{}, { answer: "" }, { answer: "   " }, { decline: false }]) {
    const res = await f.call(post(`/sessions/sA/questions/${q.record.id}`, body));
    assert.equal(res.status, 400, JSON.stringify(body));
    const { error } = await res.json() as { error: string };
    assert.match(error, /decline/);
  }
  // The program is still parked: nothing resolved with "".
  assert.equal(askHolds.get(q.record.id)?.status, "pending");

  await sweep(q);
});

test("a second answer to the same question is refused, not applied", async () => {
  const f = fixture();
  const q = askHolds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "Which env?" });

  // Two clients, both looking at the same card. The first click wins.
  const first = f.call(post(`/sessions/sA/questions/${q.record.id}`, { answer: "prod" }));
  assert.equal((await first).status, 200);

  const second = await f.call(post(`/sessions/sA/questions/${q.record.id}`, { answer: "dev" }));
  // 404 here, because the hold is already gone by the time the second handler reads
  // it. The 409 in `settledMeanwhile` covers the narrower window where it settles
  // BETWEEN that read and the write — which cannot be forced from out here, and is
  // the reason the write's return value is checked at all rather than assumed.
  assert.equal(second.status, 404);
  assert.equal(await q.answer, "prod");

  await sweep();
});
