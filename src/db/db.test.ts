/**
 * Tests for the database layer.
 *
 * The three that are load-bearing, and why:
 *
 *   - **Same-millisecond ordering.** Invariant 1. Branch seeding writes with a real
 *     clock, so a turn started right after a seed shares its millisecond. If the
 *     `rowid` tie-break were ever dropped from `messagesFor`, history would reorder
 *     under the user and nothing else in the system would notice.
 *   - **Three-level `threadFor`.** Fork and compaction are built on
 *     thread-through-parents: a branch inherits its ancestors' messages and seeds
 *     only its own. A wrong order here corrupts every replayed conversation.
 *   - **Migration idempotence across two opens.** The schema is applied on every
 *     open. If a second open were not a no-op, restarting the server would be
 *     destructive.
 *
 * Hermetic: `:memory:` for everything except the reopen test, which needs a real
 * file and uses a temp directory it removes. Nothing touches `~/.bough`, and no
 * test reads `BOUGH_DB` or `BOUGH_HOME` — every database is opened by explicit path.
 *
 * Assertions come from `node:assert/strict` rather than a matcher library: they run
 * unchanged under any runtime, and a test that cannot run offline does not belong in
 * `bun test` (plan §7).
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Database } from "bun:sqlite";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, SqliteDb } from "./db.ts";
import { migrate, SCHEMA_VERSION, userVersion } from "./migrate.ts";
import type { Message, Session, Turn } from "../schema/parts.ts";
import type { CommandRecord } from "../types.ts";

// ---- fixtures ---------------------------------------------------------------

function mem(now?: () => number): SqliteDb {
  return new SqliteDb(":memory:", now ? { now } : {});
}

function session(id: string, over: Partial<Session> = {}): Session {
  return {
    id,
    parentId: null,
    title: id,
    kind: "root",
    createdAt: 1_000,
    ...over,
  };
}

function message(id: string, sessionId: string, text: string, createdAt: number): Message {
  return {
    id,
    sessionId,
    role: "user",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt,
  };
}

function turn(id: string, sessionId: string, messageId: string, over: Partial<Turn> = {}): Turn {
  return {
    id,
    sessionId,
    messageId,
    status: "running",
    step: "start",
    createdAt: 1_000,
    updatedAt: 1_000,
    ...over,
  };
}

const texts = (ms: Message[]) =>
  ms.map((m) => (m.parts[0].type === "text" ? m.parts[0].text : "?"));

// ---- invariant 1: same-millisecond ordering ---------------------------------

test("messagesFor breaks a created_at tie by insertion order", () => {
  const db = mem();
  db.createSession(session("s"));
  // Everything in the same millisecond: only rowid can order these.
  db.createMessage(message("m1", "s", "first", 5_000));
  db.createMessage(message("m2", "s", "second", 5_000));
  db.createMessage(message("m3", "s", "third", 5_000));
  assert.deepEqual(texts(db.messagesFor("s")), ["first", "second", "third"]);
  db.close();
});

test("a turn started in the same millisecond as a seed sorts after it", () => {
  // The branch-seeding scenario of plan §6.1: openBranch() writes seeded messages
  // with a REAL clock, and the fresh turn posted immediately afterwards lands on the
  // same timestamp. The seed must still come first.
  const db = mem();
  db.createSession(session("branch"));
  const seededAt = Date.now();
  db.createMessage(message("seed1", "branch", "seeded user", seededAt));
  db.createMessage(message("seed2", "branch", "seeded reply", seededAt));
  db.createMessage(message("live", "branch", "the new turn", seededAt));
  assert.deepEqual(texts(db.messagesFor("branch")), [
    "seeded user",
    "seeded reply",
    "the new turn",
  ]);
  db.close();
});

test("created_at still dominates rowid when timestamps differ", () => {
  // The tie-break must not become the primary key: a message inserted later but
  // stamped earlier still sorts earlier.
  const db = mem();
  db.createSession(session("s"));
  db.createMessage(message("late", "s", "later", 9_000));
  db.createMessage(message("early", "s", "earlier", 1_000));
  assert.deepEqual(texts(db.messagesFor("s")), ["earlier", "later"]);
  db.close();
});

// ---- threadFor / ancestorChain ----------------------------------------------

test("threadFor concatenates three levels root -> parent -> own", () => {
  const db = mem();
  db.createSession(session("root", { createdAt: 1 }));
  db.createSession(session("mid", { parentId: "root", kind: "fork", createdAt: 2 }));
  db.createSession(session("leaf", { parentId: "mid", kind: "fork", createdAt: 3 }));

  // Interleaved timestamps on purpose: the thread is grouped by SESSION, root first,
  // and only ordered by time WITHIN a session. Sorting the whole thread by
  // created_at would produce a different — and wrong — answer here.
  db.createMessage(message("r2", "root", "root b", 200));
  db.createMessage(message("r1", "root", "root a", 100));
  db.createMessage(message("m1", "mid", "mid a", 50));
  db.createMessage(message("m2", "mid", "mid b", 400));
  db.createMessage(message("l1", "leaf", "leaf a", 10));
  db.createMessage(message("l2", "leaf", "leaf b", 300));

  assert.deepEqual(texts(db.threadFor("leaf")), [
    "root a",
    "root b",
    "mid a",
    "mid b",
    "leaf a",
    "leaf b",
  ]);
  // A mid-tree read stops at its own messages; a leaf's are not in its parent.
  assert.deepEqual(texts(db.threadFor("mid")), ["root a", "root b", "mid a", "mid b"]);
  assert.deepEqual(texts(db.threadFor("root")), ["root a", "root b"]);
  db.close();
});

test("ancestorChain is root-first and inclusive; unknown ids are empty", () => {
  const db = mem();
  db.createSession(session("root"));
  db.createSession(session("mid", { parentId: "root" }));
  db.createSession(session("leaf", { parentId: "mid" }));
  assert.deepEqual(db.ancestorChain("leaf").map((s) => s.id), ["root", "mid", "leaf"]);
  assert.deepEqual(db.ancestorChain("root").map((s) => s.id), ["root"]);
  assert.deepEqual(db.ancestorChain("nope"), []);
  db.close();
});

test("a subagent's thread is its own messages only", () => {
  // Spec §7: a subagent gets a fresh, task-only thread — parentId is null even
  // though origin_id points back at the spawner.
  const db = mem();
  db.createSession(session("spawner"));
  db.createMessage(message("p1", "spawner", "parent context", 1));
  db.createSession(
    session("sub", { kind: "subagent", parentId: null, originId: "spawner" }),
  );
  db.createMessage(message("t1", "sub", "the task", 2));
  assert.deepEqual(texts(db.threadFor("sub")), ["the task"]);
  assert.deepEqual(db.sessionsByOrigin("spawner").map((s) => s.id), ["sub"]);
  db.close();
});

// ---- migration --------------------------------------------------------------

test("migration is idempotent across two opens", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-db-test-"));
  const path = `${dir}/bough.db`;
  try {
    const first = openDb(path);
    first.createSession(session("s", { workspace: "/w", base: "abc123" }));
    first.createMessage(message("m1", "s", "hello", 100));
    first.createTurn(turn("t1", "s", "m1"));
    first.createSchedule({
      id: "sc1",
      title: "nightly",
      prompt: "do the thing",
      workspace: "/w",
      spec: "daily@09:00",
      enabled: true,
      createdAt: 1,
      lastRunAt: null,
      nextRunAt: 2,
      sessionId: "s",
    });
    const schemaBefore = introspect(path, first);
    first.close();

    // Second open re-applies the same schema block. It must not throw, must not
    // alter the schema, and must not touch a single row.
    const second = openDb(path);
    const schemaAfter = introspect(path, second);
    assert.deepEqual(schemaAfter, schemaBefore);
    assert.equal(second.getSession("s")?.base, "abc123");
    assert.deepEqual(texts(second.messagesFor("s")), ["hello"]);
    assert.equal(second.getTurn("t1")?.step, "start");
    assert.equal(second.getSchedule("sc1")?.spec, "daily@09:00");
    second.close();

    // ...and a third, to prove the second open was not a special case.
    const third = openDb(path);
    assert.deepEqual(introspect(path, third), schemaBefore);
    assert.deepEqual(texts(third.messagesFor("s")), ["hello"]);
    third.close();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

/** Every object in `sqlite_master`, plus the stamped version. Read raw on purpose. */
function introspect(path: string, keepAlive: SqliteDb): unknown {
  // `keepAlive` is the open handle whose file we are inspecting — taking it as an
  // argument documents that the caller must not have closed it yet.
  void keepAlive;
  const raw = new Database(path);
  try {
    const objects = raw
      .prepare(`SELECT type, name, sql FROM sqlite_master ORDER BY type, name`)
      .all();
    return { objects, version: userVersion(raw) };
  } finally {
    raw.close();
  }
}

test("migrate reports the version it found and stamps the current one", () => {
  const raw = new Database(":memory:");
  try {
    assert.equal(userVersion(raw), 0);
    assert.equal(migrate(raw), 0, "a fresh file is at version 0");
    assert.equal(userVersion(raw), SCHEMA_VERSION);
    assert.equal(migrate(raw), SCHEMA_VERSION, "a second run finds the stamp it left");
    assert.equal(userVersion(raw), SCHEMA_VERSION);
  } finally {
    raw.close();
  }
});

test("migration is forward-only: a newer schema version is refused", () => {
  const raw = new Database(":memory:");
  try {
    raw.exec(`PRAGMA user_version = ${SCHEMA_VERSION + 1}`);
    assert.throws(
      () => migrate(raw),
      (e: unknown) =>
        e instanceof Error &&
        e.message.includes("newer bough") &&
        e.message.includes(`v${SCHEMA_VERSION + 1}`),
      "the error must name both versions, not just say 'failed'",
    );
  } finally {
    raw.close();
  }
});

test("dropped columns are absent from the schema", () => {
  // The port's promise, made checkable: archived_at / deprecated_at /
  // first_output_at and message_embeddings do not exist, so no caller can start
  // depending on them (spec §4, §17).
  const raw = new Database(":memory:");
  try {
    migrate(raw);
    const cols = (t: string) =>
      (raw.prepare(`PRAGMA table_info(${t})`).all() as { name: string }[]).map((c) => c.name);
    for (const gone of ["archived_at", "deprecated_at"]) {
      assert.ok(!cols("sessions").includes(gone), `sessions.${gone} must not exist`);
    }
    assert.ok(!cols("turns").includes("first_output_at"), "turns.first_output_at must not exist");
    const tables = (raw
      .prepare(`SELECT name FROM sqlite_master WHERE type = 'table'`)
      .all() as { name: string }[]).map((r) => r.name);
    assert.ok(!tables.includes("message_embeddings"), "message_embeddings must not exist");
  } finally {
    raw.close();
  }
});

// ---- sessions ---------------------------------------------------------------

test("createSession returns the row as stored", () => {
  const db = mem();
  const created = db.createSession(
    session("s", { workspace: "/w", originDir: "/w", model: "m", draft: "hi" }),
  );
  assert.deepEqual(created, db.getSession("s"));
  assert.equal(created.workspace, "/w");
  assert.equal(created.outcomeOk, null);
  db.close();
});

test("listSessions is newest first and hides nothing", () => {
  const db = mem();
  db.createSession(session("a", { createdAt: 1 }));
  db.createSession(session("sub", { createdAt: 2, kind: "subagent", originId: "a" }));
  db.createSession(session("b", { createdAt: 3 }));
  // Visibility is the caller's derivation: every kind is returned here.
  assert.deepEqual(db.listSessions().map((s) => s.id), ["b", "sub", "a"]);
  db.close();
});

test("session setters round-trip, and null clears a pin", () => {
  const db = mem();
  db.createSession(session("s"));
  db.setSessionTitle("s", "renamed");
  db.setSessionWorkspace("s", "/checkout");
  db.setSessionBase("s", "deadbeef");
  db.setSessionModel("s", "opus");
  db.setSessionEffort("s", "high");
  db.setSessionDraft("s", "prefilled");
  db.setSessionOutcome("s", false);
  let s = db.getSession("s")!;
  assert.equal(s.title, "renamed");
  assert.equal(s.model, "opus");
  assert.equal(s.effort, "high");
  assert.equal(s.draft, "prefilled");
  assert.equal(s.outcomeOk, false);
  assert.deepEqual(db.getSessionRuntime("s"), { workspace: "/checkout", base: "deadbeef" });

  db.setSessionModel("s", null);
  db.setSessionDraft("s", null);
  s = db.getSession("s")!;
  assert.equal(s.model, null);
  assert.equal(s.draft, null);
  db.close();
});

test("addSessionUsage accumulates cost and overwrites the context gauge", () => {
  const db = mem();
  db.createSession(session("s"));
  db.addSessionUsage("s", {
    inputTokens: 100,
    outputTokens: 10,
    reasoningTokens: 5,
    cacheReadTokens: 900,
    cacheWriteTokens: 0,
    costUsd: 0.01,
  }, 111);
  db.addSessionUsage("s", {
    inputTokens: 50,
    outputTokens: 20,
    reasoningTokens: 1,
    cacheReadTokens: 2_000,
    cacheWriteTokens: 100,
    costUsd: 0.02,
  }, 222);

  assert.deepEqual(db.sessionUsage("s"), {
    inputTokens: 150,
    outputTokens: 30,
    reasoningTokens: 6,
    cacheReadTokens: 2_900,
    cacheWriteTokens: 100,
    costUsd: 0.03,
  });
  const s = db.getSession("s")!;
  // The gauge describes the LAST round only: 50 uncached + 2000 read + 100 written.
  assert.equal(s.contextTokens, 2_150);
  assert.equal(s.cachedTokens, 2_100);
  assert.equal(s.lastLlmAt, 222);
  db.close();
});

test("treeUsage rolls up delegated branches and excludes forks", () => {
  const db = mem();
  const spend = (id: string, cost: number) =>
    db.addSessionUsage(id, { inputTokens: 10, outputTokens: 1, costUsd: cost }, 1);

  db.createSession(session("root"));
  db.createSession(session("sub", { kind: "subagent", originId: "root" }));
  db.createSession(session("nested", { kind: "subagent", originId: "sub" }));
  db.createSession(session("wfa", { kind: "workflow_agent", originId: "root" }));
  db.createSession(session("fork", { kind: "fork", originId: "root" }));
  for (const id of ["root", "sub", "nested", "wfa", "fork"]) spend(id, 1);

  // root + sub + nested + wfa = 4; the fork is a sibling the user opened, not
  // delegated work charged to this tree.
  assert.equal(db.treeUsage("root").costUsd, 4);
  assert.equal(db.treeUsage("root").inputTokens, 40);
  assert.equal(db.treeUsage("sub").costUsd, 2);
  assert.equal(db.treeUsage("nested").costUsd, 1);
  db.close();
});

test("busySessionIds reads running turns, not pending messages", () => {
  const db = mem();
  db.createSession(session("a"));
  db.createSession(session("b"));
  db.createMessage(message("ma", "a", "x", 1));
  db.createMessage(message("mb", "b", "x", 1));
  db.createTurn(turn("ta", "a", "ma", { status: "running" }));
  db.createTurn(turn("tb", "b", "mb", { status: "orphaned" }));
  // b's message is still pending after a crash, but its turn is orphaned — the
  // session must not read as busy forever.
  db.updateMessage("mb", [{ type: "text", text: "x" }], true);
  assert.deepEqual([...db.busySessionIds()], ["a"]);
  db.close();
});

// ---- messages ---------------------------------------------------------------

test("updateMessage overwrites parts and the pending flag", () => {
  const db = mem();
  db.createSession(session("s"));
  db.createMessage({
    id: "m",
    sessionId: "s",
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: 1,
  });
  db.updateMessage("m", [
    { type: "reasoning", text: "thinking" },
    { type: "text", text: "done" },
  ], false);
  const m = db.getMessage("m")!;
  assert.equal(m.pending, false);
  assert.equal(m.parts.length, 2);
  assert.equal(m.parts[0].type, "reasoning");
  db.close();
});

// ---- turns ------------------------------------------------------------------

test("updateTurn checkpoints with the injected clock", () => {
  let clock = 5_000;
  const db = mem(() => clock);
  db.createSession(session("s"));
  db.createMessage(message("m", "s", "x", 1));
  const t = db.createTurn(turn("t", "s", "m", { createdAt: 1, updatedAt: 1 }));
  assert.equal(t.usage, null, "a turn with no reported round has no usage");

  clock = 6_000;
  db.updateTurn("t", { step: "round 1" });
  assert.equal(db.getTurn("t")!.updatedAt, 6_000);
  assert.equal(db.getTurn("t")!.status, "running", "an unpatched field is preserved");

  clock = 7_000;
  db.updateTurn("t", {
    status: "done",
    usage: { inputTokens: 10, outputTokens: 2, costUsd: 0.5 },
  });
  const done = db.getTurn("t")!;
  assert.equal(done.updatedAt, 7_000);
  assert.equal(done.step, "round 1");
  assert.deepEqual(done.usage, {
    inputTokens: 10,
    outputTokens: 2,
    reasoningTokens: null,
    cacheReadTokens: null,
    cacheWriteTokens: null,
    costUsd: 0.5,
  });

  // A turn's usage is a running total the runner carries: patching it again
  // REPLACES rather than adds, or every round after the first double-counts.
  clock = 8_000;
  db.updateTurn("t", { usage: { inputTokens: 25, outputTokens: 4 } });
  assert.equal(db.getTurn("t")!.usage!.inputTokens, 25);

  db.updateTurn("t", { status: "error", error: "context window exceeded" });
  assert.equal(db.getTurn("t")!.error, "context window exceeded");
  db.updateTurn("t", { error: null });
  assert.equal(db.getTurn("t")!.error, null);
  db.close();
});

test("turn lookups: by status, by message, latest per session", () => {
  let clock = 1;
  const db = mem(() => clock);
  db.createSession(session("s"));
  db.createMessage(message("m1", "s", "a", 1));
  db.createMessage(message("m2", "s", "b", 2));
  db.createTurn(turn("t1", "s", "m1", { createdAt: 1, updatedAt: 1 }));
  db.createTurn(turn("t2", "s", "m2", { createdAt: 2, updatedAt: 2 }));

  assert.deepEqual(db.turnsForSession("s").map((t) => t.id), ["t1", "t2"]);
  assert.deepEqual(db.turnsByStatus("running").map((t) => t.id), ["t1", "t2"]);
  assert.equal(db.turnForMessage("m1")!.id, "t1");

  // Both checkpoints land on the same millisecond — the tie-break must still pick
  // the later row rather than an arbitrary one.
  clock = 9;
  db.updateTurn("t1", { status: "done" });
  db.updateTurn("t2", { status: "interrupted" });
  assert.deepEqual([...db.latestTurnStatuses()], [["s", "interrupted"]]);
  assert.deepEqual(db.turnsByStatus("running"), []);
  db.close();
});

// ---- durable KV -------------------------------------------------------------

test("session_state is upserted, listed by key, and reports real deletes", () => {
  const db = mem();
  db.setState("root", "b", `{"n":2}`, 10);
  db.setState("root", "a", `{"n":1}`, 20);
  db.setState("root", "a", `{"n":11}`, 30);
  db.setState("other", "a", `{"n":9}`, 40);

  assert.equal(db.getState("root", "a"), `{"n":11}`);
  assert.equal(db.getState("root", "missing"), undefined);
  assert.deepEqual(db.listState("root"), [
    { key: "a", bytes: 8, updatedAt: 30 },
    { key: "b", bytes: 7, updatedAt: 10 },
  ]);
  assert.equal(db.deleteState("root", "a"), true);
  assert.equal(db.deleteState("root", "a"), false);
  // Scoping is by root id: the other lineage is untouched.
  assert.equal(db.getState("other", "a"), `{"n":9}`);
  db.close();
});

// ---- schedules --------------------------------------------------------------

test("dueSchedules returns enabled, past-due rows soonest first", () => {
  const db = mem();
  const make = (id: string, next: number, enabled: boolean) =>
    db.createSchedule({
      id,
      title: id,
      prompt: "p",
      workspace: null,
      spec: "every:1h",
      enabled,
      createdAt: 1,
      lastRunAt: null,
      nextRunAt: next,
      sessionId: null,
    });
  make("late", 50, true);
  make("later", 90, true);
  make("off", 10, false);
  make("future", 500, true);

  assert.deepEqual(db.dueSchedules(100).map((s) => s.id), ["late", "later"]);
  db.markScheduleRun("late", 100, 3_700_100);
  assert.deepEqual(db.dueSchedules(100).map((s) => s.id), ["later"]);
  assert.equal(db.getSchedule("late")!.lastRunAt, 100);

  const s = { ...db.getSchedule("later")!, enabled: false, spec: "daily@07:00" };
  db.updateSchedule(s);
  assert.equal(db.getSchedule("later")!.enabled, false);
  assert.equal(db.getSchedule("later")!.spec, "daily@07:00");
  assert.deepEqual(db.dueSchedules(100), []);

  db.deleteSchedule("off");
  assert.equal(db.getSchedule("off"), undefined);
  db.close();
});

// ---- workflows --------------------------------------------------------------

test("workflow rows round-trip and patch by field membership", () => {
  const db = mem();
  db.createSession(session("s"));
  const run = db.createWorkflow({
    id: "w1",
    sessionId: "s",
    name: "audit",
    description: "review handlers",
    script: "export const meta = {}",
    phases: [{ title: "Review" }, { title: "Verify", detail: "second pass" }],
    status: "running",
    currentPhase: null,
    result: null,
    error: null,
    args: { files: ["a.ts"] },
    resumeOf: null,
    createdAt: 1,
    finishedAt: null,
  });
  assert.deepEqual(run.phases, [{ title: "Review" }, { title: "Verify", detail: "second pass" }]);
  assert.deepEqual(run.args, { files: ["a.ts"] });

  db.updateWorkflow("w1", { currentPhase: "Review" });
  // An unpatched field survives — including the JSON ones.
  assert.deepEqual(db.getWorkflow("w1")!.args, { files: ["a.ts"] });
  assert.equal(db.getWorkflow("w1")!.currentPhase, "Review");

  db.updateWorkflow("w1", { status: "done", result: [1, 2, 3], finishedAt: 99 });
  const done = db.getWorkflow("w1")!;
  assert.equal(done.status, "done");
  assert.deepEqual(done.result, [1, 2, 3]);
  assert.equal(done.finishedAt, 99);
  assert.equal(done.script, "export const meta = {}", "the script is never patched");

  assert.deepEqual(db.unfinishedWorkflows(), []);
  assert.deepEqual(db.listWorkflows("s").map((w) => w.id), ["w1"]);
  assert.deepEqual(db.listWorkflows("nobody"), []);
  db.close();
});

/**
 * A fork's transcript IS its ancestor chain's messages, so it renders the parent's
 * workflow cards. Scoping the run list to one session id left every one of those
 * cards with no run row: they fell back to `⧉ name · launched`, dropping the status,
 * the agent counts and the elapsed time of a run that had already finished.
 */
test("a fork and a compaction list their ancestors' runs, not just their own", () => {
  const db = mem();
  db.createSession(session("root"));
  db.createSession(session("fork", { parentId: "root", kind: "fork" }));
  db.createSession(session("compact", { parentId: "fork", kind: "compaction" }));
  db.createSession(session("other"));
  const run = (id: string, sessionId: string) =>
    db.createWorkflow({
      id,
      sessionId,
      name: id,
      description: "",
      script: "",
      phases: [],
      status: "done",
      currentPhase: null,
      result: null,
      error: null,
      args: null,
      resumeOf: null,
      createdAt: 1,
      finishedAt: 2,
    });
  run("wRoot", "root");
  run("wFork", "fork");
  run("wOther", "other");

  assert.deepEqual(db.listWorkflows("root").map((w) => w.id).sort(), ["wRoot"]);
  assert.deepEqual(db.listWorkflows("fork").map((w) => w.id).sort(), ["wFork", "wRoot"]);
  // Two levels down, and still not the unrelated session's run.
  assert.deepEqual(db.listWorkflows("compact").map((w) => w.id).sort(), ["wFork", "wRoot"]);
  assert.deepEqual(db.listWorkflows("other").map((w) => w.id), ["wOther"]);
  db.close();
});

/**
 * The case the parent-only walk missed. Forking a ROOT parents the branch at the
 * root's parent — which is nothing — and copies the turns, so `parent_id` is NULL and
 * `origin_id` is the only edge back to the run that produced the copied card.
 */
test("a fork seeded by copy reads its origin's runs; a subagent does not", () => {
  const db = mem();
  db.createSession(session("root"));
  db.createSession(session("branch", { kind: "fork", originId: "root" }));
  db.createSession(session("helper", { kind: "subagent", originId: "root" }));
  db.createWorkflow({
    id: "w1",
    sessionId: "root",
    name: "n",
    description: "",
    script: "",
    phases: [],
    status: "done",
    currentPhase: null,
    result: null,
    error: null,
    args: null,
    resumeOf: null,
    createdAt: 1,
    finishedAt: 2,
  });

  assert.deepEqual(db.listWorkflows("branch").map((w) => w.id), ["w1"]);
  // `origin_id` on a delegate means its SPAWNER. Its runs are not the delegate's.
  assert.deepEqual(db.listWorkflows("helper"), []);
  db.close();
});

test("the agent journal is keyed lookup plus ordered listing", () => {
  const db = mem();
  db.createSession(session("s"));
  db.createWorkflow({
    id: "w1",
    sessionId: "s",
    name: "n",
    description: "d",
    script: "",
    phases: [],
    status: "running",
    currentPhase: null,
    result: null,
    error: null,
    args: null,
    resumeOf: null,
    createdAt: 1,
    finishedAt: null,
  });
  const agent = (id: string, idx: number, key: string) =>
    db.createWorkflowAgent({
      id,
      runId: "w1",
      idx,
      key,
      label: id,
      phase: "Review",
      prompt: `review ${id}`,
      model: null,
      status: "queued",
      result: null,
      error: null,
      sessionId: null,
      startedAt: 10,
      finishedAt: null,
    });
  agent("a2", 2, "k2");
  agent("a1", 1, "k1");

  assert.deepEqual(db.listWorkflowAgents("w1").map((a) => a.id), ["a1", "a2"]);
  assert.equal(db.findWorkflowAgent("w1", "k2")!.id, "a2");
  assert.equal(db.findWorkflowAgent("w1", "nope"), undefined);
  assert.equal(db.findWorkflowAgent("other", "k1"), undefined);

  // A queued agent's clock restarts when it actually leaves the semaphore. The
  // backing subagent session must exist first — `session_id` is a real foreign key,
  // so a journal row can never point at a session the drill-in cannot open.
  db.createSession(session("sub1", { kind: "workflow_agent", originId: "s" }));
  db.updateWorkflowAgent("a1", { status: "running", sessionId: "sub1", startedAt: 500 });
  let a1 = db.findWorkflowAgent("w1", "k1")!;
  assert.equal(a1.status, "running");
  assert.equal(a1.startedAt, 500);
  assert.equal(a1.prompt, "review a1", "an unpatched field survives");

  db.updateWorkflowAgent("a1", { status: "done", result: "report text", finishedAt: 900 });
  a1 = db.findWorkflowAgent("w1", "k1")!;
  assert.equal(a1.result, "report text");
  assert.equal(a1.sessionId, "sub1");
  db.close();
});

// ---- keyword search ---------------------------------------------------------

test("search indexes prose, is idempotent, and rebuilds identically", () => {
  const db = mem();
  db.createSession(session("s1"));
  db.createSession(session("s2"));
  const m1 = db.createMessage({
    id: "m1",
    sessionId: "s1",
    role: "user",
    parts: [{ type: "text", text: "the patch engine anchors on hashes" }],
    pending: false,
    createdAt: 100,
  });
  const m2 = db.createMessage({
    id: "m2",
    sessionId: "s2",
    role: "supervisor",
    parts: [
      { type: "reasoning", text: "consider the patch grammar" },
      { type: "tool_call", id: "c1", name: "run_steps", input: { code: "patch patch patch" } },
      { type: "text", text: "applied the patch" },
    ],
    pending: false,
    createdAt: 200,
  });
  const m3 = db.createMessage({
    id: "m3",
    sessionId: "s1",
    role: "user",
    parts: [{ type: "tool_result", callId: "c1", output: "patch", isError: false }],
    pending: false,
    createdAt: 300,
  });
  for (const m of [m1, m2, m3]) db.indexMessage(m);
  // Re-indexing the same message must not duplicate it — the streaming runner
  // re-indexes on every update.
  db.indexMessage(m2);

  const hits = db.searchMessages("patch");
  assert.deepEqual(hits.map((h) => h.messageId).sort(), ["m1", "m2"]);
  assert.equal(hits.length, 2, "no duplicate row from re-indexing");
  assert.ok(
    !hits.some((h) => h.messageId === "m3"),
    "tool results are not indexed — only prose and reasoning",
  );
  assert.ok(hits.every((h) => h.snippet.includes("patch")));
  assert.equal(hits.find((h) => h.messageId === "m1")!.createdAt, 100);

  assert.deepEqual(db.searchMessages("patch", { sessionId: "s2" }).map((h) => h.messageId), ["m2"]);
  assert.equal(db.searchMessages("patch", { limit: 1 }).length, 1);
  assert.deepEqual(db.searchMessages("nonexistentword"), []);
  // Reasoning text is indexed too (schema.sql: the projection is text + reasoning).
  assert.deepEqual(db.searchMessages("grammar").map((h) => h.messageId), ["m2"]);

  const incremental = db.searchMessages("patch");
  db.rebuildSearchIndex();
  assert.deepEqual(db.searchMessages("patch"), incremental, "rebuild == incremental");
  db.close();
});

test("a malformed search query is a 400 that says what to do", () => {
  const db = mem();
  let e: (Error & { status?: number }) | undefined;
  try {
    db.searchMessages(`"unterminated`);
  } catch (caught) {
    e = caught as Error & { status?: number };
  }
  assert.ok(e, "a malformed query must throw");
  assert.equal(e.status, 400);
  assert.ok(e.message.includes("FTS5"), "the message names the syntax, not just 'failed'");
  assert.ok(e.message.includes("Quote a phrase"), "and it names the move that resolves it");
  db.close();
});

// ---- integrity --------------------------------------------------------------

test("foreign keys are enforced on every connection", () => {
  const db = mem();
  assert.throws(
    () => db.createMessage(message("m", "no-such-session", "orphan", 1)),
    /FOREIGN KEY/i,
  );
  db.close();
});

test("openDb creates the parent directory", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-db-test-"));
  try {
    const db = openDb(`${dir}/nested/deeper/bough.db`);
    db.createSession(session("s"));
    assert.equal(db.getSession("s")!.id, "s");
    db.close();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// Command-history memory
// ---------------------------------------------------------------------------

function cmdRecord(over: Partial<CommandRecord> = {}): CommandRecord {
  return {
    sessionId: "s1",
    ts: 1_000,
    repo: "repo",
    cmd: "true",
    tags: "",
    tagList: [],
    dirs: [],
    exitCode: 0,
    durationMs: 1,
    outputHead: "",
    spillPath: null,
    source: "live",
    ...over,
  };
}

test("recordCommand round-trips through commandTagRows, scoped by repo", () => {
  const db = mem();
  db.createSession(session("s1"));
  db.recordCommand(cmdRecord({ tags: "git:push", tagList: ["git", "push"] }));
  db.recordCommand(cmdRecord({ repo: "other", tags: "bun", tagList: ["bun"], ts: 2_000 }));
  assert.deepStrictEqual(db.commandTagRows("repo"), [
    { tag: "git", ts: 1_000, exitCode: 0 },
    { tag: "push", ts: 1_000, exitCode: 0 },
  ]);
  assert.deepStrictEqual(db.commandTagRows("other"), [{ tag: "bun", ts: 2_000, exitCode: 0 }]);
  db.close();
});

test("commandTagRows dir scope covers the dir and its descendants — not name prefixes", () => {
  const db = mem();
  db.createSession(session("s1"));
  db.recordCommand(cmdRecord({ tagList: ["a"], tags: "a", dirs: ["src"] }));
  db.recordCommand(cmdRecord({ tagList: ["b"], tags: "b", dirs: ["src/tui"] }));
  db.recordCommand(cmdRecord({ tagList: ["c"], tags: "c", dirs: ["src2"] }));
  assert.deepStrictEqual(db.commandTagRows("repo", { dir: "src" }).map((r) => r.tag), ["a", "b"]);
  assert.deepStrictEqual(db.commandTagRows("repo", { dir: "src/tui" }).map((r) => r.tag), ["b"]);
  db.close();
});

test("commandTagRows sinceTs floors the lookback", () => {
  const db = mem();
  db.createSession(session("s1"));
  db.recordCommand(cmdRecord({ tagList: ["old"], tags: "old", ts: 10 }));
  db.recordCommand(cmdRecord({ tagList: ["new"], tags: "new", ts: 500 }));
  assert.deepStrictEqual(db.commandTagRows("repo", { sinceTs: 100 }).map((r) => r.tag), ["new"]);
  db.close();
});

test("a pre-session_id schedules table is ALTERed in place, rows kept", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-sched-alter-"));
  const path = join(dir, "old.db");
  try {
    // Fabricate the old shape: schedules without session_id. Unlike the
    // command_history rebuild below, these rows are USER RECORDS and must survive.
    const raw = new Database(path);
    raw.exec(`CREATE TABLE schedules (id TEXT PRIMARY KEY, title TEXT NOT NULL,
      prompt TEXT NOT NULL, workspace TEXT, spec TEXT NOT NULL, enabled INTEGER NOT NULL,
      created_at INTEGER NOT NULL, last_run_at INTEGER, next_run_at INTEGER NOT NULL);
      INSERT INTO schedules VALUES ('sc1', 'nightly', 'check the deploy', NULL,
        'every:1h', 1, 1, NULL, 2);
      PRAGMA user_version = 1;`);
    raw.close();
    const db = openDb(path);
    const kept = db.getSchedule("sc1")!;
    assert.equal(kept.title, "nightly");
    assert.equal(kept.enabled, true);
    // Reports to nobody, which is exactly what it did before the column existed.
    assert.equal(kept.sessionId, null);
    // …and the new shape round-trips through the added column.
    db.updateSchedule({ ...kept, enabled: false });
    assert.equal(db.getSchedule("sc1")!.enabled, false);
    db.close();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("a pre-output_head command_history is rebuilt empty at open, once", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-rebuild-"));
  const path = join(dir, "old.db");
  try {
    // Fabricate the day-one shape: the table group without output_head.
    const raw = new Database(path);
    raw.exec(`CREATE TABLE command_history (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL,
      ts INTEGER NOT NULL, repo TEXT NOT NULL, cmd TEXT NOT NULL, tags TEXT NOT NULL,
      exit_code INTEGER, duration_ms INTEGER, source TEXT NOT NULL DEFAULT 'live');
      CREATE TABLE command_tags (command_id INTEGER NOT NULL, tag TEXT NOT NULL);
      CREATE TABLE command_dirs (command_id INTEGER NOT NULL, rel_dir TEXT NOT NULL);
      CREATE VIRTUAL TABLE command_history_fts USING fts5(cmd, tags, command_id UNINDEXED);
      INSERT INTO command_history (session_id, ts, repo, cmd, tags)
        VALUES ('s', 1, 'r', 'old cmd', 't');`);
    raw.close();
    const db = openDb(path);
    // The old rows are gone, the new shape accepts a full record.
    assert.deepStrictEqual(db.commandTagRows("r"), []);
    db.createSession(session("s1"));
    db.recordCommand(cmdRecord({ tags: "a", tagList: ["a"], outputHead: "out" }));
    assert.equal(db.commandTagRows("repo").length, 1);
    db.close();
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
