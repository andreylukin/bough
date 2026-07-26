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
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable from this environment, and a test that cannot run offline does not
 * belong in `deno task test` (plan §7).
 */
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { openDb, SqliteDb } from "./db.ts";
import { migrate, SCHEMA_VERSION, userVersion } from "./migrate.ts";
import type { Message, Session, Turn } from "../schema/parts.ts";

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

Deno.test("messagesFor breaks a created_at tie by insertion order", () => {
  const db = mem();
  db.createSession(session("s"));
  // Everything in the same millisecond: only rowid can order these.
  db.createMessage(message("m1", "s", "first", 5_000));
  db.createMessage(message("m2", "s", "second", 5_000));
  db.createMessage(message("m3", "s", "third", 5_000));
  assert.deepEqual(texts(db.messagesFor("s")), ["first", "second", "third"]);
  db.close();
});

Deno.test("a turn started in the same millisecond as a seed sorts after it", () => {
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

Deno.test("created_at still dominates rowid when timestamps differ", () => {
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

Deno.test("threadFor concatenates three levels root -> parent -> own", () => {
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

Deno.test("ancestorChain is root-first and inclusive; unknown ids are empty", () => {
  const db = mem();
  db.createSession(session("root"));
  db.createSession(session("mid", { parentId: "root" }));
  db.createSession(session("leaf", { parentId: "mid" }));
  assert.deepEqual(db.ancestorChain("leaf").map((s) => s.id), ["root", "mid", "leaf"]);
  assert.deepEqual(db.ancestorChain("root").map((s) => s.id), ["root"]);
  assert.deepEqual(db.ancestorChain("nope"), []);
  db.close();
});

Deno.test("a subagent's thread is its own messages only", () => {
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

Deno.test("migration is idempotent across two opens", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-db-test-" });
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
    await Deno.remove(dir, { recursive: true });
  }
});

/** Every object in `sqlite_master`, plus the stamped version. Read raw on purpose. */
function introspect(path: string, keepAlive: SqliteDb): unknown {
  // `keepAlive` is the open handle whose file we are inspecting — taking it as an
  // argument documents that the caller must not have closed it yet.
  void keepAlive;
  const raw = new DatabaseSync(path);
  try {
    const objects = raw
      .prepare(`SELECT type, name, sql FROM sqlite_master ORDER BY type, name`)
      .all();
    return { objects, version: userVersion(raw) };
  } finally {
    raw.close();
  }
}

Deno.test("migrate reports the version it found and stamps the current one", () => {
  const raw = new DatabaseSync(":memory:");
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

Deno.test("migration is forward-only: a newer schema version is refused", () => {
  const raw = new DatabaseSync(":memory:");
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

Deno.test("dropped columns are absent from the schema", () => {
  // The port's promise, made checkable: archived_at / deprecated_at /
  // first_output_at and message_embeddings do not exist, so no caller can start
  // depending on them (spec §4, §17).
  const raw = new DatabaseSync(":memory:");
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

Deno.test("createSession returns the row as stored", () => {
  const db = mem();
  const created = db.createSession(
    session("s", { workspace: "/w", originDir: "/w", model: "m", draft: "hi" }),
  );
  assert.deepEqual(created, db.getSession("s"));
  assert.equal(created.workspace, "/w");
  assert.equal(created.outcomeOk, null);
  db.close();
});

Deno.test("listSessions is newest first and hides nothing", () => {
  const db = mem();
  db.createSession(session("a", { createdAt: 1 }));
  db.createSession(session("sub", { createdAt: 2, kind: "subagent", originId: "a" }));
  db.createSession(session("b", { createdAt: 3 }));
  // Visibility is the caller's derivation: every kind is returned here.
  assert.deepEqual(db.listSessions().map((s) => s.id), ["b", "sub", "a"]);
  db.close();
});

Deno.test("session setters round-trip, and null clears a pin", () => {
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

Deno.test("addSessionUsage accumulates cost and overwrites the context gauge", () => {
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

Deno.test("treeUsage rolls up delegated branches and excludes forks", () => {
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

Deno.test("busySessionIds reads running turns, not pending messages", () => {
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

Deno.test("updateMessage overwrites parts and the pending flag", () => {
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

Deno.test("updateTurn checkpoints with the injected clock", () => {
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

Deno.test("turn lookups: by status, by message, latest per session", () => {
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

Deno.test("session_state is upserted, listed by key, and reports real deletes", () => {
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

Deno.test("dueSchedules returns enabled, past-due rows soonest first", () => {
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

Deno.test("workflow rows round-trip and patch by field membership", () => {
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

Deno.test("the agent journal is keyed lookup plus ordered listing", () => {
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

Deno.test("search indexes prose, is idempotent, and rebuilds identically", () => {
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

Deno.test("a malformed search query is a 400 that says what to do", () => {
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

Deno.test("foreign keys are enforced on every connection", () => {
  const db = mem();
  assert.throws(
    () => db.createMessage(message("m", "no-such-session", "orphan", 1)),
    /FOREIGN KEY/i,
  );
  db.close();
});

Deno.test("openDb creates the parent directory", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-db-test-" });
  try {
    const db = openDb(`${dir}/nested/deeper/bough.db`);
    db.createSession(session("s"));
    assert.equal(db.getSession("s")!.id, "s");
    db.close();
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
