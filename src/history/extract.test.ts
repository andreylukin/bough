/**
 * Extract, with the two claims the AC names as load-bearing:
 *
 *   1. An ANCESTOR message can be extracted — and the same message is a 400 for fork.
 *      Both halves are asserted in one test, because "extract reaches further than fork"
 *      is the whole reason the operation exists and an assertion that only showed the
 *      copy landing would still pass against an implementation that resolved picks
 *      against `messagesFor` in a session whose ancestor happened to be empty.
 *   2. A PART-LEVEL pick copies a turn's prose without its tool calls — the copy carries
 *      exactly the picked indexes and nothing else.
 *
 * Plus the invariant every history operation shares: the source (and here, its ancestor)
 * is JSON-identical afterwards. Asserted by snapshotting the row and every message to
 * JSON before the call and comparing after, rather than spot-checking a field — a spot
 * check passes against an implementation that helpfully "tidies" a part it copied, which
 * is exactly the class of bug "never mutates" exists to forbid.
 *
 * Everything runs offline: an in-memory database and a real bus, no LLM at all (extract
 * copies, it does not summarize). Assertions come from `node:assert/strict` — jsr.io is
 * unreachable here, so `@std/assert` cannot resolve (plan §7).
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { ExtractError, ForkError } from "../errors.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { createHandler, type Route, route } from "../server/app.ts";
import { extract, type ExtractCtx, extractH } from "./extract.ts";
import { fork } from "./fork.ts";

// ---- fixtures ---------------------------------------------------------------

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  ctx: ExtractCtx & AppCtx;
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

/** The texts of a message list — the readable shape of "what landed, in what order". */
function textsOf(messages: readonly Message[]): string[] {
  return messages.map((m) =>
    m.parts.filter((p): p is Extract<Part, { type: "text" }> => p.type === "text")
      .map((p) => p.text).join(" | ")
  );
}

/** The whole of a session as storage holds it — the byte-unchanged snapshot. */
function snapshot(db: SqliteDb, sessionId: string): string {
  return JSON.stringify({
    session: db.getSession(sessionId),
    messages: db.messagesFor(sessionId),
  });
}

/**
 * A parent session and a child whose thread inherits it — the shape that makes the
 * ancestor claim meaningful. The child is a `fork` so the comparison against `fork()`
 * is on the operation a user would actually be running.
 */
function lineage(f: Fixture): {
  parent: Session;
  child: Session;
  parentMessages: Message[];
  childMessages: Message[];
} {
  const parent = session(f.db, {
    title: "the original work",
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
    base: "abc123",
  });
  const parentMessages = [
    text(f.db, parent.id, "user", "how does the parser handle comments?"),
    text(f.db, parent.id, "supervisor", "it skips them in the balanced-brace scan"),
  ];
  const child = session(f.db, {
    title: "fork · the original work",
    kind: "fork",
    parentId: parent.id,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
    base: "abc123",
  });
  const childMessages = [
    text(f.db, child.id, "user", "now add nested template literals"),
    text(f.db, child.id, "supervisor", "done — meta.ts handles them"),
  ];
  return { parent, child, parentMessages, childMessages };
}

// ---- AC 1: an ancestor message, which fork cannot touch ---------------------

Deno.test("extract copies an ANCESTOR's message — the thing fork refuses", () => {
  const f = fixture();
  const { parent, child, parentMessages, childMessages } = lineage(f);
  const ancestor = parentMessages[1];

  // The message IS in the child's visible thread: that is the whole premise. A user
  // looking at the child's transcript can see it and select it.
  assert.ok(f.db.threadFor(child.id).some((m) => m.id === ancestor.id));

  // Fork refuses it, naming the ancestor — a fork reconstructs its thread through the
  // parent chain, so it cannot cut a row it does not own (spec §14).
  assert.throws(
    () => fork(f.ctx, child.id, { atMessageId: ancestor.id }),
    (e: unknown) =>
      e instanceof ForkError && /belongs to ancestor session/.test((e as Error).message),
  );

  // Extract takes it, together with one of the child's own — a selection spanning the
  // inheritance boundary, which is the case the operation exists for.
  const { session: root, messages } = extract(f.ctx, child.id, {
    picks: [{ messageId: ancestor.id }, { messageId: childMessages[1].id }],
  });

  assert.equal(root.kind, "root");
  // A ROOT: it inherits nothing, which is what lets it carry an ancestor's turn
  // without carrying the ancestor.
  assert.equal(root.parentId, null);
  assert.deepEqual(textsOf(messages), [
    "it skips them in the balanced-brace scan",
    "done — meta.ts handles them",
  ]);
  // The new session's THREAD is exactly its own messages — nothing inherited.
  assert.deepEqual(f.db.threadFor(root.id).map((m) => m.id), messages.map((m) => m.id));

  // Copies, not moves: fresh ids, and the originals are still where they were.
  assert.notEqual(messages[0].id, ancestor.id);
  assert.equal(f.db.getMessage(ancestor.id)?.sessionId, parent.id);
  assert.equal(f.db.messagesFor(parent.id).length, 2);
  assert.equal(f.db.messagesFor(child.id).length, 2);

  // Lineage points at the session extracted FROM and the last picked message, so the
  // tree can draw the edge even though nothing is inherited through it.
  assert.equal(root.originId, child.id);
  assert.equal(root.originMessageId, childMessages[1].id);
  f.db.close();
});

Deno.test("the extracted root keeps the source's workspace, base, originDir and pins", () => {
  const f = fixture();
  const { child, childMessages } = lineage(f);
  f.db.setSessionModel(child.id, "openai:gpt-5");
  f.db.setSessionEffort(child.id, "high");

  const { session: root } = extract(f.ctx, child.id, {
    picks: [{ messageId: childMessages[0].id }],
  });

  // The same checkout, worked in place — with the sha its change set is measured from.
  assert.equal(root.workspace, "/tmp/checkout");
  assert.equal(root.base, "abc123");
  assert.equal(root.originDir, "/tmp/checkout");
  // A model id is a provider routing decision: the extracted conversation must not
  // silently move to another vendor's default.
  assert.equal(root.model, "openai:gpt-5");
  assert.equal(root.effort, "high");
  // Read back, not just echoed.
  assert.equal(f.db.getSession(root.id)?.model, "openai:gpt-5");

  // Titled off the BASE title: extracting a fork must not compound into
  // "extract · fork · X".
  assert.equal(root.title, "extract · the original work");
  f.db.close();
});

// ---- AC 2: part-level picks -------------------------------------------------

Deno.test("a part-level pick copies a turn's prose without its tool calls", () => {
  const f = fixture();
  const source = session(f.db, { title: "the work" });
  text(f.db, source.id, "user", "find the retry bound");
  const turn = message(f.db, source.id, "supervisor", [
    { type: "reasoning", text: "check the runner first" },
    { type: "text", text: "Retries are bounded at 3 in turn/runner.ts." },
    { type: "tool_call", id: "t1", name: "run_steps", input: { code: "await bash('rg retry')" } },
    { type: "tool_result", callId: "t1", isError: false, output: "runner.ts:148: MAX_RETRIES = 3" },
    { type: "text", text: "An exhausted retry surfaces as a turn error." },
  ]);

  const { session: root, messages } = extract(f.ctx, source.id, {
    // The two prose parts only — indexes 1 and 4.
    picks: [{ messageId: turn.id, parts: [1, 4] }],
  });

  assert.equal(messages.length, 1);
  const copied = messages[0];
  assert.equal(copied.role, "supervisor");
  assert.deepEqual(copied.parts, [
    { type: "text", text: "Retries are bounded at 3 in turn/runner.ts." },
    { type: "text", text: "An exhausted retry surfaces as a turn error." },
  ]);
  // No tool call, no tool result, no reasoning came along.
  assert.equal(copied.parts.some((p) => p.type === "tool_call"), false);
  assert.equal(copied.parts.some((p) => p.type === "tool_result"), false);
  // What storage kept, not just what was returned.
  assert.deepEqual(f.db.messagesFor(root.id)[0].parts, copied.parts);

  // The original turn still has all five of its parts.
  assert.equal(f.db.getMessage(turn.id)?.parts.length, 5);
  f.db.close();
});

Deno.test("the copied parts are a deep copy — mutating one cannot reach the other", () => {
  const f = fixture();
  const source = session(f.db, { title: "the work" });
  const turn = message(f.db, source.id, "supervisor", [
    { type: "tool_call", id: "t1", name: "run_steps", input: { code: "original" } },
  ]);

  const { messages } = extract(f.ctx, source.id, { picks: [{ messageId: turn.id }] });
  const part = messages[0].parts[0] as Extract<Part, { type: "tool_call" }>;
  (part.input as { code: string }).code = "rewritten";

  const original = f.db.getMessage(turn.id)!.parts[0] as Extract<Part, { type: "tool_call" }>;
  assert.deepEqual(original.input, { code: "original" });
  f.db.close();
});

// ---- selection semantics ----------------------------------------------------

Deno.test("picks are copied in THREAD order, whatever order they were selected in", () => {
  const f = fixture();
  const { parentMessages, child, childMessages } = lineage(f);

  const { messages } = extract(f.ctx, child.id, {
    // Sent bottom-up and interleaved across the inheritance boundary, as a user
    // shift-clicking upward would send it.
    picks: [
      { messageId: childMessages[1].id },
      { messageId: parentMessages[0].id },
      { messageId: childMessages[0].id },
    ],
  });

  assert.deepEqual(textsOf(messages), [
    "how does the parser handle comments?",
    "now add nested template literals",
    "done — meta.ts handles them",
  ]);
  f.db.close();
});

Deno.test("a whole-message pick wins over a partial one for the same message", () => {
  const f = fixture();
  const source = session(f.db, { title: "the work" });
  const turn = message(f.db, source.id, "supervisor", [
    { type: "text", text: "one" },
    { type: "text", text: "two" },
  ]);

  const { messages } = extract(f.ctx, source.id, {
    picks: [{ messageId: turn.id, parts: [0] }, { messageId: turn.id }],
  });

  assert.equal(messages.length, 1);
  assert.deepEqual(textsOf(messages), ["one | two"]);
  f.db.close();
});

// ---- the source is untouched -------------------------------------------------

Deno.test("extract leaves the source AND its ancestor byte-identical", () => {
  const f = fixture();
  const { parent, child, parentMessages, childMessages } = lineage(f);
  const before = [snapshot(f.db, parent.id), snapshot(f.db, child.id)];

  extract(f.ctx, child.id, {
    picks: [
      { messageId: parentMessages[0].id },
      { messageId: parentMessages[1].id, parts: [0] },
      { messageId: childMessages[0].id },
    ],
  });

  assert.deepEqual([snapshot(f.db, parent.id), snapshot(f.db, child.id)], before);
  f.db.close();
});

// ---- events ------------------------------------------------------------------

Deno.test("the new root is announced before the copies that go into it", () => {
  const f = fixture();
  const { child, childMessages } = lineage(f);
  f.events.length = 0;

  const { session: root } = extract(f.ctx, child.id, {
    picks: childMessages.map((m) => ({ messageId: m.id })),
  });

  assert.deepEqual(f.events.map((e) => e.type), [
    "session.created",
    "message.started",
    "message.started",
  ]);
  // A `message.started` for a session the client has never heard of is a message it
  // has nowhere to put — hence the order.
  assert.equal(f.events[0].sessionId, root.id);
  assert.ok(f.events.every((e) => e.sessionId === root.id));
  f.db.close();
});

// ---- refusals ----------------------------------------------------------------

Deno.test("an unknown session is a 404 and writes nothing", () => {
  const f = fixture();
  const { childMessages } = lineage(f);
  const sessionsBefore = f.db.listSessions().length;

  assert.throws(
    () => extract(f.ctx, "no-such-session", { picks: [{ messageId: childMessages[0].id }] }),
    (e: unknown) => e instanceof ExtractError && (e as ExtractError).status === 404,
  );
  assert.equal(f.db.listSessions().length, sessionsBefore);
  f.db.close();
});

Deno.test("a message from outside the thread is a 400 naming where it lives", () => {
  const f = fixture();
  const { child } = lineage(f);
  // A sibling branch: real message, real session, not in this session's thread.
  const other = session(f.db, { title: "unrelated" });
  const stray = text(f.db, other.id, "user", "different work entirely");
  const sessionsBefore = f.db.listSessions().length;

  assert.throws(
    () => extract(f.ctx, child.id, { picks: [{ messageId: stray.id }] }),
    (e: unknown) =>
      e instanceof ExtractError && (e as ExtractError).status === 400 &&
      (e as Error).message.includes(other.id),
  );
  // Validation runs before the branch opens, so a bad pick leaves no empty root
  // behind in the user's session list.
  assert.equal(f.db.listSessions().length, sessionsBefore);
  f.db.close();
});

Deno.test("a nonexistent message id, and a part index out of range, are both 400", () => {
  const f = fixture();
  const source = session(f.db, { title: "the work" });
  const turn = text(f.db, source.id, "user", "only one part here");

  assert.throws(
    () => extract(f.ctx, source.id, { picks: [{ messageId: "nope" }] }),
    (e: unknown) =>
      e instanceof ExtractError && /no message nope exists/.test((e as Error).message),
  );
  assert.throws(
    () => extract(f.ctx, source.id, { picks: [{ messageId: turn.id, parts: [3] }] }),
    (e: unknown) =>
      e instanceof ExtractError && /part index out of range/.test((e as Error).message),
  );
  f.db.close();
});

Deno.test("a session with an empty thread has nothing to extract", () => {
  const f = fixture();
  const empty = session(f.db, { title: "brand new" });
  assert.throws(
    () => extract(f.ctx, empty.id, { picks: [{ messageId: "anything" }] }),
    (e: unknown) => e instanceof ExtractError && /empty thread/.test((e as Error).message),
  );
  f.db.close();
});

// ---- the route ----------------------------------------------------------------

const TABLE: Route[] = [route("POST", "/sessions/:id/extract", extractH)];

Deno.test("POST /sessions/:id/extract answers 201 with the new root and its thread", async () => {
  const f = fixture();
  const { parentMessages, child, childMessages } = lineage(f);
  const call = createHandler(f.ctx, { routes: TABLE });

  const res = await call(
    new Request(`http://x/sessions/${child.id}/extract`, {
      method: "POST",
      body: JSON.stringify({
        picks: [{ messageId: parentMessages[1].id }, { messageId: childMessages[0].id }],
      }),
    }),
  );

  assert.equal(res.status, 201);
  const body = await res.json() as { session: Session; thread: Message[] };
  assert.equal(body.session.kind, "root");
  assert.equal(body.session.parentId, null);
  // The thread rides along so the client can render the root it is switching to
  // without an immediate second fetch.
  assert.deepEqual(textsOf(body.thread), [
    "it skips them in the balanced-brace scan",
    "now add nested template literals",
  ]);
  f.db.close();
});

Deno.test("the route maps an unknown session to 404 and a bad body to 400", async () => {
  const f = fixture();
  const { child, childMessages } = lineage(f);
  const call = createHandler(f.ctx, { routes: TABLE });
  const post = (id: string, body: unknown) =>
    call(
      new Request(`http://x/sessions/${id}/extract`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    );

  const missing = await post("no-such-session", { picks: [{ messageId: childMessages[0].id }] });
  assert.equal(missing.status, 404);

  // An empty selection is the schema's 400, not the domain's.
  const empty = await post(child.id, { picks: [] });
  assert.equal(empty.status, 400);
  assert.match((await empty.json()).error, /invalid body/);

  const stray = await post(child.id, { picks: [{ messageId: "nope" }] });
  assert.equal(stray.status, 400);
  assert.match((await stray.json()).error, /no message nope exists/);
  f.db.close();
});
