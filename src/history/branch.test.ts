/**
 * Branch seeding, with the ordering invariant (plan §6.1) as the load-bearing test.
 *
 * The AC test is deliberately not a unit test of `add()`. It seeds a branch and then
 * runs a REAL turn on it through `turn/runner.ts` — with every write in the scenario
 * pinned to the same millisecond by an injected clock — because the only version of the
 * claim worth having is "the transcript the runner and the seeder wrote together comes
 * back in the right order". A test that asserted `createdAt` values would still pass
 * against a seeder that stamped `base + i` and put the fork's own turn in the middle of
 * its history.
 *
 * The turn is driven by a scripted fake `LlmClient` and a fake program runner: nothing
 * here touches the network, binds a socket, or writes outside an in-memory database
 * (plan §7). Assertions come from `node:assert/strict` — jsr.io is unreachable here, so
 * a test that needs `@std/assert` cannot run offline.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx, LlmClient, LlmResult } from "../types.ts";
import { beginTurn, RUN_STEPS, STOP } from "../turn/runner.ts";
import { TurnRegistry } from "../turn/queue.ts";
import {
  baseTitle,
  type BranchCtx,
  mergePicks,
  openBranch,
  pickParts,
  resolvePicks,
  Seeder,
} from "./branch.ts";

// ---- fixtures ---------------------------------------------------------------

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  ctx: BranchCtx & AppCtx;
  /** Reads whatever `now` is set to; the tests move this, never the seeder. */
  clock: { now: number; calls: number };
}

function fixture(): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const clock = { now: 1_700_000_000_000, calls: 0 };
  const ctx = {
    db,
    bus,
    now: () => {
      clock.calls++;
      return clock.now;
    },
  };
  return { db, bus, events, ctx, clock };
}

function session(
  db: SqliteDb,
  over: Partial<Session> & { title: string },
): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    ...over,
  });
}

function message(
  db: SqliteDb,
  sessionId: string,
  role: Message["role"],
  text: string,
  createdAt: number,
): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role,
    parts: [{ type: "text", text }],
    pending: false,
    createdAt,
  });
}

/** The text of every part of a message, joined — enough to identify a copy. */
function textOf(m: Message): string {
  return m.parts.map((p) => ("text" in p ? p.text : `<${p.type}>`)).join("|");
}

function textsOf(messages: Message[]): string[] {
  return messages.map(textOf);
}

/** A model that narrates once and stops, in the same response (spec §5). */
function oneRoundLlm(said: string): LlmClient {
  return {
    run(): Promise<LlmResult> {
      return Promise.resolve({
        content: [
          { type: "text", text: said },
          { type: "tool_use", id: "stop-1", name: STOP, input: {} },
        ],
        stopReason: "tool_use",
      });
    },
  };
}

/** Turn deps that keep the runner entirely off the network and out of a worker. */
function turnDeps(now: () => number) {
  return {
    registry: new TurnRegistry(),
    now,
    assemble: () => ({ system: "SYSTEM", systemVolatile: "", sections: [] }),
    program: () => Promise.resolve({ ok: true as const, logs: [] }),
    outageDelayMs: 0,
    reportError: (err: unknown) => {
      throw err;
    },
  };
}

// ---- the ordering invariant (plan §6.1) --------------------------------------

Deno.test("a seeded branch and the turn that follows it order correctly in one millisecond", async () => {
  const f = fixture();

  // A parent with shared history, and the session about to be forked.
  const parent = session(f.db, { title: "parent" });
  message(f.db, parent.id, "user", "ancestor question", 1_100);
  message(f.db, parent.id, "supervisor", "ancestor answer", 1_101);
  const target = session(f.db, { title: "target", parentId: parent.id });
  message(f.db, target.id, "user", "own question", 1_200);
  message(f.db, target.id, "supervisor", "own answer", 1_201);
  message(f.db, target.id, "user", "the turn being forked away from", 1_202);

  // Every write from here on lands in the SAME millisecond: the seed, the user
  // message, and the supervisor message the runner creates. `rowid` is the only
  // thing that can order them, which is exactly the case the invariant is about.
  const ms = 1_700_000_000_777;
  f.clock.now = ms;

  const own = f.db.messagesFor(target.id);
  const seeder = openBranch(f.ctx, {
    // Thread-through-parents: parented at the TARGET'S PARENT, so the ancestors are
    // inherited rather than copied.
    parentId: target.parentId,
    title: `fork · ${baseTitle(target.title)}`,
    kind: "fork",
    originId: target.id,
    originMessageId: own[2].id,
  });
  for (const m of own.slice(0, 2)) seeder.copy(m);

  // …and immediately, a real turn on the branch — fork's "edit & resend".
  const branchId = seeder.session.id;
  f.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: branchId,
    role: "user",
    parts: [{ type: "text", text: "edited question" }],
    pending: false,
    createdAt: ms,
  });
  const appCtx: AppCtx = { db: f.db, bus: f.bus, llm: oneRoundLlm("fresh answer"), now: () => ms };
  const { done } = beginTurn(appCtx, branchId, turnDeps(() => ms));
  const outcome = await done;
  assert.equal(outcome.status, "done");

  // ── the branch's own messages, in the order they were written ──
  const branchOwn = f.db.messagesFor(branchId);
  assert.deepEqual(textsOf(branchOwn), [
    "own question",
    "own answer",
    "edited question",
    "fresh answer",
  ]);

  // ── and the case is genuinely the same-millisecond one ──
  assert.deepEqual(
    branchOwn.map((m) => m.createdAt),
    [ms, ms, ms, ms],
    "the seed and the turn must share a millisecond, or this test proves nothing",
  );

  // ── the full thread: inherited ancestors first, then the branch's own ──
  assert.deepEqual(textsOf(f.db.threadFor(branchId)), [
    "ancestor question",
    "ancestor answer",
    "own question",
    "own answer",
    "edited question",
    "fresh answer",
  ]);

  // ── nothing copied the ancestors, and the source is untouched ──
  assert.equal(branchOwn.length, 4);
  assert.deepEqual(textsOf(f.db.messagesFor(target.id)), [
    "own question",
    "own answer",
    "the turn being forked away from",
  ]);
});

Deno.test("the same ordering holds on the real clock, with no injected time at all", async () => {
  const db = openDb(":memory:");
  const bus = new Bus();
  const target = db.createSession({
    id: crypto.randomUUID(),
    title: "target",
    kind: "root",
    createdAt: Date.now(),
    parentId: null,
  });
  message(db, target.id, "user", "one", Date.now());
  message(db, target.id, "supervisor", "two", Date.now());

  // No `now` on the ctx: `Date.now` throughout, seeder and runner alike.
  const ctx: AppCtx = { db, bus, llm: oneRoundLlm("live answer") };
  const seeder = openBranch(ctx, {
    parentId: null,
    title: "fork · target",
    kind: "fork",
    originId: target.id,
  });
  for (const m of db.messagesFor(target.id)) seeder.copy(m);
  db.createMessage({
    id: crypto.randomUUID(),
    sessionId: seeder.session.id,
    role: "user",
    parts: [{ type: "text", text: "three" }],
    pending: false,
    createdAt: Date.now(),
  });
  await beginTurn(ctx, seeder.session.id, turnDeps(Date.now)).done;

  assert.deepEqual(textsOf(db.messagesFor(seeder.session.id)), [
    "one",
    "two",
    "three",
    "live answer",
  ]);
  db.close();
});

Deno.test("the seeder stamps the clock it is handed and never advances it", () => {
  const f = fixture();
  f.clock.now = 42;
  const seeder = openBranch(f.ctx, { parentId: null, title: "b", kind: "root" });
  seeder.add("user", [{ type: "text", text: "a" }]);
  seeder.add("supervisor", [{ type: "text", text: "b" }]);
  seeder.add("user", [{ type: "text", text: "c" }]);

  const stamps = f.db.messagesFor(seeder.session.id).map((m) => m.createdAt);
  assert.deepEqual(stamps, [42, 42, 42], "no per-message increment may be invented");
  assert.equal(seeder.session.createdAt, 42);
  // One read per write, and the value is stored verbatim — nothing is derived from
  // the previous message's stamp.
  assert.equal(f.clock.calls, 4);

  // A clock that moves is followed, not overridden.
  f.clock.now = 99;
  const later = seeder.add("user", [{ type: "text", text: "d" }]);
  assert.equal(later.createdAt, 99);
});

// ---- announcing ---------------------------------------------------------------

Deno.test("the session is announced before its messages, and every seeded message is a message.started", () => {
  const f = fixture();
  const seeder = openBranch(f.ctx, {
    parentId: null,
    title: "extract · thing",
    kind: "root",
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
    base: "abc123",
    originId: "src-1",
    originMessageId: "msg-1",
  });
  const first = seeder.add("user", [{ type: "text", text: "seeded" }]);
  const second = seeder.add("supervisor", [{ type: "text", text: "also seeded" }]);

  assert.deepEqual(f.events.map((e) => e.type), [
    "session.created",
    "message.started",
    "message.started",
  ]);
  const created = f.events[0].data as Session;
  // The event carries what storage kept, not the argument that was passed in.
  assert.deepEqual(created, f.db.getSession(seeder.session.id));
  assert.equal(created.kind, "root");
  assert.equal(created.workspace, "/tmp/checkout");
  assert.equal(created.base, "abc123");
  assert.equal(created.originId, "src-1");
  assert.equal(created.originMessageId, "msg-1");
  assert.equal(f.events[0].sessionId, seeder.session.id);

  assert.deepEqual(f.events[1].data, first);
  assert.deepEqual(f.events[2].data, second);
  // Seeded history is complete on arrival — a pending message would look like a turn
  // that never finished, and nothing exists to close it.
  assert.equal((f.events[1].data as Message).pending, false);
});

Deno.test("lineage fields absent from the spec stay absent from the row", () => {
  const f = fixture();
  const seeder = openBranch(f.ctx, { parentId: null, title: "bare", kind: "root" });
  const stored = f.db.getSession(seeder.session.id)!;
  assert.equal(stored.workspace ?? null, null);
  assert.equal(stored.originId ?? null, null);
  assert.equal(stored.base ?? null, null);
  assert.equal(stored.parentId, null);
});

// ---- copying ------------------------------------------------------------------

Deno.test("copy takes a new id and a deep copy of the parts", () => {
  const f = fixture();
  const source = session(f.db, { title: "source" });
  const parts: Part[] = [
    { type: "text", text: "prose" },
    { type: "tool_call", id: "call-1", name: RUN_STEPS, input: { code: "console.log(1)" } },
    { type: "tool_result", callId: "call-1", output: "1", isError: false },
  ];
  const original = f.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: source.id,
    role: "supervisor",
    parts,
    pending: false,
    createdAt: 5,
  });

  const seeder = openBranch(f.ctx, { parentId: null, title: "b", kind: "root" });
  const copied = seeder.copy(original);

  assert.notEqual(copied.id, original.id);
  assert.equal(copied.sessionId, seeder.session.id);
  assert.equal(copied.role, "supervisor");
  assert.deepEqual(copied.parts, original.parts);

  // The copy shares no structure with the original: mutating either afterwards must
  // not reach the other. History is a tree because nothing is rewritten in place.
  (parts[0] as { text: string }).text = "MUTATED";
  assert.equal(textOf(f.db.getMessage(copied.id)!).split("|")[0], "prose");
  assert.notEqual(copied.parts[1], original.parts[1]);
});

Deno.test("a seeded message is searchable immediately, and a rebuild agrees", () => {
  const f = fixture();
  const seeder = openBranch(f.ctx, { parentId: null, title: "b", kind: "root" });
  seeder.add("user", [{ type: "text", text: "the peculiar zarquon problem" }]);

  const incremental = f.db.searchMessages("zarquon");
  assert.equal(incremental.length, 1);
  assert.equal(incremental[0].sessionId, seeder.session.id);

  f.db.rebuildSearchIndex();
  assert.deepEqual(f.db.searchMessages("zarquon"), incremental);
});

// ---- move-into: a Seeder over an existing session ------------------------------

Deno.test("a Seeder constructed on an existing session appends to it", () => {
  const f = fixture();
  const target = session(f.db, { title: "target" });
  message(f.db, target.id, "user", "already here", 1);
  f.events.length = 0;

  const source = session(f.db, { title: "source" });
  const picked = message(f.db, source.id, "supervisor", "moved in", 2);

  f.clock.now = 7;
  new Seeder(f.ctx, target).copy(picked);

  assert.deepEqual(textsOf(f.db.messagesFor(target.id)), ["already here", "moved in"]);
  // No session is created — only the append is announced.
  assert.deepEqual(f.events.map((e) => e.type), ["message.started"]);
  assert.equal(f.events[0].sessionId, target.id);
  // And the source keeps its turn: this is a copy, never a move.
  assert.deepEqual(textsOf(f.db.messagesFor(source.id)), ["moved in"]);
});

// ---- picks ---------------------------------------------------------------------

Deno.test("mergePicks: a whole-message pick wins, partials union and sort", () => {
  const merged = mergePicks([
    { messageId: "a", parts: [3, 1] },
    { messageId: "a", parts: [2] },
    { messageId: "b", parts: [0] },
    { messageId: "b" },
    { messageId: "c" },
    { messageId: "c", parts: [9] },
  ]);
  assert.deepEqual(merged.get("a"), [1, 2, 3]);
  assert.equal(merged.get("b"), null, "a whole-message pick supersedes an earlier partial");
  assert.equal(merged.get("c"), null, "…and is not narrowed by a later partial");
});

Deno.test("pickParts: null takes everything, an out-of-range index is undefined", () => {
  const m: Message = {
    id: "m",
    sessionId: "s",
    role: "supervisor",
    parts: [
      { type: "text", text: "zero" },
      { type: "reasoning", text: "one" },
      { type: "text", text: "two" },
    ],
    pending: false,
    createdAt: 0,
  };
  assert.deepEqual(pickParts(m, null), m.parts);
  assert.deepEqual(pickParts(m, [0, 2]), [m.parts[0], m.parts[2]]);
  assert.equal(pickParts(m, [3]), undefined);
});

Deno.test("resolvePicks: restores thread order and reports bad picks through the caller's error", () => {
  const f = fixture();
  const s = session(f.db, { title: "s" });
  const a = message(f.db, s.id, "user", "first", 1);
  const b = message(f.db, s.id, "supervisor", "second", 2);
  const c = message(f.db, s.id, "user", "third", 3);
  const thread = f.db.messagesFor(s.id);

  class PickError extends Error {}
  const err = (m: string) => new PickError(m);

  // Sent out of order, as a user selecting upward would: order comes from the thread.
  const resolved = resolvePicks(thread, [
    { messageId: c.id },
    { messageId: a.id },
    { messageId: b.id, parts: [0] },
  ], err);
  assert.deepEqual(resolved.map((r) => r.idx), [0, 1, 2]);
  assert.deepEqual(textsOf(resolved.map((r) => r.view)), ["first", "second", "third"]);

  assert.throws(
    () => resolvePicks(thread, [{ messageId: "not-in-thread" }], err),
    (e: unknown) =>
      e instanceof PickError && /must be messages of the source thread/.test(String(e)),
  );
  assert.throws(
    () => resolvePicks(thread, [{ messageId: a.id, parts: [4] }], err),
    (e: unknown) => e instanceof PickError && String(e).includes(a.id),
  );
});

Deno.test("baseTitle strips accumulated branch prefixes, once", () => {
  assert.equal(baseTitle("fork · fork · rename the router"), "rename the router");
  assert.equal(baseTitle("extract · subagent · handoff · thing"), "thing");
  assert.equal(baseTitle("rename the router"), "rename the router");
  // Only leading prefixes, and only the known ones — a title that merely mentions one
  // keeps it.
  assert.equal(baseTitle("why the fork · thing broke"), "why the fork · thing broke");
  assert.equal(baseTitle("compacted · 3 turns"), "compacted · 3 turns");
});
