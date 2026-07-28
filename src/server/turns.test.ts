/**
 * Tests for `POST /sessions/:id/interrupt`.
 *
 * Driven through the REAL route table (`createHandler(routes)`) rather than by
 * calling the handler directly, because half of what this task fixes is that the
 * route did not exist: a test that imported `interruptSession` and called it would
 * have passed against the broken tree. The registry is injected on the ctx, so
 * nothing here starts a turn, spawns a worker or reaches the network (plan §7).
 *
 * Assertions come from `node:assert/strict`: jsr.io is unreachable here and a test
 * that cannot run offline does not belong in `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { AppCtx } from "../types.ts";
import { TurnRegistry } from "../turn/queue.ts";
import { createHandler } from "./app.ts";
import type { InterruptResult, WithTurnRegistry } from "./turns.ts";

function fixture(): { ctx: AppCtx & WithTurnRegistry; db: SqliteDb; registry: TurnRegistry } {
  const db = openDb(":memory:");
  const registry = new TurnRegistry();
  return { ctx: { db, bus: new Bus(), model: "test-model", turnRegistry: registry }, db, registry };
}

function seedSession(db: SqliteDb): string {
  const id = crypto.randomUUID();
  db.createSession({
    id,
    parentId: null,
    title: "t",
    kind: "root",
    workspace: "/tmp",
    createdAt: Date.now(),
  });
  return id;
}

function interrupt(id: string): Request {
  return new Request(`http://127.0.0.1:4321/sessions/${id}/interrupt`, { method: "POST" });
}

test("interrupting a running turn aborts it and reports that it did", async () => {
  const { ctx, db, registry } = fixture();
  const id = seedSession(db);

  const controller = registry.begin(id);
  let cascaded = false;
  registry.onInterrupt(id, () => {
    cascaded = true;
  });

  const res = await createHandler(ctx)(interrupt(id));
  const body = await res.json() as InterruptResult;

  assert.equal(res.status, 200);
  assert.equal(body.interrupted, true);
  assert.equal(body.sessionId, id);
  assert.equal(controller.signal.aborted, true, "the turn's controller must be aborted");
  assert.equal(cascaded, true, "the cascade hooks must fire — that is what kills the children");
});

test("interrupting an idle session is an ANSWER, not an error", async () => {
  const { ctx, db } = fixture();
  const id = seedSession(db);

  const res = await createHandler(ctx)(interrupt(id));
  const body = await res.json() as InterruptResult;

  // A stop pressed a beat after the turn ended must not make the client branch on
  // a status code; `interrupted: false` is the whole answer.
  assert.equal(res.status, 200);
  assert.equal(body.interrupted, false);
  assert.match(body.message, /nothing was running/);
});

test("a second interrupt is safe — the verb is idempotent", async () => {
  const { ctx, db, registry } = fixture();
  const id = seedSession(db);
  registry.begin(id);

  const first = await (await createHandler(ctx)(interrupt(id))).json() as InterruptResult;
  const second = await (await createHandler(ctx)(interrupt(id))).json() as InterruptResult;

  assert.equal(first.interrupted, true);
  // Still true: the controller is still registered until the turn unwinds, and a
  // double-tap must not read as a failure either way.
  assert.equal(typeof second.interrupted, "boolean");
});

test("interrupting an unknown session is a 404, not a silent success", async () => {
  const { ctx } = fixture();
  const res = await createHandler(ctx)(interrupt("no-such-session"));
  assert.equal(res.status, 404);
  await res.body?.cancel();
});
