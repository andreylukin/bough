import { assert, assertEquals, assertRejects, assertThrows } from "jsr:@std/assert@1";
import { Bus } from "./bus.ts";
import { Db, type Schedule } from "./db/db.ts";
import type { LlmClient } from "./supervisor/llm.ts";
import { fireSchedule, nextRun, parseSpec, scheduleVerb, tickSchedules } from "./schedules.ts";

// Local wall-clock helper so the daily@ tests hold in any timezone.
const local = (y: number, mo: number, d: number, h: number, mi: number) =>
  new Date(y, mo - 1, d, h, mi).getTime();

Deno.test("parseSpec: accepts the grammar, rejects everything else", () => {
  assertEquals(parseSpec("every:30m"), { kind: "every", ms: 30 * 60_000 });
  assertEquals(parseSpec("every:2h"), { kind: "every", ms: 2 * 3_600_000 });
  assertEquals(parseSpec("every:1d"), { kind: "every", ms: 86_400_000 });
  assertEquals(parseSpec("daily@09:00"), { kind: "daily", hh: 9, mm: 0 });
  assertEquals(parseSpec("daily@9:05"), { kind: "daily", hh: 9, mm: 5 });
  assertEquals(parseSpec("daily@23:59"), { kind: "daily", hh: 23, mm: 59 });
  for (
    const bad of [
      "",
      "hourly",
      "every:0m", // N ≥ 1
      "every:30s", // no seconds unit
      "every:m",
      "every:30",
      "daily@24:00", // hour out of range
      "daily@10:60", // minute out of range
      "daily@9", // minutes required
      "daily@9:5", // minutes are two digits
      " every:30m",
    ]
  ) assertEquals(parseSpec(bad), null, `should reject ${JSON.stringify(bad)}`);
});

Deno.test("nextRun: every:<N> advances a fixed interval from `from`", () => {
  const from = local(2026, 7, 20, 10, 0);
  assertEquals(nextRun("every:30m", from), from + 30 * 60_000);
  assertEquals(nextRun("every:1d", from), from + 86_400_000);
  assertThrows(() => nextRun("nonsense", from));
});

Deno.test("nextRun: daily@ before today's slot → today; at/after → tomorrow", () => {
  // 08:00 with a 09:00 schedule → today 09:00.
  assertEquals(nextRun("daily@09:00", local(2026, 7, 20, 8, 0)), local(2026, 7, 20, 9, 0));
  // 14:30, slot already past → tomorrow 09:00.
  assertEquals(nextRun("daily@09:00", local(2026, 7, 20, 14, 30)), local(2026, 7, 21, 9, 0));
  // Exactly at the slot → strictly after, so tomorrow (a fire at 09:00 must not
  // compute its next run as the same instant).
  assertEquals(nextRun("daily@09:00", local(2026, 7, 20, 9, 0)), local(2026, 7, 21, 9, 0));
  // Month rollover.
  assertEquals(nextRun("daily@06:15", local(2026, 7, 31, 23, 0)), local(2026, 8, 1, 6, 15));
});

// ---- ticker (injected clock + fake fire; no LLM, no timers) ----------------

function sched(db: Db, patch: Partial<Schedule>): Schedule {
  return db.createSchedule({
    id: crypto.randomUUID(),
    title: "t",
    prompt: "p",
    workspace: null,
    spec: "every:30m",
    enabled: true,
    createdAt: 0,
    lastRunAt: null,
    nextRunAt: 0,
    ...patch,
  });
}

Deno.test("tickSchedules: fires due schedules once and stamps last/next run", () => {
  const db = new Db(":memory:");
  const now = local(2026, 7, 20, 10, 0);
  const due = sched(db, { spec: "every:30m", nextRunAt: now });
  const later = sched(db, { spec: "every:30m", nextRunAt: now + 60_000 });
  const off = sched(db, { spec: "every:30m", nextRunAt: now, enabled: false });

  const fired: string[] = [];
  assertEquals(tickSchedules(db, now, (s) => fired.push(s.id)), 1);
  assertEquals(fired, [due.id]);
  assertEquals(db.getSchedule(due.id)?.lastRunAt, now);
  assertEquals(db.getSchedule(due.id)?.nextRunAt, now + 30 * 60_000);
  // The others are untouched.
  assertEquals(db.getSchedule(later.id)?.lastRunAt, null);
  assertEquals(db.getSchedule(off.id)?.lastRunAt, null);
  // An immediate second tick has nothing due.
  assertEquals(tickSchedules(db, now, (s) => fired.push(s.id)), 0);
  assertEquals(fired.length, 1);
  db.close();
});

Deno.test("tickSchedules: catch-up after downtime fires ONCE, advances from now", () => {
  const db = new Db(":memory:");
  const now = local(2026, 7, 20, 10, 0);
  // every:30m that was due 5h ago — 10 slots were missed while the server was down.
  const s = sched(db, { spec: "every:30m", nextRunAt: now - 5 * 3_600_000 });

  const fired: string[] = [];
  assertEquals(tickSchedules(db, now, (x) => fired.push(x.id)), 1);
  assertEquals(fired, [s.id], "one catch-up fire, not one per missed slot");
  // Advanced from NOW, not from the stale next_run_at.
  assertEquals(db.getSchedule(s.id)?.nextRunAt, now + 30 * 60_000);
  assertEquals(tickSchedules(db, now + 1, () => fired.push("again")), 0);
  db.close();
});

Deno.test("tickSchedules: daily@ catch-up lands on the next wall-clock slot", () => {
  const db = new Db(":memory:");
  // daily@09:00, server slept through three 09:00s; it's now 14:00.
  const now = local(2026, 7, 20, 14, 0);
  const s = sched(db, { spec: "daily@09:00", nextRunAt: local(2026, 7, 17, 9, 0) });

  let fires = 0;
  assertEquals(tickSchedules(db, now, () => fires++), 1);
  assertEquals(fires, 1);
  assertEquals(db.getSchedule(s.id)?.nextRunAt, local(2026, 7, 21, 9, 0));
  db.close();
});

// ---- the schedule.* host fn (shared validated CRUD) ------------------------

Deno.test("scheduleVerb: add/list/enable/disable/remove through the shared module", async () => {
  const db = new Db(":memory:");
  const ws = Deno.makeTempDirSync({ prefix: "sched-ws-" });

  // add — workspace omitted defaults to the calling session's.
  const added = await scheduleVerb(
    db,
    "add",
    { title: "morning check", prompt: "check the deploy", spec: "daily@09:00" },
    ws,
  ) as Schedule;
  assertEquals(added.title, "morning check");
  assertEquals(added.workspace, ws);
  assertEquals(added.enabled, true);
  assert(added.nextRunAt > Date.now());

  const listed = await scheduleVerb(db, "list", null) as Schedule[];
  assertEquals(listed.map((s) => s.id), [added.id]);

  // disable / enable round-trip; re-enable recomputes next_run_at from now.
  const off = await scheduleVerb(db, "disable", added.id) as Schedule;
  assertEquals(off.enabled, false);
  const on = await scheduleVerb(db, "enable", added.id) as Schedule;
  assertEquals(on.enabled, true);
  assert(on.nextRunAt > Date.now());

  assertEquals(await scheduleVerb(db, "remove", added.id), { ok: true, removed: added.id });
  assertEquals((await scheduleVerb(db, "list", null) as Schedule[]).length, 0);

  // Validation goes through the same code path as REST: bad spec, bad workspace,
  // unknown id, unknown verb all reject.
  await assertRejects(
    () => scheduleVerb(db, "add", { title: "t", prompt: "p", spec: "hourly" }),
    Error,
    "invalid spec",
  );
  await assertRejects(
    () =>
      scheduleVerb(db, "add", {
        title: "t",
        prompt: "p",
        spec: "every:1h",
        workspace: "/nope/zzz",
      }),
    Error,
    "does not exist",
  );
  await assertRejects(() => scheduleVerb(db, "enable", "zzz"), Error, "not found");
  await assertRejects(() => scheduleVerb(db, "remove", ""), Error, "id (string) required");
  await assertRejects(() => scheduleVerb(db, "explode", null), Error, "unknown schedule verb");
  db.close();
});

Deno.test("fireSchedule: creates a titled root session, announces it, runs the prompt", async () => {
  const db = new Db(":memory:");
  const bus = new Bus();
  const created: string[] = [];
  bus.subscribe((e) => e.type === "session.created" && created.push(e.sessionId ?? ""));
  // Scripted client: answers the harness's stop-nudge immediately, so the turn ends.
  const llm: LlmClient = {
    run: (_params, _onText) =>
      Promise.resolve({
        content: [{ type: "tool_use", id: "stop-1", name: "stop", input: {} }],
        stopReason: "tool_use",
      }),
  };
  const finished = new Promise<void>((resolve) => {
    bus.subscribe((e) => e.type === "message.finished" && resolve());
  });

  const session = fireSchedule({ db, bus, llm }, {
    id: "sch1",
    title: "morning check",
    prompt: "check the deploy",
    workspace: null,
    spec: "daily@09:00",
    enabled: true,
    createdAt: 0,
    lastRunAt: null,
    nextRunAt: 0,
  });
  await finished;

  assertEquals(created, [session.id]);
  const got = db.getSession(session.id);
  assertEquals(got?.title, "morning check");
  assertEquals(got?.kind, "root");
  const msgs = db.messagesFor(session.id);
  assertEquals(msgs[0].role, "user");
  assertEquals(msgs[0].parts, [{ type: "text", text: "check the deploy" }]);
  db.close();
});

Deno.test("tickSchedules: a throwing fire still advances (no hot-loop)", () => {
  const db = new Db(":memory:");
  const now = local(2026, 7, 20, 10, 0);
  const s = sched(db, { spec: "every:1h", nextRunAt: now });
  assertEquals(
    tickSchedules(db, now, () => {
      throw new Error("boom");
    }),
    1,
  );
  assert((db.getSchedule(s.id)?.nextRunAt ?? 0) > now);
  assertEquals(tickSchedules(db, now + 1, () => {}), 0);
  db.close();
});
