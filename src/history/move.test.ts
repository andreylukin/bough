/**
 * Move-into, with the claim the operation's name works against: **it is a COPY.** The
 * source keeps every turn it had — the AC's byte-identical snapshot — and the target
 * gains fresh messages at the end of its own.
 *
 * The other half of the coverage is the three refusals, each of which exists because the
 * copies land at the TAIL of the target's own messages: itself, a target running a turn,
 * and an ANCESTOR of the source (whose tail sits in the middle of the source's visible
 * thread). The ancestor case is the one worth having a test for — it is the case where
 * "the source's rows are untouched" is still true and the source's THREAD changes anyway.
 *
 * Offline: an in-memory database and a real bus, no LLM (move-into copies, it does not
 * summarize). `node:assert/strict` — jsr.io is unreachable here (plan §7).
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { MoveError } from "../errors.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { createHandler, type Route, route } from "../server/app.ts";
import { move, moveIntoH, type MoveCtx } from "./move.ts";

// ---- fixtures ---------------------------------------------------------------

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  ctx: MoveCtx & AppCtx;
}

function fixture(): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  return { db, bus, events, ctx: { db, bus } };
}

function session(db: SqliteDb, over: Partial<Session> & { title: string }): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    ...over,
  });
}

let stamp = 1_700_000_000_000;

function message(
  db: SqliteDb,
  sessionId: string,
  role: Message["role"],
  parts: Part[],
): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role,
    parts,
    pending: false,
    createdAt: stamp++,
  });
}

function text(db: SqliteDb, sessionId: string, role: Message["role"], t: string): Message {
  return message(db, sessionId, role, [{ type: "text", text: t }]);
}

function textsOf(messages: readonly Message[]): string[] {
  return messages.map((m) =>
    m.parts.filter((p): p is Extract<Part, { type: "text" }> => p.type === "text")
      .map((p) => p.text).join(" | ")
  );
}

function snapshot(db: SqliteDb, sessionId: string): string {
  return JSON.stringify({
    session: db.getSession(sessionId),
    messages: db.messagesFor(sessionId),
  });
}

/** Two unrelated roots: one to copy from, one to append onto. */
function pair(f: Fixture): {
  source: Session;
  target: Session;
  sourceMessages: Message[];
  targetMessages: Message[];
} {
  const source = session(f.db, { title: "the investigation" });
  const sourceMessages = [
    text(f.db, source.id, "user", "why does the ticker fire twice?"),
    text(f.db, source.id, "supervisor", "catch-up advances from now, not the stale value"),
  ];
  const target = session(f.db, { title: "the fix" });
  const targetMessages = [
    text(f.db, target.id, "user", "let's write it up"),
  ];
  return { source, target, sourceMessages, targetMessages };
}

// ---- the copy ----------------------------------------------------------------

Deno.test("move-into appends copies at the end of the target's own messages", () => {
  const f = fixture();
  const { source, target, sourceMessages, targetMessages } = pair(f);

  const { session: returned, messages } = move(f.ctx, target.id, {
    sourceId: source.id,
    picks: sourceMessages.map((m) => ({ messageId: m.id })),
  });

  assert.equal(returned.id, target.id);
  assert.deepEqual(textsOf(f.db.messagesFor(target.id)), [
    "let's write it up",
    "why does the ticker fire twice?",
    "catch-up advances from now, not the stale value",
  ]);
  // Fresh ids and fresh session — copies, not the originals re-parented.
  assert.equal(messages.every((m) => m.sessionId === target.id), true);
  assert.equal(
    messages.some((m) => sourceMessages.some((s) => s.id === m.id)),
    false,
  );
  // Roles ride along: a copied supervisor turn stays a supervisor turn.
  assert.deepEqual(messages.map((m) => m.role), ["user", "supervisor"]);
  assert.equal(f.db.messagesFor(target.id).length, targetMessages.length + 2);
  f.db.close();
});

Deno.test("the source is byte-identical afterwards — this is a copy, not a move", () => {
  const f = fixture();
  const { source, target, sourceMessages } = pair(f);
  const before = snapshot(f.db, source.id);

  move(f.ctx, target.id, {
    sourceId: source.id,
    picks: sourceMessages.map((m) => ({ messageId: m.id })),
  });

  assert.equal(snapshot(f.db, source.id), before);
  f.db.close();
});

Deno.test("picks reach into the SOURCE's ancestors, and land in thread order", () => {
  const f = fixture();
  const parent = session(f.db, { title: "the origin" });
  const inherited = text(f.db, parent.id, "supervisor", "the parser skips comments");
  const source = session(f.db, {
    title: "fork · the origin",
    kind: "fork",
    parentId: parent.id,
  });
  const own = text(f.db, source.id, "supervisor", "and now template literals");
  const target = session(f.db, { title: "the writeup" });

  const { messages } = move(f.ctx, target.id, {
    sourceId: source.id,
    // Sent bottom-up: the selection is not a sequence.
    picks: [{ messageId: own.id }, { messageId: inherited.id }],
  });

  assert.deepEqual(textsOf(messages), [
    "the parser skips comments",
    "and now template literals",
  ]);
  // The ancestor kept its message; nothing was moved out of it.
  assert.equal(f.db.messagesFor(parent.id).length, 1);
  f.db.close();
});

Deno.test("a part-level pick copies a turn's prose without its tool calls", () => {
  const f = fixture();
  const { source, target } = pair(f);
  const turn = message(f.db, source.id, "supervisor", [
    { type: "text", text: "The ticker advances next_run_at from now." },
    { type: "tool_call", id: "t1", name: "run_steps", input: { code: "await bash('rg tick')" } },
    { type: "tool_result", callId: "t1", isError: false, output: "schedules.ts:88" },
  ]);

  const { messages } = move(f.ctx, target.id, {
    sourceId: source.id,
    picks: [{ messageId: turn.id, parts: [0] }],
  });

  assert.deepEqual(messages[0].parts, [
    { type: "text", text: "The ticker advances next_run_at from now." },
  ]);
  assert.equal(f.db.getMessage(turn.id)?.parts.length, 3);
  f.db.close();
});

Deno.test("each copy is announced as message.started on the target", () => {
  const f = fixture();
  const { source, target, sourceMessages } = pair(f);
  f.events.length = 0;

  move(f.ctx, target.id, {
    sourceId: source.id,
    picks: sourceMessages.map((m) => ({ messageId: m.id })),
  });

  // No session.created: move-into creates nothing.
  assert.deepEqual(f.events.map((e) => e.type), ["message.started", "message.started"]);
  assert.ok(f.events.every((e) => e.sessionId === target.id));
  f.db.close();
});

// ---- the refusals ------------------------------------------------------------

Deno.test("a session cannot receive its own turns", () => {
  const f = fixture();
  const { source, sourceMessages } = pair(f);

  assert.throws(
    () =>
      move(f.ctx, source.id, {
        sourceId: source.id,
        picks: [{ messageId: sourceMessages[0].id }],
      }),
    (e: unknown) =>
      e instanceof MoveError && (e as MoveError).status === 400 &&
      /source and target are both/.test((e as Error).message),
  );
  assert.equal(f.db.messagesFor(source.id).length, 2);
  f.db.close();
});

Deno.test("an ANCESTOR of the source is refused: it would rewrite the source's thread", () => {
  const f = fixture();
  const parent = session(f.db, { title: "the origin" });
  text(f.db, parent.id, "user", "first");
  const child = session(f.db, {
    title: "fork · the origin",
    kind: "fork",
    parentId: parent.id,
  });
  const own = text(f.db, child.id, "supervisor", "second");
  const threadBefore = f.db.threadFor(child.id).map((m) => m.id);

  assert.throws(
    () => move(f.ctx, parent.id, { sourceId: child.id, picks: [{ messageId: own.id }] }),
    (e: unknown) =>
      e instanceof MoveError && (e as MoveError).status === 400 &&
      /is an ancestor of/.test((e as Error).message),
  );
  // Nothing was written, so the source's visible thread is untouched.
  assert.deepEqual(f.db.threadFor(child.id).map((m) => m.id), threadBefore);
  f.db.close();
});

Deno.test("a target running a turn is a 409, not an interleaved transcript", () => {
  const f = fixture();
  const { source, target, sourceMessages, targetMessages } = pair(f);
  // One turn per session (spec §5): a live turn owns the tail this would append to.
  f.db.createTurn({
    id: crypto.randomUUID(),
    sessionId: target.id,
    messageId: targetMessages[0].id,
    status: "running",
    step: "streaming",
    createdAt: 1_000,
    updatedAt: 1_000,
  });

  assert.throws(
    () =>
      move(f.ctx, target.id, {
        sourceId: source.id,
        picks: [{ messageId: sourceMessages[0].id }],
      }),
    (e: unknown) =>
      e instanceof MoveError && (e as MoveError).status === 409 &&
      /running a turn/.test((e as Error).message),
  );
  assert.equal(f.db.messagesFor(target.id).length, targetMessages.length);
  f.db.close();
});

Deno.test("an unknown target or source is a 404 and writes nothing", () => {
  const f = fixture();
  const { source, target, sourceMessages } = pair(f);

  assert.throws(
    () =>
      move(f.ctx, "no-such-target", {
        sourceId: source.id,
        picks: [{ messageId: sourceMessages[0].id }],
      }),
    (e: unknown) => e instanceof MoveError && (e as MoveError).status === 404,
  );
  assert.throws(
    () =>
      move(f.ctx, target.id, {
        sourceId: "no-such-source",
        picks: [{ messageId: sourceMessages[0].id }],
      }),
    (e: unknown) => e instanceof MoveError && (e as MoveError).status === 404,
  );
  assert.equal(f.db.messagesFor(target.id).length, 1);
  f.db.close();
});

Deno.test("a pick outside the source's thread is a 400 naming where it lives", () => {
  const f = fixture();
  const { source, target } = pair(f);
  const other = session(f.db, { title: "elsewhere" });
  const stray = text(f.db, other.id, "user", "different work");

  assert.throws(
    () => move(f.ctx, target.id, { sourceId: source.id, picks: [{ messageId: stray.id }] }),
    (e: unknown) =>
      e instanceof MoveError && (e as MoveError).status === 400 &&
      (e as Error).message.includes(other.id),
  );
  assert.equal(f.db.messagesFor(target.id).length, 1);
  f.db.close();
});

// ---- the route ----------------------------------------------------------------

const TABLE: Route[] = [route("POST", "/sessions/:id/move-into", moveIntoH)];

Deno.test("POST /sessions/:id/move-into answers 200 with the target, its thread and a count", async () => {
  const f = fixture();
  const { source, target, sourceMessages } = pair(f);
  const call = createHandler(f.ctx, { routes: TABLE });

  const res = await call(
    new Request(`http://x/sessions/${target.id}/move-into`, {
      method: "POST",
      body: JSON.stringify({
        sourceId: source.id,
        // Duplicate picks of one message merge, so `appended` is 2, not 3 — which is
        // exactly why the count is in the response.
        picks: [
          { messageId: sourceMessages[0].id },
          { messageId: sourceMessages[0].id, parts: [0] },
          { messageId: sourceMessages[1].id },
        ],
      }),
    }),
  );

  // 200, not 201: this history operation creates no session.
  assert.equal(res.status, 200);
  const body = await res.json() as { session: Session; thread: Message[]; appended: number };
  assert.equal(body.session.id, target.id);
  assert.equal(body.appended, 2);
  assert.deepEqual(textsOf(body.thread), [
    "let's write it up",
    "why does the ticker fire twice?",
    "catch-up advances from now, not the stale value",
  ]);
  f.db.close();
});

Deno.test("the route maps an unknown target to 404, self-move to 400, a bad body to 400", async () => {
  const f = fixture();
  const { source, target, sourceMessages } = pair(f);
  const call = createHandler(f.ctx, { routes: TABLE });
  const post = (id: string, body: unknown) =>
    call(
      new Request(`http://x/sessions/${id}/move-into`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    );

  const missing = await post("no-such-session", {
    sourceId: source.id,
    picks: [{ messageId: sourceMessages[0].id }],
  });
  assert.equal(missing.status, 404);

  const self = await post(target.id, {
    sourceId: target.id,
    picks: [{ messageId: sourceMessages[0].id }],
  });
  assert.equal(self.status, 400);
  assert.match((await self.json()).error, /source and target are both/);

  // No `sourceId` at all is the schema's 400, not the domain's.
  const malformed = await post(target.id, { picks: [{ messageId: sourceMessages[0].id }] });
  assert.equal(malformed.status, 400);
  assert.match((await malformed.json()).error, /invalid body/);
  f.db.close();
});
