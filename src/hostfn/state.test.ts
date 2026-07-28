/**
 * `state.*` is a thin shell over four `Db` accessors, so almost none of these tests
 * are about get-then-get-it-back. They are about the two things that are easy to get
 * wrong and impossible to notice afterwards:
 *
 *   - **Scope.** The acceptance criterion (plan T6.2): a fork and its parent read the
 *     SAME store. A per-session scope passes every single-session test and fails the
 *     first time anyone forks, silently, by handing the branch an empty store. The
 *     subagent case is here too, because a subagent's `parentId` is null and a naive
 *     parent-walk therefore makes it its own root.
 *   - **The cap.** 16KB per value, hard, and *nothing stored* when it trips — a
 *     partially-written note is a wrong note.
 *
 * Hermetic and offline: every test owns an in-memory database, the clock is injected,
 * and nothing here reads `~/.bough`, binds a socket, or spawns a worker.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * denied by this environment's egress policy, so the jsr import declared in
 * `deno.json` cannot resolve. `node:assert` is built into the runtime and needs no
 * fetch. (Same constraint `db.test.ts` and `patch.test.ts` document.)
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { SqliteDb } from "../db/db.ts";
import { StateError } from "../errors.ts";
import type { Session } from "../schema/parts.ts";
import type { Db, TurnCtx } from "../types.ts";
import { createStateHostFn, lineageRoot, MAX_KEYS, MAX_VALUE_BYTES, stateVerb } from "./state.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

function mem(): SqliteDb {
  return new SqliteDb(":memory:", {});
}

function session(id: string, over: Partial<Session> = {}): Session {
  return { id, parentId: null, title: id, kind: "root", createdAt: 1_000, ...over };
}

/** A frozen clock, so `updatedAt` is a value a test can assert on. */
const at = (t: number) => () => t;

/**
 * The error a call threw. `node:assert`'s own `throws` is typed `void`, so it cannot
 * hand back the error — and the error text IS the thing under test here (spec §6:
 * a message that says only "failed" is a defect).
 */
function caught(fn: () => unknown): Error {
  try {
    fn();
  } catch (err) {
    return err as Error;
  }
  throw new assert.AssertionError({ message: "expected a throw, got none" });
}

/**
 * A `TurnCtx` with only the fields `state.*` touches. The rest of the context is
 * genuinely unused here, which is the point of the module boundary: a host function
 * takes a ctx and nothing else, so no server exists in this file.
 */
function turnCtx(db: Db, sessionId: string, now = at(5_000)): TurnCtx {
  return {
    db,
    bus: { publish: (e) => ({ ...e, seq: 1, ts: 0 }), subscribe: () => () => {}, size: 0 },
    sessionId,
    turnId: "t1",
    messageId: "m1",
    workspace: "/tmp",
    model: "test-model",
    signal: new AbortController().signal,
    depth: 0,
    now,
  } as TurnCtx;
}

/** The host fn's JSON-in/JSON-out call, as the worker makes it. */
async function call(
  fns: ReturnType<typeof createStateHostFn>,
  verb: string,
  args?: unknown,
): Promise<unknown> {
  const out = await fns.state!(verb, JSON.stringify(args ?? null));
  return JSON.parse(out);
}

// ---------------------------------------------------------------------------
// the verbs
// ---------------------------------------------------------------------------

test("get / set / list / delete round-trip any JSON", () => {
  const db = mem();
  assert.equal(stateVerb(db, "root", "get", "todo"), null);

  const set = stateVerb(db, "root", "set", {
    key: "todo",
    value: { left: ["a.ts", "b.ts"], done: 3, ok: false },
  }, at(1_000)) as { ok: boolean; key: string; bytes: number };
  assert.equal(set.ok, true);
  assert.equal(set.key, "todo");
  assert.ok(set.bytes > 0);

  assert.deepEqual(stateVerb(db, "root", "get", "todo"), {
    left: ["a.ts", "b.ts"],
    done: 3,
    ok: false,
  });

  // list gives keys and sizes only — a listing must never drag whole values back
  // into the context this store exists to spare.
  const list = stateVerb(db, "root", "list", null) as { key: string; updatedAt: number }[];
  assert.deepEqual(list.map((e) => e.key), ["todo"]);
  assert.equal(list[0].updatedAt, 1_000);
  assert.equal(Object.keys(list[0]).sort().join(","), "bytes,key,updatedAt");

  // Deleting twice is not an error: "there was none" is an answer.
  assert.deepEqual(stateVerb(db, "root", "delete", "todo"), {
    ok: true,
    key: "todo",
    removed: true,
  });
  assert.deepEqual(stateVerb(db, "root", "delete", "todo"), {
    ok: true,
    key: "todo",
    removed: false,
  });
  assert.equal(stateVerb(db, "root", "get", "todo"), null);
});

test("an unset key reads as null, not an error — `?? default` is the idiom", () => {
  const db = mem();
  assert.equal(stateVerb(db, "root", "get", "never-written"), null);
  // A stored null is indistinguishable from unset, and that is fine: both mean
  // "nothing useful here".
  stateVerb(db, "root", "set", { key: "k", value: null });
  assert.equal(stateVerb(db, "root", "get", "k"), null);
  // …but the key exists, which `list` shows and `delete` confirms.
  assert.deepEqual((stateVerb(db, "root", "list", null) as { key: string }[]).map((e) => e.key), [
    "k",
  ]);
});

test("set re-set overwrites in place and re-stamps updatedAt", () => {
  const db = mem();
  stateVerb(db, "root", "set", { key: "k", value: 1 }, at(1_000));
  stateVerb(db, "root", "set", { key: "k", value: 2 }, at(2_000));
  assert.equal(stateVerb(db, "root", "get", "k"), 2);
  const list = stateVerb(db, "root", "list", null) as { key: string; updatedAt: number }[];
  assert.equal(list.length, 1);
  assert.equal(list[0].updatedAt, 2_000);
});

test("two roots keep separate stores", () => {
  const db = mem();
  stateVerb(db, "a", "set", { key: "k", value: 1 });
  stateVerb(db, "b", "set", { key: "k", value: 2 });
  stateVerb(db, "a", "set", { key: "k", value: 3 });
  assert.equal(stateVerb(db, "a", "get", "k"), 3);
  assert.equal(stateVerb(db, "b", "get", "k"), 2);
  assert.equal((stateVerb(db, "a", "list", null) as unknown[]).length, 1);
});

// ---------------------------------------------------------------------------
// scope — the acceptance criterion
// ---------------------------------------------------------------------------

test("AC: a fork and its parent read the SAME store", async () => {
  const db = mem();
  db.createSession(session("root1"));
  // A fork is parented at the target's parent, so it shares every ancestor.
  db.createSession(session("fork1", {
    kind: "fork",
    parentId: "root1",
    originId: "root1",
  }));

  const parent = createStateHostFn(turnCtx(db, "root1"));
  const fork = createStateHostFn(turnCtx(db, "fork1"));

  await call(parent, "set", { key: "ported", value: ["a.ts"] });
  // The fork sees what the parent wrote…
  assert.deepEqual(await call(fork, "get", "ported"), ["a.ts"]);
  // …and writing from the fork is visible to the parent. One store, one lineage.
  await call(fork, "set", { key: "ported", value: ["a.ts", "b.ts"] });
  assert.deepEqual(await call(parent, "get", "ported"), ["a.ts", "b.ts"]);

  // Both resolve to the same scope, which is what makes the above true rather than
  // a coincidence of two writes landing in two stores that happen to agree.
  assert.equal(lineageRoot(db, "fork1"), "root1");
  assert.equal(lineageRoot(db, "root1"), "root1");
});

test("a compaction child and a deep fork chain resolve to the same root", () => {
  const db = mem();
  db.createSession(session("root1"));
  db.createSession(session("f1", { kind: "fork", parentId: "root1", originId: "root1" }));
  db.createSession(session("c1", { kind: "compaction", parentId: "f1", originId: "f1" }));
  assert.equal(lineageRoot(db, "c1"), "root1");
});

test("a subagent shares its spawner's store — parentId is null, originId is not", async () => {
  const db = mem();
  db.createSession(session("root1"));
  // What `agents/subagent.ts` creates: a fresh, task-only thread (`parentId: null`)
  // whose only link upward is the lineage edge.
  db.createSession(session("sub1", {
    kind: "subagent",
    parentId: null,
    originId: "root1",
  }));

  assert.equal(lineageRoot(db, "sub1"), "root1");

  const spawner = createStateHostFn(turnCtx(db, "root1"));
  const child = createStateHostFn(turnCtx(db, "sub1"));
  await call(spawner, "set", { key: "plan", value: "port files 1-40" });
  assert.equal(await call(child, "get", "plan"), "port files 1-40");
});

test("a workflow agent, and a subagent of a fork, both reach the lineage root", () => {
  const db = mem();
  db.createSession(session("root1"));
  db.createSession(session("f1", { kind: "fork", parentId: "root1", originId: "root1" }));
  db.createSession(session("sub1", { kind: "subagent", parentId: null, originId: "f1" }));
  db.createSession(session("wa1", { kind: "workflow_agent", parentId: null, originId: "f1" }));
  assert.equal(lineageRoot(db, "sub1"), "root1");
  assert.equal(lineageRoot(db, "wa1"), "root1");
});

test("lineageRoot survives a cycle and an unknown session", () => {
  const db = mem();
  // A session whose origin points back at a descendant: a bad write, not a shape the
  // system creates. It must terminate, not hang every state call in the process.
  db.createSession(session("x", { kind: "subagent", parentId: null, originId: "y" }));
  db.createSession(session("y", { kind: "subagent", parentId: null, originId: "x" }));
  const root = lineageRoot(db, "x");
  assert.ok(root === "x" || root === "y");
  // An unknown session is its own root — the only answer available.
  assert.equal(lineageRoot(db, "nobody"), "nobody");
});

// ---------------------------------------------------------------------------
// caps — the other acceptance criterion
// ---------------------------------------------------------------------------

test("AC: a value over 16KB is rejected, and nothing is stored", () => {
  const db = mem();
  const oversized = "x".repeat(MAX_VALUE_BYTES); // + 2 quote bytes once serialized
  const err = caught(() => stateVerb(db, "root", "set", { key: "log", value: oversized }));
  assert.ok(err instanceof StateError);
  assert.equal(err.status, 400);
  assert.match(err.message, /too large/);
  assert.match(err.message, new RegExp(String(MAX_VALUE_BYTES)));
  // The message must say what to do instead, not merely that it failed (spec §6).
  assert.match(err.message, /file/);
  // Rejected, never truncated: a shortened note is a wrong note.
  assert.equal(stateVerb(db, "root", "get", "log"), null);
  assert.deepEqual(stateVerb(db, "root", "list", null), []);
});

test("the cap is on BYTES, so a value just under it still lands", () => {
  const db = mem();
  // JSON adds the two quotes, so this serializes to exactly MAX_VALUE_BYTES.
  const exact = "y".repeat(MAX_VALUE_BYTES - 2);
  const ok = stateVerb(db, "root", "set", { key: "k", value: exact }) as { bytes: number };
  assert.equal(ok.bytes, MAX_VALUE_BYTES);
  assert.equal(stateVerb(db, "root", "get", "k"), exact);
  // One more character is one byte too many.
  assert.throws(
    () => stateVerb(db, "root", "set", { key: "k2", value: exact + "y" }),
    StateError,
  );
  // Multi-byte characters count as bytes, not as characters.
  assert.throws(
    () => stateVerb(db, "root", "set", { key: "k3", value: "é".repeat(MAX_VALUE_BYTES - 1) }),
    StateError,
  );
});

test("the key cap refuses a 201st key but still lets an existing one be rewritten", () => {
  const db = mem();
  for (let i = 0; i < MAX_KEYS; i++) stateVerb(db, "root", "set", { key: `k${i}`, value: i });
  const err = caught(() => stateVerb(db, "root", "set", { key: "one-too-many", value: 1 }));
  assert.ok(err instanceof StateError);
  assert.match(err.message, /too many keys/);
  assert.match(err.message, /state\.delete/);
  // A lineage at the cap must still be able to correct itself, or it is bricked.
  stateVerb(db, "root", "set", { key: "k0", value: "rewritten" });
  assert.equal(stateVerb(db, "root", "get", "k0"), "rewritten");
  // …and freeing a slot lets a new key in.
  stateVerb(db, "root", "delete", "k1");
  stateVerb(db, "root", "set", { key: "one-too-many", value: 1 });
  assert.equal(stateVerb(db, "root", "get", "one-too-many"), 1);
});

// ---------------------------------------------------------------------------
// argument errors — the text is a product surface
// ---------------------------------------------------------------------------

test("bad arguments name the verb and the fix", () => {
  const db = mem();
  const empty = caught(() => stateVerb(db, "root", "get", ""));
  assert.ok(empty instanceof StateError);
  assert.match(empty.message, /state\.get/);

  assert.ok(caught(() => stateVerb(db, "root", "get", { key: 42 })) instanceof StateError);
  const long = caught(() => stateVerb(db, "root", "set", { key: "x".repeat(500), value: 1 }));
  assert.ok(long instanceof StateError);
  assert.match(long.message, /key too long/);

  const missing = caught(() => stateVerb(db, "root", "set", { key: "k" }));
  assert.ok(missing instanceof StateError);
  assert.match(missing.message, /value is required/);
  assert.match(missing.message, /state\.delete/);

  // A function is not JSON. `JSON.stringify` reports it by returning nothing.
  assert.ok(
    caught(() => stateVerb(db, "root", "set", { key: "k", value: () => 1 })) instanceof StateError,
  );
  // A cycle is not JSON either, and the message says so rather than throwing raw.
  const cyclic: Record<string, unknown> = {};
  cyclic.self = cyclic;
  assert.ok(
    caught(() => stateVerb(db, "root", "set", { key: "k", value: cyclic })) instanceof StateError,
  );

  const unknown = caught(() => stateVerb(db, "root", "nope", null));
  assert.ok(unknown instanceof StateError);
  assert.match(unknown.message, /unknown verb/);
  assert.match(unknown.message, /get, set, list, delete/);
});

test("a row that is not JSON is reported, not thrown raw at the program", () => {
  const db = mem();
  db.setState("root", "corrupt", "{not json", 1_000);
  const err = caught(() => stateVerb(db, "root", "get", "corrupt"));
  assert.ok(err instanceof StateError);
  assert.equal(err.status, 500);
  assert.match(err.message, /not valid JSON/);
});

// ---------------------------------------------------------------------------
// the bridge
// ---------------------------------------------------------------------------

test("the host fn is string-in/string-out and an unset key comes back as `null`", async () => {
  const db = mem();
  db.createSession(session("s1"));
  const fns = createStateHostFn(turnCtx(db, "s1"));

  // The worker sends `JSON.stringify(args)` and re-inflates the reply, so an unset
  // key must be the four characters `null` — an empty string would be a parse error
  // inside the worker, which the program would see as a broken host function.
  assert.equal(await fns.state!("get", JSON.stringify("nope")), "null");
  assert.equal(await fns.state!("list", "null"), "[]");

  await fns.state!("set", JSON.stringify({ key: "k", value: { a: 1 } }));
  assert.equal(await fns.state!("get", JSON.stringify("k")), '{"a":1}');

  // `state.list()` sends no arguments at all in some shapes; an empty string must
  // not be a parse failure.
  assert.equal(await fns.state!("list", ""), '[{"key":"k","bytes":7,"updatedAt":5000}]');
});

test("the host fn rejects rather than throwing junk at the program", async () => {
  const db = mem();
  db.createSession(session("s1"));
  const fns = createStateHostFn(turnCtx(db, "s1"));
  await assert.rejects(() => fns.state!("get", "{not json"), StateError);
  await assert.rejects(() => fns.state!("frobnicate", "null"), StateError);
});

test("the injected clock is used, and rootId can be pinned", async () => {
  const db = mem();
  const fns = createStateHostFn(turnCtx(db, "whatever"), { rootId: "pinned", now: at(777) });
  await call(fns, "set", { key: "k", value: 1 });
  assert.equal(stateVerb(db, "pinned", "get", "k"), 1);
  const list = stateVerb(db, "pinned", "list", null) as { updatedAt: number }[];
  assert.equal(list[0].updatedAt, 777);
});
