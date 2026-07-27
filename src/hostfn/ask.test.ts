/**
 * `ask()` is a promise that somebody else has to settle, so every test here is about
 * a way it could fail to settle — a parked program is a wedged turn, and a wedged
 * turn is a session the user cannot use again.
 *
 * The two acceptance criteria (plan T6.1) lead:
 *
 *   - **An interrupt settles the hold rather than hanging.** Both routes to it: the
 *     turn's own `signal`, and the turn simply ENDING with a hold still parked —
 *     which is the case nobody writes by hand, because a program torn down by the
 *     wall-clock timeout never unwinds its host promise.
 *   - **A restart leaves nothing pending.** Modelled the only way it can be: a fresh
 *     registry is empty, because the registry is the only place a hold ever lived.
 *     There is no table to check and no recovery pass to run — that IS the design.
 *
 * The third thing under test is the transcript record. A settled question persists as
 * an `AskPart` on the supervisor message (spec §6), and the append must never flip a
 * finished message back to `pending` — a message left pending is a session the UI
 * shows as busy forever.
 *
 * Hermetic and offline: real `Bus`, in-memory database, no worker, no socket, no
 * clock that has to advance. Every test builds its own `AskHolds`, so no test can
 * settle another's question or leave one behind for the next file.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * denied by this environment's egress policy, so the jsr import declared in
 * `deno.json` cannot resolve. `node:assert` is built into the runtime and needs no
 * fetch. (Same constraint `db.test.ts` and `patch.test.ts` document.)
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { SqliteDb } from "../db/db.ts";
import { AskDeclinedError, ProgramError } from "../errors.ts";
import type { AskPart, AskQuestion, Message, Session } from "../schema/parts.ts";
import type { Db, TurnCtx } from "../types.ts";
import { appendAskPart, AskHolds, createAskHostFn } from "./ask.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

const SESSION = "s1";
const MESSAGE = "m1";

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  holds: AskHolds;
  /** Every `ask.question` payload the bus carried, in order. */
  questions: AskQuestion[];
  ctx: TurnCtx;
  abort: AbortController;
  /** The supervisor message as it stands in the database right now. */
  message(): Message;
  /** Its `ask` parts, in order. */
  askParts(): AskPart[];
  /** What the runner does when it closes the message and ends the turn. */
  finishTurn(status?: "done" | "interrupted"): void;
}

function session(id: string, over: Partial<Session> = {}): Session {
  return { id, parentId: null, title: id, kind: "root", createdAt: 1_000, ...over };
}

function fixture(): Fixture {
  const db = new SqliteDb(":memory:", {});
  const bus = new Bus({ onListenerError: () => {} });
  const holds = new AskHolds(() => Date.now());
  const questions: AskQuestion[] = [];
  bus.subscribe((e) => {
    if (e.type === "ask.question") questions.push(e.data as AskQuestion);
  });

  db.createSession(session(SESSION));
  db.createMessage({
    id: MESSAGE,
    sessionId: SESSION,
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: 1_000,
  });

  const abort = new AbortController();
  const ctx = {
    db: db as Db,
    bus,
    sessionId: SESSION,
    turnId: "t1",
    messageId: MESSAGE,
    workspace: "/tmp",
    model: "test-model",
    signal: abort.signal,
    depth: 0,
  } as TurnCtx;

  const message = () => db.getMessage(MESSAGE)!;
  return {
    db,
    bus,
    holds,
    questions,
    ctx,
    abort,
    message,
    askParts: () => message().parts.filter((p): p is AskPart => p.type === "ask"),
    /**
     * The exact two events the turn runner emits when it ends, in the order it
     * emits them: the message is closed first, then the turn.
     */
    finishTurn(status: "done" | "interrupted" = "done") {
      const current = db.getMessage(MESSAGE)!;
      db.updateMessage(MESSAGE, current.parts, false);
      bus.publish({ type: "message.finished", sessionId: SESSION, data: { messageId: MESSAGE } });
      bus.publish({
        type: "turn.finished",
        sessionId: SESSION,
        data: { turnId: "t1", sessionId: SESSION, status },
      });
    },
  };
}

/** The error a rejected promise carried. */
async function rejection(p: Promise<unknown>): Promise<Error> {
  try {
    await p;
  } catch (err) {
    return err as Error;
  }
  throw new assert.AssertionError({ message: "expected a rejection, got none" });
}

// ---------------------------------------------------------------------------
// the registry
// ---------------------------------------------------------------------------

Deno.test("raise → answer: resolves, and the SAME id is re-emitted as answered", async () => {
  const f = fixture();
  const { record, answer } = f.holds.raise(f.bus, {
    sessionId: SESSION,
    messageId: MESSAGE,
    question: "Which env?",
    options: ["dev", "prod"],
  });

  // Registered, listed, announced pending — in that order, so a listener that
  // answers synchronously finds the hold rather than racing it.
  assert.equal(f.holds.get(record.id)?.question, "Which env?");
  assert.deepEqual(f.holds.list(SESSION).map((q) => q.id), [record.id]);
  assert.equal(f.questions[0].status, "pending");
  assert.deepEqual(f.questions[0].options, ["dev", "prod"]);

  assert.equal(f.holds.answer(record.id, "prod"), true);
  assert.equal(await answer, "prod");

  // Settled: gone from the registry, final event on the SAME id so the hold card
  // updates in place rather than a second card appearing beside a stale one.
  assert.equal(f.holds.get(record.id), undefined);
  assert.equal(f.holds.size, 0);
  assert.equal(f.questions[1].id, record.id);
  assert.equal(f.questions[1].status, "answered");
  assert.equal(f.questions[1].answer, "prod");

  // A second settle is a no-op: two clients answering at once is a race, not an
  // error, and the first one wins.
  assert.equal(f.holds.answer(record.id, "dev"), false);
  assert.equal(f.questions.length, 2);
});

Deno.test("raise → decline: rejects catchably with 'user declined'", async () => {
  const f = fixture();
  const { record, answer } = f.holds.raise(f.bus, {
    sessionId: SESSION,
    messageId: MESSAGE,
    question: "Drop the table?",
  });
  assert.equal(f.holds.decline(record.id), true);

  const err = await rejection(answer);
  assert.ok(err instanceof AskDeclinedError);
  // The phrase is what spec §6 requires the model to be told, and the question is
  // repeated so a program holding several knows which one was dismissed.
  assert.match(err.message, /user declined/);
  assert.match(err.message, /Drop the table\?/);
  assert.equal(f.questions[1].status, "declined");
  assert.equal(f.holds.size, 0);
});

Deno.test("AC: an interrupt settles the hold rather than hanging", async () => {
  const f = fixture();
  const { answer } = f.holds.raise(
    f.bus,
    { sessionId: SESSION, messageId: MESSAGE, question: "Which branch?" },
    f.abort.signal,
  );
  assert.equal(f.holds.size, 1);

  f.abort.abort();

  // Settled, not hanging: the promise is already rejected by the time the abort
  // returns, and the hold is out of the registry.
  const err = await rejection(answer);
  assert.ok(err instanceof ProgramError);
  assert.match(err.message, /interrupted/);
  // Distinguishable from a decline — "you stopped it" and "the user said no" call
  // for different moves from the program.
  assert.ok(!(err instanceof AskDeclinedError));
  assert.equal(f.holds.size, 0);
  assert.equal(f.questions[1].status, "interrupted");
});

Deno.test("an already-aborted signal settles immediately, without registering", async () => {
  const f = fixture();
  f.abort.abort();
  const { answer } = f.holds.raise(
    f.bus,
    { sessionId: SESSION, messageId: MESSAGE, question: "Which?" },
    f.abort.signal,
  );
  await rejection(answer);
  assert.equal(f.holds.size, 0);
  assert.equal(f.questions.at(-1)?.status, "interrupted");
});

Deno.test("expire clears one session's holds and leaves the other's", async () => {
  const f = fixture();
  const a = f.holds.raise(f.bus, { sessionId: "sA", messageId: "m1", question: "a?" });
  const b = f.holds.raise(f.bus, { sessionId: "sB", messageId: "m2", question: "b?" });

  assert.equal(f.holds.expire("sA"), 1);
  await rejection(a.answer);
  assert.deepEqual(f.holds.list().map((q) => q.id), [b.record.id]);

  assert.equal(f.holds.expire(), 1);
  await rejection(b.answer);
  assert.equal(f.holds.size, 0);
  // Sweeping an empty registry is a no-op, not a second round of events.
  assert.equal(f.holds.expire(), 0);
});

Deno.test("list is oldest-first and scoped, so a client can rebuild its cards", async () => {
  let t = 0;
  const holds = new AskHolds(() => ++t);
  const bus = new Bus({ onListenerError: () => {} });
  const raised = [
    holds.raise(bus, { sessionId: "sA", messageId: "m", question: "1" }),
    holds.raise(bus, { sessionId: "sB", messageId: "m", question: "2" }),
    holds.raise(bus, { sessionId: "sA", messageId: "m", question: "3" }),
  ];
  // Nobody is awaiting these; the sweep below rejects them all, and an unobserved
  // rejection would take the test runner down rather than this test.
  const settled = raised.map((r) => rejection(r.answer));

  assert.deepEqual(holds.list("sA").map((q) => q.id), [raised[0].record.id, raised[2].record.id]);
  assert.deepEqual(holds.list().map((q) => q.question), ["1", "2", "3"]);
  holds.expire();
  await Promise.all(settled);
});

// ---------------------------------------------------------------------------
// AC: a restart leaves nothing pending
// ---------------------------------------------------------------------------

Deno.test("AC: a restart leaves nothing pending — there is nothing to heal", async () => {
  const f = fixture();
  const before = new AskHolds();
  const { record, answer } = before.raise(f.bus, {
    sessionId: SESSION,
    messageId: MESSAGE,
    question: "Which env?",
  });
  assert.equal(before.size, 1);

  // The process ends. Nothing is written, nothing is flushed — the registry simply
  // ceases to exist along with the turn that owned the hold.
  const after = new AskHolds();
  assert.equal(after.size, 0);
  assert.equal(after.list().length, 0);
  assert.equal(after.get(record.id), undefined);
  // …and the answer route finds nothing to settle, which is what makes the 404 in
  // `server/questions.ts` the honest answer rather than a lost update.
  assert.equal(after.answer(record.id, "prod"), false);
  assert.equal(after.decline(record.id), false);
  assert.equal(after.expire(), 0);

  // The message carries no half-written hold either: the durable record is only ever
  // written once the question has SETTLED, so there is no pending ask on the row for
  // a recovery pass to find.
  assert.equal(f.askParts().length, 0);

  before.expire();
  await rejection(answer);
});

// ---------------------------------------------------------------------------
// the settled part
// ---------------------------------------------------------------------------

Deno.test("appendAskPart preserves `pending` and is idempotent on the id", () => {
  const f = fixture();
  const part: AskPart = {
    type: "ask",
    id: "q1",
    question: "Which env?",
    options: ["dev", "prod"],
    status: "answered",
    answer: "prod",
  };

  assert.equal(appendAskPart(f.db, f.bus, SESSION, MESSAGE, part), true);
  assert.equal(f.message().pending, true, "an append during the turn leaves it pending");
  // A second append of the same question is refused: a transcript with the question
  // in it twice is worse than one missing it.
  assert.equal(appendAskPart(f.db, f.bus, SESSION, MESSAGE, part), false);
  assert.equal(f.askParts().length, 1);

  // Once the runner has closed the message, a late append must NOT reopen it — a
  // message left pending is a session the UI shows as busy forever.
  f.db.updateMessage(MESSAGE, f.message().parts, false);
  appendAskPart(f.db, f.bus, SESSION, MESSAGE, { ...part, id: "q2" });
  assert.equal(f.message().pending, false);
  assert.equal(f.askParts().length, 2);

  // A message that no longer exists is not an error worth raising into a program
  // that has already been given its answer.
  assert.equal(appendAskPart(f.db, f.bus, SESSION, "gone", part), false);
});

// ---------------------------------------------------------------------------
// the bridged host function
// ---------------------------------------------------------------------------

Deno.test("ask() resolves with the answer and records it on the message", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });

  const parked = ask!("Which env?", JSON.stringify({ options: ["dev", "prod"] }));
  const live = f.holds.list(SESSION);
  assert.equal(live.length, 1);
  assert.deepEqual(live[0].options, ["dev", "prod"]);

  f.holds.answer(live[0].id, "prod");
  assert.equal(await parked, "prod");

  // Buffered until the runner's last write: the runner owns the parts array in
  // memory and rewrites it wholesale, so a part written now would be erased by the
  // very next append (the tool_result of the program that asked).
  assert.equal(f.askParts().length, 0);

  f.finishTurn();
  const parts = f.askParts();
  assert.equal(parts.length, 1);
  assert.deepEqual(parts[0], {
    type: "ask",
    id: live[0].id,
    question: "Which env?",
    options: ["dev", "prod"],
    status: "answered",
    answer: "prod",
  });
  assert.equal(f.message().pending, false);
});

Deno.test("a part written during the turn would be erased — the buffer is why it is not", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  const parked = ask!("Which env?", "{}");
  f.holds.answer(f.holds.list()[0].id, "prod");
  await parked;

  // The runner appends the program's tool_result from its own in-memory array, which
  // has never seen the ask part. This is the write that would clobber it.
  f.db.updateMessage(MESSAGE, [{ type: "text", text: "done" }], true);

  f.finishTurn();
  // Survived, because it was flushed after that write rather than before it.
  assert.equal(f.askParts().length, 1);
  assert.equal(f.askParts()[0].status, "answered");
});

Deno.test("ask() rejects catchably on decline and records the dismissal", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  const parked = ask!("Drop the table?", "{}");
  f.holds.decline(f.holds.list()[0].id);

  const err = await rejection(parked);
  assert.ok(err instanceof AskDeclinedError);
  assert.match(err.message, /user declined/);

  f.finishTurn();
  const parts = f.askParts();
  assert.equal(parts.length, 1);
  assert.equal(parts[0].status, "declined");
  assert.equal(parts[0].answer, undefined);
});

Deno.test("AC: interrupting the turn settles a parked ask() rather than hanging it", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  const parked = ask!("Which branch?", "{}");
  assert.equal(f.holds.size, 1);

  f.abort.abort();

  const err = await rejection(parked);
  assert.match(err.message, /interrupted/);
  assert.equal(f.holds.size, 0);

  f.finishTurn("interrupted");
  assert.equal(f.askParts()[0].status, "interrupted");
});

Deno.test("AC: a hold still parked when the turn ends is swept, not left haunting", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  // No signal abort and no answer: the shape a wall-clock timeout leaves behind,
  // where the worker is gone but the host promise was never unwound.
  const parked = ask!("Which env?", "{}");
  assert.equal(f.holds.size, 1);

  f.finishTurn();

  const err = await rejection(parked);
  assert.match(err.message, /interrupted/);
  assert.equal(f.holds.size, 0, "the hold is gone from the registry");
  assert.equal(f.holds.list(SESSION).length, 0, "and from every client's card list");
  // The final event says how it ended, so a card that was showing "pending" closes.
  assert.equal(f.questions.at(-1)?.status, "interrupted");
  // Swept after the message closed, so its part applies straight through.
  assert.equal(f.askParts().length, 1);
  assert.equal(f.askParts()[0].status, "interrupted");
  assert.equal(f.message().pending, false, "and the message is not reopened");
});

Deno.test("the turn's bus subscription is released when the turn ends", async () => {
  const f = fixture();
  const before = f.bus.size;
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  // Nothing is subscribed until the first question — a turn that never asks pays
  // nothing.
  assert.equal(f.bus.size, before);

  const parked = ask!("Which env?", "{}");
  assert.equal(f.bus.size, before + 1);
  f.finishTurn();
  await rejection(parked);
  assert.equal(f.bus.size, before, "no listener leak per turn");
});

Deno.test("two questions in one turn both land, in the order they were asked", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });

  const first = ask!("Which env?", JSON.stringify({ options: ["dev", "prod"] }));
  f.holds.answer(f.holds.list()[0].id, "prod");
  assert.equal(await first, "prod");

  const second = ask!("Proceed?", "{}");
  f.holds.decline(f.holds.list()[0].id);
  await rejection(second);

  f.finishTurn();
  assert.deepEqual(f.askParts().map((p) => [p.question, p.status]), [
    ["Which env?", "answered"],
    ["Proceed?", "declined"],
  ]);
});

Deno.test("ask() refuses before announcing a card nobody can answer", async () => {
  const f = fixture();
  f.abort.abort();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  const err = await rejection(ask!("Which env?", "{}"));
  assert.match(err.message, /interrupted/);
  // Nothing was announced and nothing parked: the turn was already over.
  assert.equal(f.questions.length, 0);
  assert.equal(f.holds.size, 0);
});

Deno.test("options are read leniently; a malformed bag is refused with the fix", async () => {
  const f = fixture();
  const appended: AskPart[] = [];
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds, append: (p) => appended.push(p) });

  // Non-strings become strings, blanks are dropped: a model that wrote `[1, 2]`
  // meant two choices, and refusing the question over it costs a round to learn
  // nothing.
  const parked = ask!("Pick", JSON.stringify({ options: [1, " prod ", "", "  "] }));
  assert.deepEqual(f.holds.list()[0].options, ["1", "prod"]);
  f.holds.answer(f.holds.list()[0].id, "prod");
  await parked;
  assert.deepEqual(appended[0].options, ["1", "prod"]);

  // No options at all is free text, and the part carries no empty array.
  const free = ask!("Anything?", "{}");
  assert.equal(f.holds.list()[0].options, undefined);
  f.holds.answer(f.holds.list()[0].id, "sure");
  await free;
  assert.equal(appended[1].options, undefined);

  // A bag that is not an object at all is a call shaped wrongly.
  const bad = await rejection(ask!("Pick", '"dev"'));
  assert.match(bad.message, /options/);
  // …and so is an empty question.
  const empty = await rejection(ask!("   ", "{}"));
  assert.match(empty.message, /question is empty/);

  f.finishTurn();
});

Deno.test("a settled ask never reopens a message the runner already closed", async () => {
  const f = fixture();
  const { ask } = createAskHostFn(f.ctx, { holds: f.holds });
  const parked = ask!("Which env?", "{}");
  // The turn dies first — message closed, turn finished — and only then does the
  // sweep settle the hold and write its part.
  f.finishTurn("interrupted");
  await rejection(parked);
  assert.equal(f.message().pending, false);
  assert.equal(f.askParts().length, 1);
});
