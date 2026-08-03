/**
 * Schedule grammar, next-run math, and the validated CRUD (T6.3).
 *
 * The heaviest coverage is on the pure half, which is where the invariant lives: the
 * catch-up rule is arithmetic (`nextRun` measures from the instant it is handed), and
 * a test can prove it in a millisecond where the ticker test has to simulate
 * downtime. `schedules.test.ts` covers the loop that consumes it.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve. (Same constraint `hostfn/shell.test.ts` and `bus.test.ts` document.)
 */

import { test } from "bun:test";
import assert from "node:assert";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { ScheduleError } from "../errors.ts";
import type { Schedule } from "../schema/parts.ts";
import type { TurnCtx } from "../types.ts";
import {
  createScheduleHostFn,
  nextRun,
  parseSpec,
  scheduleCreate,
  schedulePatch,
  scheduleRemove,
  scheduleVerb,
} from "./schedule.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const T0 = Date.UTC(2026, 0, 15, 12, 0, 0);
const MINUTE = 60_000;
const HOUR = 3_600_000;

/** A workspace resolver that accepts anything — no directory is touched. */
const anyWorkspace = (raw: string) => Promise.resolve(`/resolved${raw}`);

function db(): SqliteDb {
  return openDb(":memory:");
}

function turnCtx(store: SqliteDb, workspace = "/work/repo"): TurnCtx {
  return {
    db: store,
    bus: new Bus(),
    sessionId: "session-1",
    turnId: "turn-1",
    messageId: "message-1",
    workspace,
    model: "test-model",
    signal: new AbortController().signal,
    depth: 0,
    now: () => T0,
  };
}

async function seed(store: SqliteDb, over: Partial<Schedule> = {}): Promise<Schedule> {
  const created = await scheduleCreate(
    store,
    { title: "nightly", prompt: "check the deploy", spec: "every:30m" },
    { now: () => T0, workspace: anyWorkspace },
  );
  if (Object.keys(over).length === 0) return created;
  const merged = { ...created, ...over };
  store.updateSchedule(merged);
  return store.getSchedule(created.id)!;
}

/** Assert a thrown `ScheduleError` with the expected status and message fragment. */
async function rejects(fn: () => unknown, status: number, fragment: string): Promise<void> {
  try {
    await fn();
  } catch (err) {
    assert.ok(err instanceof ScheduleError, `expected ScheduleError, got ${err}`);
    assert.equal(err.status, status, err.message);
    assert.ok(
      err.message.includes(fragment),
      `expected message to mention ${JSON.stringify(fragment)}, got: ${err.message}`,
    );
    return;
  }
  assert.fail("expected a ScheduleError");
}

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

test("parseSpec accepts every:<N><m|h|d>", () => {
  assert.deepEqual(parseSpec("every:30m"), { kind: "every", ms: 30 * MINUTE });
  assert.deepEqual(parseSpec("every:2h"), { kind: "every", ms: 2 * HOUR });
  assert.deepEqual(parseSpec("every:1d"), { kind: "every", ms: 86_400_000 });
});

test("parseSpec accepts daily@HH:MM", () => {
  assert.deepEqual(parseSpec("daily@09:00"), { kind: "daily", hh: 9, mm: 0 });
  assert.deepEqual(parseSpec("daily@9:05"), { kind: "daily", hh: 9, mm: 5 });
  assert.deepEqual(parseSpec("daily@23:59"), { kind: "daily", hh: 23, mm: 59 });
});

test("parseSpec rejects everything else", () => {
  for (
    const bad of [
      "",
      "every:0m", // N ≥ 1 — a zero interval is always due, on every tick, forever
      "every:m",
      "every:5s",
      "every:5w",
      "every: 5m",
      "EVERY:5m",
      "daily@24:00",
      "daily@09:60",
      "daily@9",
      "daily@09:00:00",
      "0 9 * * *", // cron is NOT the grammar
      "hourly",
    ]
  ) {
    assert.equal(parseSpec(bad), null, `expected ${JSON.stringify(bad)} to be rejected`);
  }
});

// ---------------------------------------------------------------------------
// nextRun
// ---------------------------------------------------------------------------

test("nextRun for every: adds the interval to the instant it is given", () => {
  assert.equal(nextRun("every:30m", T0), T0 + 30 * MINUTE);
  // The invariant, stated as arithmetic: five hours of downtime does not compound.
  // Whatever `from` is, the answer is exactly one interval later.
  assert.equal(nextRun("every:30m", T0 + 5 * HOUR), T0 + 5 * HOUR + 30 * MINUTE);
});

test("nextRun for daily@ lands at the next local wall-clock occurrence", () => {
  // Local time, so the assertion is built with the local constructor rather than UTC.
  const morning = new Date(2026, 0, 15, 8, 0, 0, 0).getTime();
  const nine = new Date(2026, 0, 15, 9, 0, 0, 0).getTime();
  assert.equal(nextRun("daily@09:00", morning), nine);

  // Already past today → tomorrow, same wall clock.
  const afternoon = new Date(2026, 0, 15, 14, 0, 0, 0).getTime();
  assert.equal(nextRun("daily@09:00", afternoon), new Date(2026, 0, 16, 9, 0, 0, 0).getTime());

  // Exactly at the slot is NOT "now again": strictly after, or the row stays due.
  assert.equal(nextRun("daily@09:00", nine), new Date(2026, 0, 16, 9, 0, 0, 0).getTime());
});

test("nextRun throws on a spec that does not parse", () => {
  assert.throws(() => nextRun("weekly", T0), ScheduleError);
});

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

test("scheduleCreate stores the row with next_run_at one interval out", async () => {
  const store = db();
  const created = await scheduleCreate(
    store,
    { title: "deploy check", prompt: "check the deploy", spec: "every:2h" },
    { now: () => T0, workspace: anyWorkspace },
  );
  assert.equal(created.enabled, true);
  assert.equal(created.workspace, null);
  assert.equal(created.lastRunAt, null);
  assert.equal(created.createdAt, T0);
  assert.equal(created.nextRunAt, T0 + 2 * HOUR);
  assert.deepEqual(store.listSchedules().map((s) => s.id), [created.id]);
  store.close();
});

test("scheduleCreate rejects a bad spec with the grammar", async () => {
  const store = db();
  await rejects(
    () =>
      scheduleCreate(store, { title: "t", prompt: "p", spec: "hourly" }, {
        now: () => T0,
        workspace: anyWorkspace,
      }),
    400,
    "every:<N><m|h|d>",
  );
  assert.equal(store.listSchedules().length, 0);
  store.close();
});

test("scheduleCreate resolves the workspace through the injected resolver", async () => {
  const store = db();
  const created = await scheduleCreate(
    store,
    { title: "t", prompt: "p", spec: "every:1d", workspace: "~/repo" },
    { now: () => T0, workspace: anyWorkspace },
  );
  assert.equal(created.workspace, "/resolved~/repo");
  store.close();
});

test("scheduleCreate surfaces a workspace that does not exist", async () => {
  const store = db();
  await rejects(
    () =>
      scheduleCreate(store, { title: "t", prompt: "p", spec: "every:1d", workspace: "/nope" }, {
        now: () => T0,
        workspace: () => Promise.reject(new ScheduleError(400, "workspace does not exist: /nope")),
      }),
    400,
    "workspace does not exist",
  );
  store.close();
});

test("schedulePatch leaves next_run_at alone for a cosmetic edit", async () => {
  const store = db();
  const created = await seed(store);
  const patched = await schedulePatch(store, created.id, { title: "renamed" }, {
    now: () => T0 + 5 * MINUTE,
    workspace: anyWorkspace,
  });
  assert.equal(patched.title, "renamed");
  assert.equal(patched.nextRunAt, created.nextRunAt);
  store.close();
});

test("schedulePatch recomputes next_run_at from now when the spec changes", async () => {
  const store = db();
  const created = await seed(store);
  const at = T0 + 5 * MINUTE;
  const patched = await schedulePatch(store, created.id, { spec: "every:2h" }, {
    now: () => at,
    workspace: anyWorkspace,
  });
  assert.equal(patched.nextRunAt, at + 2 * HOUR);
  store.close();
});

test("re-enabling recomputes from now — the disabled stretch is not downtime", async () => {
  const store = db();
  const created = await seed(store);
  await schedulePatch(store, created.id, { enabled: false }, {
    now: () => T0,
    workspace: anyWorkspace,
  });

  // A week later. If re-enabling kept the stale next_run_at, the schedule would be
  // due the instant it was switched back on — which is not what "enable" means.
  const at = T0 + 7 * 24 * HOUR;
  const reenabled = await schedulePatch(store, created.id, { enabled: true }, {
    now: () => at,
    workspace: anyWorkspace,
  });
  assert.equal(reenabled.enabled, true);
  assert.equal(reenabled.nextRunAt, at + 30 * MINUTE);
  assert.equal(store.dueSchedules(at).length, 0);
  store.close();
});

test("disabling does not recompute, and a disabled row is never due", async () => {
  const store = db();
  const created = await seed(store);
  const patched = await schedulePatch(store, created.id, { enabled: false }, {
    now: () => T0 + MINUTE,
    workspace: anyWorkspace,
  });
  assert.equal(patched.nextRunAt, created.nextRunAt);
  assert.equal(store.dueSchedules(T0 + 10 * HOUR).length, 0);
  store.close();
});

test("schedulePatch clears the workspace with an explicit null", async () => {
  const store = db();
  const created = await scheduleCreate(
    store,
    { title: "t", prompt: "p", spec: "every:1d", workspace: "/repo" },
    { now: () => T0, workspace: anyWorkspace },
  );
  const patched = await schedulePatch(store, created.id, { workspace: null }, {
    now: () => T0,
    workspace: anyWorkspace,
  });
  assert.equal(patched.workspace, null);
  store.close();
});

test("patching and removing an unknown id is a 404, not a silent success", async () => {
  const store = db();
  await rejects(() => schedulePatch(store, "nope", { title: "x" }), 404, "not found");
  await rejects(() => Promise.resolve(scheduleRemove(store, "nope")), 404, "not found");
  store.close();
});

test("scheduleRemove deletes the row", async () => {
  const store = db();
  const created = await seed(store);
  scheduleRemove(store, created.id);
  assert.equal(store.getSchedule(created.id), undefined);
  store.close();
});

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

test("scheduleVerb add defaults the workspace to the caller's", async () => {
  const store = db();
  const added = await scheduleVerb(
    store,
    "add",
    { title: "t", prompt: "p", spec: "every:1h" },
    "/work/repo",
    { now: () => T0, workspace: anyWorkspace },
  ) as Schedule;
  assert.equal(added.workspace, "/resolved/work/repo");
  store.close();
});

test("scheduleVerb add keeps an explicit workspace over the default", async () => {
  const store = db();
  const added = await scheduleVerb(
    store,
    "add",
    { title: "t", prompt: "p", spec: "every:1h", workspace: "/elsewhere" },
    "/work/repo",
    { now: () => T0, workspace: anyWorkspace },
  ) as Schedule;
  assert.equal(added.workspace, "/resolved/elsewhere");
  store.close();
});

test("scheduleVerb add reports a malformed argument object", async () => {
  const store = db();
  await rejects(
    () => scheduleVerb(store, "add", { title: "t" }, null, { now: () => T0 }),
    400,
    "schedule.add",
  );
  store.close();
});

test("scheduleVerb enable/disable/remove take a bare id string", async () => {
  const store = db();
  const created = await seed(store);

  const disabled = await scheduleVerb(store, "disable", created.id, null, {
    now: () => T0,
    workspace: anyWorkspace,
  }) as Schedule;
  assert.equal(disabled.enabled, false);

  const enabled = await scheduleVerb(store, "enable", created.id, null, {
    now: () => T0 + HOUR,
    workspace: anyWorkspace,
  }) as Schedule;
  assert.equal(enabled.enabled, true);

  assert.deepEqual(
    await scheduleVerb(store, "remove", created.id, null, { now: () => T0 }),
    { ok: true, removed: created.id },
  );
  store.close();
});

test("scheduleVerb says how to call a verb that got the wrong argument", async () => {
  const store = db();
  await rejects(() => scheduleVerb(store, "enable", { id: "x" }, null), 400, "as a string");
  store.close();
});

test("scheduleVerb names the verbs when it gets an unknown one", async () => {
  const store = db();
  await rejects(() => scheduleVerb(store, "pause", null, null), 400, "list, add, enable");
  store.close();
});

// ---------------------------------------------------------------------------
// The bridged host function
// ---------------------------------------------------------------------------

test("the schedule host fn takes JSON in and returns JSON out", async () => {
  const store = db();
  const { schedule } = createScheduleHostFn(turnCtx(store), { workspace: anyWorkspace });

  const added = JSON.parse(
    await schedule!("add", JSON.stringify({ title: "t", prompt: "p", spec: "every:15m" })),
  ) as Schedule;
  assert.equal(added.nextRunAt, T0 + 15 * MINUTE);
  // The session's own checkout is the default — a schedule made from a program must
  // not silently target the server's cwd months later.
  assert.equal(added.workspace, "/resolved/work/repo");
  // The calling conversation is stamped as where firings report back — from the
  // ctx, never from the wire (`ScheduleDeps.sessionId`).
  assert.equal(added.sessionId, "session-1");

  const listed = JSON.parse(await schedule!("list", "null")) as Schedule[];
  assert.deepEqual(listed.map((s) => s.id), [added.id]);

  const removed = JSON.parse(await schedule!("remove", JSON.stringify(added.id)));
  assert.deepEqual(removed, { ok: true, removed: added.id });
  store.close();
});

test("the schedule host fn rejects non-JSON arguments catchably", async () => {
  const store = db();
  const { schedule } = createScheduleHostFn(turnCtx(store));
  await rejects(() => schedule!("add", "{not json"), 400, "valid JSON");
  store.close();
});
