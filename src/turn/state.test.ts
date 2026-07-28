/**
 * Checkpointing and boot recovery.
 *
 * The crash is simulated the honest way: a turn is really started against a real
 * database **file**, the runner is abandoned mid-round (its LLM never answers), and
 * then the handle is closed and reopened — which is what a process death and a
 * restart actually leave behind. Writing the `running` row by hand would test the
 * recovery against a fixture rather than against the state the runner produces, and
 * those are exactly the two things that drift.
 *
 * What has to be true after the restart: the turn is `orphaned`, the supervisor
 * message is closed with a note saying the server restarted, the session is not
 * busy, and a fresh turn runs on it normally. A stuck `pending` message is the
 * user-visible form of this bug — the session looks like it is still working, and
 * every later post queues behind a turn that no longer exists.
 *
 * Offline and hermetic: an in-memory database where a file is not needed, a temp
 * file where it is, and never `~/.bough`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx, LlmBlock, LlmClient, LlmResult } from "../types.ts";
import { beginTurn, RUN_STEPS, STOP } from "./runner.ts";
import { TurnRegistry } from "./queue.ts";
import {
  checkpoint,
  finishTurn,
  INITIAL_STEP,
  ORPHAN_ERROR,
  ORPHAN_NOTE,
  recoverOrphanedTurns,
  startTurn,
} from "./state.ts";

// ---- fixtures ---------------------------------------------------------------

const STUB_PROMPT = () => ({ system: "SYSTEM", systemVolatile: "", sections: [] as never[] });

function seedSession(db: SqliteDb, kind: Session["kind"] = "root"): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    title: "crash test",
    kind,
    createdAt: 1_000,
    parentId: null,
  });
}

/**
 * `at` defaults to the real clock, and the crash test relies on it: messages order
 * by `(createdAt, rowid)`, and the runner stamps its supervisor placeholder from the
 * real clock too. Seeding with small synthetic numbers would sort every user message
 * before every supervisor one and quietly test a transcript that cannot occur
 * (plan §6.1).
 */
function userMessage(db: SqliteDb, sessionId: string, text: string, at = Date.now()): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role: "user",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: at,
  });
}

/** A model that answers, once, with text and a stop. */
function answeringLlm(text: string): LlmClient {
  return {
    run(): Promise<LlmResult> {
      const content: LlmBlock[] = [
        { type: "text", text },
        { type: "tool_use", id: crypto.randomUUID(), name: STOP, input: {} },
      ];
      return Promise.resolve({ content, stopReason: "tool_use" });
    },
  };
}

// ---- checkpointing ----------------------------------------------------------

test("a turn opens running and every checkpoint records where it got to", () => {
  const db = openDb(":memory:");
  const session = seedSession(db);
  const message = userMessage(db, session.id, "hi", 2_000);

  const turn = startTurn(db, session.id, message.id, () => 5_000);
  assert.equal(turn.status, "running");
  assert.equal(turn.step, INITIAL_STEP);
  assert.equal(turn.createdAt, 5_000);
  assert.deepEqual([...db.busySessionIds()], [session.id]);

  checkpoint(db, turn.id, "round:1");
  assert.equal(db.getTurn(turn.id)!.step, "round:1");

  checkpoint(db, turn.id, "tool:run_steps", {
    inputTokens: 10,
    outputTokens: 2,
    costUsd: 0.5,
  });
  const mid = db.getTurn(turn.id)!;
  assert.equal(mid.step, "tool:run_steps");
  assert.equal(mid.usage?.inputTokens, 10);
  assert.equal(mid.status, "running", "a checkpoint never ends a turn");

  // Usage REPLACES rather than accumulates — the runner carries the running total.
  checkpoint(db, turn.id, "round:2", { inputTokens: 25, outputTokens: 5 });
  assert.equal(db.getTurn(turn.id)!.usage?.inputTokens, 25);

  finishTurn(db, turn.id, "done");
  assert.equal(db.getTurn(turn.id)!.status, "done");
  assert.equal(db.busySessionIds().size, 0, "a finished turn frees its session");
  db.close();
});

test("finishing clears a stale error rather than leaving the previous attempt's", () => {
  const db = openDb(":memory:");
  const session = seedSession(db);
  const message = userMessage(db, session.id, "hi", 2_000);
  const turn = startTurn(db, session.id, message.id);

  finishTurn(db, turn.id, "error", { error: "provider exploded" });
  assert.equal(db.getTurn(turn.id)!.error, "provider exploded");
  finishTurn(db, turn.id, "done");
  assert.equal(db.getTurn(turn.id)!.error, null);
  db.close();
});

// ---- the crash --------------------------------------------------------------

test("a mid-turn crash leaves a session that is usable after restart", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-state-test-"));
  const path = `${dir}/bough.db`;
  try {
    // ── before the crash ──
    const db1 = openDb(path);
    const session = seedSession(db1);
    userMessage(db1, session.id, "start something long");

    // A model that never answers: the turn is genuinely mid-round when the process
    // "dies". Nothing awaits it, and its promise is abandoned along with the rest.
    const wedged: LlmClient = { run: () => new Promise<LlmResult>(() => {}) };
    const bus1 = new Bus();
    const ctx1: AppCtx = { db: db1, bus: bus1, llm: wedged, model: "claude-opus-4-8" };
    const started = beginTurn(ctx1, session.id, {
      registry: new TurnRegistry(),
      assemble: STUB_PROMPT,
    });
    // Let the loop reach its first round.
    await new Promise((r) => setTimeout(r, 0));

    assert.equal(db1.getMessage(started.message.id)!.pending, true);
    assert.equal(db1.busySessionIds().has(session.id), true);
    const wedgedTurn = db1.turnForMessage(started.message.id)!;
    assert.equal(wedgedTurn.status, "running");

    // ── the crash: the process goes away with the row still `running` ──
    db1.close();

    // ── the restart ──
    const db2 = openDb(path);
    const bus2 = new Bus();
    const events: BoughEvent[] = [];
    bus2.subscribe((e) => events.push(e));

    const recovered = recoverOrphanedTurns(db2, bus2);
    assert.equal(recovered.length, 1);
    assert.equal(recovered[0].turnId, wedgedTurn.id);
    assert.equal(recovered[0].sessionId, session.id);
    assert.equal(recovered[0].step, INITIAL_STEP, "it says where the turn got to");
    assert.equal(recovered[0].closedMessage, true);

    // The row, the message, and the session.
    assert.equal(db2.getTurn(wedgedTurn.id)!.status, "orphaned");
    assert.equal(db2.getTurn(wedgedTurn.id)!.error, ORPHAN_ERROR);
    const closed = db2.getMessage(started.message.id)!;
    assert.equal(closed.pending, false, "no stuck pending message");
    assert.equal(closed.parts.at(-1)!.type, "text");
    assert.equal((closed.parts.at(-1) as { text: string }).text, ORPHAN_NOTE);
    assert.equal(db2.busySessionIds().size, 0, "the session unblocks");

    // The client is told, so a reconnecting UI stops showing a turn in flight.
    assert.deepEqual(
      events.map((e) => e.type),
      ["message.part", "message.finished", "turn.finished"],
    );
    assert.equal(
      (events.at(-1)!.data as { status: string }).status,
      "orphaned",
    );

    // ── the session is usable ──
    const ctx2: AppCtx = {
      db: db2,
      bus: bus2,
      llm: answeringLlm("Picking up where we left off."),
      model: "claude-opus-4-8",
    };
    userMessage(db2, session.id, "try again");
    const outcome = await beginTurn(ctx2, session.id, {
      registry: new TurnRegistry(),
      assemble: STUB_PROMPT,
    }).done;

    assert.equal(outcome.status, "done");
    assert.equal(db2.getMessage(outcome.messageId)!.pending, false);
    // Two supervisor messages: the orphaned one and the new one, in order.
    const own = db2.messagesFor(session.id);
    assert.deepEqual(own.map((m) => m.role), ["user", "supervisor", "user", "supervisor"]);
    assert.equal(own.every((m) => !m.pending), true);
    db2.close();

    // The abandoned turn's promise is still pending against a closed database; it
    // is never observed, exactly as it would not be after a real process death.
    void started.done.catch(() => {});
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("recovery is idempotent and touches nothing that already ended", () => {
  const db = openDb(":memory:");
  const bus = new Bus();
  const session = seedSession(db);

  const finished = userMessage(db, session.id, "one", 2_000);
  const doneTurn = startTurn(db, session.id, finished.id, () => 2_100);
  finishTurn(db, doneTurn.id, "done");

  const strandedMessage = db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "supervisor",
    parts: [{ type: "text", text: "partial answer" }],
    pending: true,
    createdAt: 3_000,
  });
  const stranded = startTurn(db, session.id, strandedMessage.id, () => 3_000);

  assert.equal(recoverOrphanedTurns(db, bus).length, 1);
  assert.equal(recoverOrphanedTurns(db, bus).length, 0, "a second boot finds nothing");

  assert.equal(db.getTurn(doneTurn.id)!.status, "done", "a finished turn is untouched");
  assert.equal(db.getTurn(stranded.id)!.status, "orphaned");
  // The partial answer survives — the note is appended, not substituted.
  const parts = db.getMessage(strandedMessage.id)!.parts;
  assert.equal(parts.length, 2);
  assert.equal((parts[0] as { text: string }).text, "partial answer");
  db.close();
});

test("a message already closed still gets its turn.finished, and the hook still fires", () => {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const session = seedSession(db);

  // The message was closed but the row never was — the crash landed between the
  // two writes.
  const message = db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "supervisor",
    parts: [{ type: "text", text: "answered" }],
    pending: false,
    createdAt: 3_000,
  });
  const turn = startTurn(db, session.id, message.id);

  const seen: string[] = [];
  const recovered = recoverOrphanedTurns(db, bus, { onOrphan: (o) => seen.push(o.turnId) });

  assert.equal(recovered[0].closedMessage, false);
  assert.deepEqual(seen, [turn.id]);
  assert.deepEqual(events.map((e) => e.type), ["turn.finished"]);
  assert.equal(db.getMessage(message.id)!.parts.length, 1, "a closed message is not appended to");
  db.close();
});

test("a throwing orphan hook does not abandon the remaining orphans", () => {
  const db = openDb(":memory:");
  const bus = new Bus();
  const a = seedSession(db);
  const b = seedSession(db);
  for (const s of [a, b]) {
    const m = db.createMessage({
      id: crypto.randomUUID(),
      sessionId: s.id,
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: 3_000,
    });
    startTurn(db, s.id, m.id);
  }

  const errors: unknown[] = [];
  const recovered = recoverOrphanedTurns(db, bus, {
    onOrphan: () => {
      throw new Error("the parent notice failed");
    },
    onHookError: (e) => errors.push(e),
  });

  assert.equal(recovered.length, 2);
  assert.equal(errors.length, 2);
  assert.equal(db.busySessionIds().size, 0, "every session unblocked regardless");
  db.close();
});

test("recovery leaves the run_steps transcript replayable", () => {
  // A turn that died between a tool call and its result: replay has to close the
  // pair, and recovery must not invent one on the message itself.
  const db = openDb(":memory:");
  const bus = new Bus();
  const session = seedSession(db);
  const message = db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "supervisor",
    parts: [{ type: "tool_call", id: "c1", name: RUN_STEPS, input: { code: "x" } }],
    pending: true,
    createdAt: 3_000,
  });
  startTurn(db, session.id, message.id);
  recoverOrphanedTurns(db, bus);

  const parts = db.getMessage(message.id)!.parts;
  assert.deepEqual(parts.map((p) => p.type), ["tool_call", "text"]);
  db.close();
});
