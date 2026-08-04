/**
 * Tests for the take-back (`history/unsend.ts`).
 *
 * This is the one operation in `history/` that DELETES, so the tests are arranged
 * around the two questions that follow from that: does it remove exactly what it
 * promised, and does it refuse everything else. The refusals are the load-bearing
 * half — a take-back that could be aimed at any message would be an in-place
 * rewrite of history wearing a smaller name, which is precisely what every other
 * module here exists not to be.
 *
 * The route is driven end to end (a real `Request` through `createHandler`) rather
 * than by calling the handler's internals, because the guards are the product: a
 * client one release out of step hits them, and the sentence it gets back is what
 * the user reads. Nothing here binds a socket or leaves an in-memory database.
 *
 * `../server/app.ts` is imported FIRST for the reason `fork.test.ts` states: the
 * handler modules and `app.ts` are a documented cycle that is only safe when
 * `app.ts` is the module that starts evaluating.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { createHandler, type Route, route } from "../server/app.ts";
import { unsendMessageH, type UnsendResult } from "./unsend.ts";

// ---- fixtures ---------------------------------------------------------------

interface Fixture {
  db: SqliteDb;
  ctx: AppCtx;
}

function fixture(): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  // No LLM: nothing here starts a turn, and a fake that could be called would only
  // hide it if something did.
  return { db, ctx: { db, bus, llm: { run: () => Promise.reject(new Error("no llm")) } } };
}

function session(db: SqliteDb, title: string, parentId: string | null = null): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    kind: parentId ? "fork" : "root",
    createdAt: 1_000,
    parentId,
    title,
  });
}

let stamp = 1_000;
function message(
  db: SqliteDb,
  sessionId: string,
  role: Message["role"],
  text: string,
): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role,
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: stamp++,
  });
}

const TABLE: Route[] = [route("POST", "/sessions/:id/unsend", unsendMessageH)];

function poster(f: Fixture) {
  const call = createHandler(f.ctx, { routes: TABLE });
  return (id: string, body: unknown) =>
    call(
      new Request(`http://x/sessions/${id}/unsend`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    );
}

const textsOf = (messages: Message[]) =>
  messages.map((m) => m.parts.map((p) => ("text" in p ? p.text : `<${p.type}>`)).join(""));

// ---- what it removes --------------------------------------------------------

test("the last user message and the answer it provoked are gone; the rest is untouched", async () => {
  const f = fixture();
  const s = session(f.db, "chat");
  message(f.db, s.id, "user", "first ask");
  message(f.db, s.id, "supervisor", "first answer");
  const retracted = message(f.db, s.id, "user", "the typo");
  const partial = message(f.db, s.id, "supervisor", "half an answer to a typo");

  const res = await poster(f)(s.id, { atMessageId: retracted.id });
  assert.equal(res.status, 200);
  const body = await res.json() as UnsendResult;

  // The text comes back for the composer — that is the whole point of the gesture.
  assert.equal(body.text, "the typo");
  assert.deepEqual(body.removed, [retracted.id, partial.id]);
  // Nothing was running, and saying so is not an error (`server/turns.ts`).
  assert.equal(body.interrupted, false);

  // The conversation is now what it was before the message was ever sent.
  assert.deepEqual(textsOf(f.db.messagesFor(s.id)), ["first ask", "first answer"]);
  assert.equal(f.db.getMessage(retracted.id), undefined);
  assert.equal(f.db.getMessage(partial.id), undefined);
});

test("a retracted message stops answering keyword search", async () => {
  const f = fixture();
  const s = session(f.db, "chat");
  const m = message(f.db, s.id, "user", "kumquat migration plan");
  f.db.indexMessage(m);
  assert.equal(f.db.searchMessages("kumquat").length, 1);

  await poster(f)(s.id, { atMessageId: m.id });
  // A deleted message that still matched would surface in `/search` with nothing to
  // open — the FTS row has to go with the message.
  assert.deepEqual(f.db.searchMessages("kumquat"), []);
});

test("the session it was sent in is the one it is removed from — siblings are untouched", async () => {
  const f = fixture();
  const parent = session(f.db, "parent");
  message(f.db, parent.id, "user", "ancestor question");
  const own = session(f.db, "branch", parent.id);
  const mine = message(f.db, own.id, "user", "my ask");

  await poster(f)(own.id, { atMessageId: mine.id });

  assert.deepEqual(textsOf(f.db.messagesFor(own.id)), []);
  // The inherited prefix is another session's rows and was never in scope.
  assert.deepEqual(textsOf(f.db.messagesFor(parent.id)), ["ancestor question"]);
});

// ---- what it refuses --------------------------------------------------------

test("an earlier user message is refused — that is what fork is for", async () => {
  const f = fixture();
  const s = session(f.db, "chat");
  const earlier = message(f.db, s.id, "user", "first ask");
  message(f.db, s.id, "supervisor", "first answer");
  message(f.db, s.id, "user", "second ask");

  const res = await poster(f)(s.id, { atMessageId: earlier.id });
  assert.equal(res.status, 400);
  const { error } = await res.json() as { error: string };
  // The refusal names the operation that works, because a user who reached this has
  // a real intention and a bare 400 leaves them with a key that did nothing.
  assert.match(error, /fork/);
  // And it refused by not deleting, which is the assertion that actually matters.
  assert.deepEqual(textsOf(f.db.messagesFor(s.id)), [
    "first ask",
    "first answer",
    "second ask",
  ]);
});

test("a supervisor message is refused — the model's turns are not the user's to retract", async () => {
  const f = fixture();
  const s = session(f.db, "chat");
  message(f.db, s.id, "user", "ask");
  const answer = message(f.db, s.id, "supervisor", "answer");

  const res = await poster(f)(s.id, { atMessageId: answer.id });
  assert.equal(res.status, 400);
  assert.equal(f.db.messagesFor(s.id).length, 2);
});

test("an ancestor's message is refused — a session cannot delete rows it does not own", async () => {
  const f = fixture();
  const parent = session(f.db, "parent");
  const theirs = message(f.db, parent.id, "user", "ancestor question");
  const own = session(f.db, "branch", parent.id);
  message(f.db, own.id, "user", "my ask");

  const res = await poster(f)(own.id, { atMessageId: theirs.id });
  assert.equal(res.status, 400);
  assert.deepEqual(textsOf(f.db.messagesFor(parent.id)), ["ancestor question"]);
});

test("an unknown session is a 404, and an unknown message a 400", async () => {
  const f = fixture();
  const s = session(f.db, "chat");
  const m = message(f.db, s.id, "user", "ask");

  assert.equal((await poster(f)("nope", { atMessageId: m.id })).status, 404);
  assert.equal((await poster(f)(s.id, { atMessageId: "nope" })).status, 400);
  assert.equal(f.db.messagesFor(s.id).length, 1);
});

// ---- the db primitive under it ----------------------------------------------

test("deleteMessagesFrom cuts a contiguous tail, ties broken by insertion order", () => {
  const db = openDb(":memory:");
  const s = db.createSession({
    id: "s1",
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    title: "same-millisecond",
  });
  // Three messages sharing one timestamp — the seeded-branch case `messagesFor`
  // documents. Ordering has to fall through to rowid, or the cut takes the wrong
  // rows and history silently reorders.
  const ids = ["a", "b", "c"].map((id) =>
    db.createMessage({
      id,
      sessionId: s.id,
      role: "user",
      parts: [{ type: "text", text: id }],
      pending: false,
      createdAt: 5_000,
    })
  );
  assert.deepEqual(db.deleteMessagesFrom(s.id, ids[1].id), ["b", "c"]);
  assert.deepEqual(db.messagesFor(s.id).map((m) => m.id), ["a"]);
  // A message that is not this session's is not this session's to delete.
  assert.deepEqual(db.deleteMessagesFrom(s.id, "gone"), []);
});
