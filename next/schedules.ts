/**
 * The schedule ticker and its REST surface: what actually makes a recurring run
 * happen (spec §9).
 *
 * THE INVARIANT THIS HOLDS: **a schedule that missed N slots fires ONCE.** The
 * arithmetic that guarantees it lives in `hostfn/schedule.ts` (`nextRun(spec, now)`
 * always advances from *now*); this file is the other half — the loop that reads
 * "due" as a boolean about one row rather than as a count of elapsed slots. Three
 * details carry it, and each is a bug if it slips:
 *
 *   1. `dueSchedules(now)` returns each enabled row **once**, however far behind it
 *      is. There is no inner loop walking a stale `next_run_at` forward slot by slot,
 *      because that loop is exactly the burst — a laptop shut overnight would wake up
 *      and open sixteen sessions before the user finished logging in.
 *   2. The advance happens **before** the fire, not after. A `fire` that throws must
 *      not leave the row still due: the very next tick, 30 seconds later, would fire
 *      it again, and again, for as long as the failure lasts.
 *   3. `now` is threaded in, never read inside. One tick uses one instant for both the
 *      due test and the advance, so a slow pass cannot skip a slot it should have
 *      caught, and a test can drive five hours of downtime in a millisecond.
 *
 * WHAT FIRING IS. A fresh **root** session titled from the schedule, with the
 * schedule's prompt posted into it as an ordinary user message. Not a branch, not a
 * subagent: the prompt has to stand alone anyway (the fired session sees no prior
 * conversation), and a root is the thing that shows up in the session list, forks,
 * and can be adopted — after the first turn starts it is indistinguishable from a
 * session the user opened by hand.
 *
 * WHY THE TICKER TAKES A CTX RATHER THAN A DATABASE. Firing needs the bus (to
 * announce the new session) and the turn starter (to run the prompt), and both arrive
 * on the ctx that `server/main.ts` builds. `startTurn` is read off it structurally,
 * the same seam `server/sessions.ts` and `agents/notes.ts` use — an unwired starter
 * degrades to "the session exists with an unanswered message", never to a crash.
 *
 * Ported from `src/schedules.ts` (`fireSchedule`, `tickSchedules`,
 * `startScheduleTicker`, and the REST handlers in `src/server/app.ts`). Deltas from
 * that port are marked `NOTE:`.
 */
import { CreateScheduleBody, PatchScheduleBody } from "./schema/requests.ts";
import type { Message, Schedule, Session } from "./schema/parts.ts";
import type { AppCtx, Db } from "./types.ts";
import {
  nextRun,
  scheduleCreate,
  type ScheduleDeps,
  schedulePatch,
  scheduleRemove,
} from "./hostfn/schedule.ts";
import { json, parseBody } from "./server/http.ts";
import type { WithTurnStarter } from "./server/sessions.ts";

// ---------------------------------------------------------------------------
// Firing
// ---------------------------------------------------------------------------

/** The production interval. ~30s, per spec §9. */
export const TICK_MS = 30_000;

export interface FireDeps extends ScheduleDeps {
  /** Where a failure to start the fired turn is reported. Tests pass a collector. */
  reportError?: (error: unknown, schedule: Schedule) => void;
}

/** What a firing produced, for the caller that wants to assert on it. */
export interface FiredSchedule {
  session: Session;
  message: Message;
}

/**
 * Fire one schedule: a fresh root session, the prompt as its first user message, and
 * a turn on it.
 *
 * The session is announced before the message, and the message before the turn, so a
 * live TUI renders the new session already carrying its prompt rather than an empty
 * card that fills in a beat later.
 *
 * **This never throws.** It is called from a timer callback with nobody to report to;
 * a throw would surface as an unhandled rejection and take the server down, losing
 * every other session with it. A failure to start the turn leaves the session and its
 * message persisted — the user can see what was supposed to run and post into it.
 */
export function fireSchedule(
  ctx: AppCtx,
  schedule: Schedule,
  deps: FireDeps = {},
): FiredSchedule | null {
  const now = deps.now ?? ctx.now ?? Date.now;
  const report = deps.reportError ??
    ((error: unknown, s: Schedule) =>
      console.error(`schedule ${s.id} (${s.title}) failed to fire:`, error));

  try {
    const session = ctx.db.createSession({
      id: crypto.randomUUID(),
      title: schedule.title,
      kind: "root",
      // A fired session inherits nothing: no parent thread, no lineage edge. The
      // prompt is the whole briefing, which is what the prompt section tells the
      // model when it writes one (`prompt/schedule.md`).
      parentId: null,
      createdAt: now(),
      ...(schedule.workspace
        ? { workspace: schedule.workspace, originDir: schedule.workspace }
        : {}),
    });
    ctx.bus.publish({ type: "session.created", sessionId: session.id, data: session });

    const message = ctx.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: session.id,
      role: "user",
      parts: [{ type: "text", text: schedule.prompt }],
      pending: false,
      createdAt: now(),
    });
    indexQuietly(ctx.db, message);
    ctx.bus.publish({ type: "message.started", sessionId: session.id, data: message });

    // Fire and forget, like the HTTP post path: the turn runs for minutes and there
    // is no response to hold open. An absent starter is not an error — it is the
    // pre-M2 shape, and the session is still there with its prompt.
    const start = (ctx as AppCtx & WithTurnStarter).startTurn;
    if (start) {
      const running = start(ctx, session, message);
      if (running instanceof Promise) running.catch((err) => report(err, schedule));
    }
    return { session, message };
  } catch (err) {
    report(err, schedule);
    return null;
  }
}

/** Indexing failure is a degraded search, never a lost firing. */
function indexQuietly(db: Db, message: Message): void {
  try {
    db.indexMessage(message);
  } catch (err) {
    console.error(`failed to index message ${message.id}:`, err);
  }
}

// ---------------------------------------------------------------------------
// One tick
// ---------------------------------------------------------------------------

/**
 * One ticker pass at `now`. Returns the schedules that fired, in order.
 *
 * Read the two statements in the loop together — they are the catch-up rule:
 * `markScheduleRun` stamps `last_run_at = now` and `next_run_at = nextRun(spec, now)`
 * **before** `fire` runs, so the row is no longer due whatever happens next, and the
 * new time is measured from this instant rather than from the slot that was missed.
 *
 * `fire` is a parameter rather than a call to `fireSchedule` so a test can drive the
 * loop with a counter and prove the burst does not happen without a database full of
 * sessions or a single LLM call.
 */
export function tickSchedules(
  db: Db,
  now: number,
  fire: (schedule: Schedule) => void,
): Schedule[] {
  const due = db.dueSchedules(now);
  for (const schedule of due) {
    db.markScheduleRun(schedule.id, now, nextRun(schedule.spec, now));
    try {
      fire(schedule);
    } catch (err) {
      // `fireSchedule` does not throw; a test fake or a future caller might. One
      // schedule's failure must not skip the rest of this pass.
      console.error(`schedule ${schedule.id} (${schedule.title}) failed to fire:`, err);
    }
  }
  return due;
}

export interface TickerDeps extends FireDeps {
  /** Defaults to `TICK_MS`. */
  intervalMs?: number;
  /** Defaults to `fireSchedule`. Tests observe firings here. */
  fire?: (ctx: AppCtx, schedule: Schedule) => unknown;
}

/**
 * The production loop: `tickSchedules` on a ~30s interval. Returns a stopper.
 *
 * The timer is unref'd so the ticker alone never keeps a process alive — a CLI or a
 * test that starts one and finishes its work should exit, not hang for 30 seconds
 * waiting for a tick nobody reads.
 *
 * NOTE: no immediate pass at boot. The first tick lands one interval in, which gives
 * a server that is still recovering orphaned turns (`server/main.ts`) a moment before
 * it starts opening new sessions. A schedule due right now waits at most 30 seconds,
 * which is inside the cadence anything expressible in this grammar asked for.
 */
export function startScheduleTicker(ctx: AppCtx, deps: TickerDeps = {}): () => void {
  const now = deps.now ?? ctx.now ?? Date.now;
  const fire = deps.fire ?? ((c: AppCtx, s: Schedule) => fireSchedule(c, s, deps));
  const timer = setInterval(() => {
    try {
      tickSchedules(ctx.db, now(), (schedule) => fire(ctx, schedule));
    } catch (err) {
      // A throwing tick must not kill the interval — the next pass may well work,
      // and a silently dead ticker is a feature that stops existing with no signal.
      console.error("schedule tick failed:", err);
    }
  }, deps.intervalMs ?? TICK_MS);
  Deno.unrefTimer(timer);
  return () => clearInterval(timer);
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/**
 * The four handlers are `function` DECLARATIONS, not `const` arrows, and that is
 * load-bearing rather than stylistic.
 *
 * This module and `server/app.ts` form an import cycle — app.ts imports these
 * handlers for its route table, and this file imports app.ts's `json`/`parseBody`.
 * A cycle is fine when every binding read during module evaluation is already
 * initialized, and hoisted function declarations are: they exist from module
 * *instantiation*, before any body runs. A `const` handler would be in its temporal
 * dead zone whenever this module happens to be evaluated first (a test importing
 * `schedules.ts` directly does exactly that), and app.ts's route table would throw
 * building itself. Same reason app.ts's own `json`, `parseBody` and `route` are
 * declarations.
 */

/**
 * `GET /schedules` — every schedule, in creation order.
 *
 * No visibility derivation here: schedules are flat and few, and the panel shows
 * disabled ones too — that is how you re-enable one.
 */
export function listSchedulesH(_req: Request, ctx: AppCtx): Response {
  return json(ctx.db.listSchedules());
}

/** `POST /schedules` — 201 with the stored row, `next_run_at` already computed. */
export async function createScheduleH(req: Request, ctx: AppCtx): Promise<Response> {
  const body = await parseBody(req, CreateScheduleBody);
  return json(await scheduleCreate(ctx.db, body, { now: ctx.now }), 201);
}

/** `PATCH /schedules/:id` — partial update; `workspace: null` clears it. */
export async function patchScheduleH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  // `{}` as the fallback: every field is optional, so an empty body is a legal
  // no-op patch rather than a 400 about a missing object.
  const body = await parseBody(req, PatchScheduleBody, {});
  return json(await schedulePatch(ctx.db, params.id, body, { now: ctx.now }));
}

/** `DELETE /schedules/:id` — 404 on an unknown id rather than a silent success. */
export function deleteScheduleH(
  _req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Response {
  scheduleRemove(ctx.db, params.id);
  return json({ ok: true, removed: params.id });
}
