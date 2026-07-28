/**
 * The schedule ticker (T6.3).
 *
 * THE TEST THIS FILE EXISTS FOR is `a ticker down through five slots fires ONCE`.
 * Everything else here is support. The catch-up rule (spec §9, plan §6.8) is the one
 * schedule behavior that cannot be observed in ordinary use — you only find out it is
 * wrong when a laptop that was shut overnight wakes up and opens sixteen sessions —
 * so it is proven with an injected clock instead of being trusted.
 *
 * No LLM, no worker, no socket: the turn starter is a recorder, and the ticker's
 * `fire` seam is a counter. What is under test is the loop's arithmetic and the
 * ordering of the advance against the fire, both of which are pure.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve. (Same constraint `hostfn/shell.test.ts` and `bus.test.ts` document.)
 */

import { test } from "bun:test";
import assert from "node:assert";
import { createHandler, type Route, route } from "./server/app.ts";
import { Bus } from "./bus.ts";
import { openDb, type SqliteDb } from "./db/db.ts";
import { scheduleCreate } from "./hostfn/schedule.ts";
import type { BoughEvent } from "./schema/events.ts";
import type { Message, Schedule, Session } from "./schema/parts.ts";
import type { AppCtx } from "./types.ts";
import {
  createScheduleH,
  deleteScheduleH,
  fireSchedule,
  listSchedulesH,
  patchScheduleH,
  startScheduleTicker,
  tickSchedules,
} from "./schedules.ts";
import type { WithTurnStarter } from "./server/sessions.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const T0 = Date.UTC(2026, 0, 15, 12, 0, 0);
const MINUTE = 60_000;
const HOUR = 3_600_000;

interface Fixture {
  db: SqliteDb;
  ctx: AppCtx & WithTurnStarter;
  events: BoughEvent[];
  started: { session: Session; message: Message }[];
  close: () => void;
}

function fixture(now: () => number = () => T0): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const started: { session: Session; message: Message }[] = [];
  const ctx: AppCtx & WithTurnStarter = {
    db,
    bus,
    now,
    startTurn: (_c, session, message) => {
      started.push({ session, message });
    },
  };
  return { db, ctx, events, started, close: () => db.close() };
}

/** A schedule straight into the database — the CRUD has its own tests. */
function seed(db: SqliteDb, over: Partial<Schedule> = {}): Schedule {
  return db.createSchedule({
    id: crypto.randomUUID(),
    title: "deploy check",
    prompt: "check the deploy and report",
    workspace: null,
    spec: "every:30m",
    enabled: true,
    createdAt: T0,
    lastRunAt: null,
    nextRunAt: T0 + 30 * MINUTE,
    ...over,
  });
}

// ---------------------------------------------------------------------------
// Catch-up — the invariant
// ---------------------------------------------------------------------------

test("a ticker down through five slots fires ONCE, then resumes cadence", () => {
  const f = fixture();
  const schedule = seed(f.db, { spec: "every:1h", nextRunAt: T0 + HOUR });
  const fired: string[] = [];
  const fire = (s: Schedule) => fired.push(s.id);

  // Not due yet.
  assert.deepEqual(tickSchedules(f.db, T0 + 30 * MINUTE, fire).length, 0);

  // The server was down from T0+1h to T0+6h — five slots came and went.
  const back = T0 + 6 * HOUR;
  assert.deepEqual(tickSchedules(f.db, back, fire).map((s) => s.id), [schedule.id]);
  assert.deepEqual(fired, [schedule.id], "five missed slots must not become five runs");

  // The row advanced FROM NOW, not from the stale value: had it advanced from the
  // stored T0+1h it would be due at T0+2h — already past — and the next tick would
  // fire again, and again, for five more ticks.
  const after = f.db.getSchedule(schedule.id)!;
  assert.equal(after.lastRunAt, back);
  assert.equal(after.nextRunAt, back + HOUR);

  // Every tick between now and the next slot is quiet.
  for (const at of [back + 1, back + MINUTE, back + 59 * MINUTE]) {
    assert.equal(tickSchedules(f.db, at, fire).length, 0);
  }
  assert.equal(fired.length, 1);

  // Then the cadence resumes, exactly once.
  assert.equal(tickSchedules(f.db, back + HOUR, fire).length, 1);
  assert.equal(fired.length, 2);
  f.close();
});

test("a daily schedule missed for a week also fires once", () => {
  const f = fixture();
  const at = new Date(2026, 0, 15, 9, 0, 0, 0).getTime();
  const schedule = seed(f.db, { spec: "daily@09:00", nextRunAt: at });
  const fired: string[] = [];

  // A week later, at 09:30 local.
  const back = new Date(2026, 0, 22, 9, 30, 0, 0).getTime();
  tickSchedules(f.db, back, (s) => fired.push(s.id));
  assert.deepEqual(fired, [schedule.id]);
  // Next occurrence is tomorrow at 09:00 — today's 09:00 is already past.
  assert.equal(
    f.db.getSchedule(schedule.id)!.nextRunAt,
    new Date(2026, 0, 23, 9, 0, 0, 0).getTime(),
  );
  f.close();
});

test("the advance happens before the fire, so a throwing fire cannot hot-loop", () => {
  const f = fixture();
  const schedule = seed(f.db, { nextRunAt: T0 });
  let attempts = 0;
  const boom = () => {
    attempts++;
    throw new Error("fire failed");
  };

  // The loop reports the failure to the server log; muted here so an intentional
  // throw does not print a stack, and so the reporting can be asserted rather than
  // inferred.
  const logged: unknown[] = [];
  const realError = console.error;
  console.error = (...args: unknown[]) => logged.push(args);
  try {
    // The throw is swallowed — one bad schedule must not abort the pass — and the row
    // is advanced anyway, so the next tick 30 seconds later does not fire it again.
    assert.equal(tickSchedules(f.db, T0, boom).length, 1);
    assert.equal(attempts, 1);
    assert.equal(f.db.getSchedule(schedule.id)!.nextRunAt, T0 + 30 * MINUTE);
    assert.equal(tickSchedules(f.db, T0 + 30_000, boom).length, 0);
    assert.equal(attempts, 1);
  } finally {
    console.error = realError;
  }
  assert.equal(logged.length, 1);
  f.close();
});

test("one pass fires every due schedule and skips the disabled ones", () => {
  const f = fixture();
  const a = seed(f.db, { title: "a", nextRunAt: T0 });
  const b = seed(f.db, { title: "b", nextRunAt: T0 - HOUR });
  seed(f.db, { title: "disabled", nextRunAt: T0 - HOUR, enabled: false });
  seed(f.db, { title: "later", nextRunAt: T0 + HOUR });

  const fired = tickSchedules(f.db, T0, () => {});
  assert.deepEqual(new Set(fired.map((s) => s.id)), new Set([a.id, b.id]));
  f.close();
});

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

test("firing opens a FRESH ROOT session carrying the prompt, and starts a turn", () => {
  const f = fixture();
  const schedule = seed(f.db, { workspace: "/work/repo" });

  const fired = fireSchedule(f.ctx, schedule)!;
  assert.ok(fired);

  const session = f.db.getSession(fired.session.id)!;
  assert.equal(session.kind, "root");
  assert.equal(session.parentId, null, "a fired session inherits no thread");
  assert.equal(session.title, schedule.title);
  assert.equal(f.db.getSessionRuntime(session.id).workspace, "/work/repo");

  // The prompt is the session's first user message — the whole briefing, since the
  // session sees none of the conversation that created the schedule.
  const thread = f.db.threadFor(session.id);
  assert.equal(thread.length, 1);
  assert.equal(thread[0].role, "user");
  assert.deepEqual(thread[0].parts, [{ type: "text", text: schedule.prompt }]);
  assert.equal(thread[0].pending, false);

  // And a turn was asked for, on that session, with that message.
  assert.deepEqual(f.started.map((s) => [s.session.id, s.message.id]), [[
    session.id,
    thread[0].id,
  ]]);

  // Announced in order: the session, then the message on it.
  const kinds = f.events.map((e) => e.type);
  assert.deepEqual(kinds, ["session.created", "message.started"]);
  f.close();
});

test("firing without a workspace leaves the session unpinned", () => {
  const f = fixture();
  const fired = fireSchedule(f.ctx, seed(f.db))!;
  assert.equal(f.db.getSessionRuntime(fired.session.id).workspace, null);
  f.close();
});

test("firing never throws when the turn starter fails", () => {
  const f = fixture();
  const errors: unknown[] = [];
  f.ctx.startTurn = () => {
    throw new Error("no turn for you");
  };

  const fired = fireSchedule(f.ctx, seed(f.db), { reportError: (e) => errors.push(e) });
  // The session and its message survive — the user can see what was meant to run.
  assert.equal(fired, null);
  assert.equal(errors.length, 1);
  assert.equal(f.db.listSessions().length, 1);
  f.close();
});

test("firing with no turn starter wired still records the session", () => {
  const f = fixture();
  delete f.ctx.startTurn;
  const fired = fireSchedule(f.ctx, seed(f.db))!;
  assert.ok(fired);
  assert.equal(f.db.threadFor(fired.session.id).length, 1);
  f.close();
});

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

test("the real ticker fires once however many times it ticks past a missed slot", async () => {
  const f = fixture();
  seed(f.db, { spec: "every:1h", nextRunAt: T0 + HOUR });

  // The clock jumps six hours ahead of the schedule's next slot and STAYS there, so
  // every tick in the window sees a row that was due five slots ago. A loop that
  // caught up slot by slot — or one that failed to advance the row — would fire on
  // each of them.
  let clock = T0 + 6 * HOUR;
  const fired: Schedule[] = [];
  const stop = startScheduleTicker({ ...f.ctx, now: () => clock }, {
    intervalMs: 2,
    fire: (_ctx, s) => fired.push(s),
  });
  try {
    await new Promise((r) => setTimeout(r, 60));
  } finally {
    stop();
  }
  assert.equal(fired.length, 1, `expected exactly one firing, got ${fired.length}`);

  // Advance past the next slot and it fires again — the ticker is alive, not stuck.
  clock += HOUR;
  const stop2 = startScheduleTicker({ ...f.ctx, now: () => clock }, {
    intervalMs: 2,
    fire: (_ctx, s) => fired.push(s),
  });
  try {
    await new Promise((r) => setTimeout(r, 60));
  } finally {
    stop2();
  }
  assert.equal(fired.length, 2);
  f.close();
});

test("the ticker's stopper ends it", async () => {
  const f = fixture();
  seed(f.db, { nextRunAt: T0 - HOUR });
  let clock = T0;
  const fired: Schedule[] = [];
  const stop = startScheduleTicker({ ...f.ctx, now: () => clock }, {
    intervalMs: 2,
    fire: (_c, s) => fired.push(s),
  });
  await new Promise((r) => setTimeout(r, 30));
  stop();
  const after = fired.length;
  // Make the schedule due again; a stopped ticker must not notice.
  clock += 10 * HOUR;
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(fired.length, after);
  assert.equal(after, 1);
  f.close();
});

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

const TABLE: Route[] = [
  route("GET", "/schedules", listSchedulesH),
  route("POST", "/schedules", createScheduleH),
  route("PATCH", "/schedules/:id", patchScheduleH),
  route("DELETE", "/schedules/:id", deleteScheduleH),
];

const url = (path: string) => `http://127.0.0.1:4321${path}`;

test("the schedule routes are CRUD over the same validated path", async () => {
  const f = fixture();
  const call = createHandler(f.ctx, { routes: TABLE });

  const created = await call(
    new Request(url("/schedules"), {
      method: "POST",
      body: JSON.stringify({ title: "nightly", prompt: "run the suite", spec: "every:2h" }),
    }),
  );
  assert.equal(created.status, 201);
  const schedule = await created.json() as Schedule;
  assert.equal(schedule.nextRunAt, T0 + 2 * HOUR, "the ctx clock is the one used");

  const listed = await (await call(new Request(url("/schedules")))).json() as Schedule[];
  assert.deepEqual(listed.map((s) => s.id), [schedule.id]);

  const patched = await call(
    new Request(url(`/schedules/${schedule.id}`), {
      method: "PATCH",
      body: JSON.stringify({ enabled: false }),
    }),
  );
  assert.equal(patched.status, 200);
  assert.equal(((await patched.json()) as Schedule).enabled, false);

  const removed = await call(
    new Request(url(`/schedules/${schedule.id}`), { method: "DELETE" }),
  );
  assert.equal(removed.status, 200);
  assert.equal(f.db.listSchedules().length, 0);
  f.close();
});

test("POST /schedules rejects a bad spec as a 400 naming the grammar", async () => {
  const f = fixture();
  const call = createHandler(f.ctx, { routes: TABLE });
  const res = await call(
    new Request(url("/schedules"), {
      method: "POST",
      body: JSON.stringify({ title: "t", prompt: "p", spec: "0 9 * * *" }),
    }),
  );
  assert.equal(res.status, 400);
  assert.match((await res.json()).error, /every:<N><m\|h\|d>/);
  f.close();
});

test("PATCH and DELETE on an unknown schedule are 404s", async () => {
  const f = fixture();
  const call = createHandler(f.ctx, { routes: TABLE });
  const patched = await call(
    new Request(url("/schedules/nope"), { method: "PATCH", body: "{}" }),
  );
  assert.equal(patched.status, 404);
  const deleted = await call(new Request(url("/schedules/nope"), { method: "DELETE" }));
  assert.equal(deleted.status, 404);
  f.close();
});

test("an empty PATCH body is a legal no-op, not a 400", async () => {
  const f = fixture();
  const created = await scheduleCreate(f.db, { title: "t", prompt: "p", spec: "every:1h" }, {
    now: () => T0,
  });
  const call = createHandler(f.ctx, { routes: TABLE });
  const res = await call(new Request(url(`/schedules/${created.id}`), { method: "PATCH" }));
  assert.equal(res.status, 200);
  assert.equal(((await res.json()) as Schedule).nextRunAt, created.nextRunAt);
  f.close();
});
