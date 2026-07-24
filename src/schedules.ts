/**
 * Recurring agent runs. A schedule is a stored (title, prompt, workspace, spec)
 * tuple; the server ticker (startScheduleTicker, ~30s) fires each enabled schedule
 * whose next_run_at has passed by creating a fresh root session titled from the
 * schedule and starting a turn with its prompt — the session then behaves like any
 * other (TUI picker, forks, adoption).
 *
 * Spec grammar (validated at create/edit time by parseSpec):
 *   every:<N><m|h|d>   — fixed interval, N ≥ 1 (every:30m, every:2h, every:1d)
 *   daily@HH:MM        — once a day at a LOCAL wall-clock time (daily@09:00)
 *
 * Catch-up semantics: next_run_at is always advanced FROM NOW at fire time
 * (nextRun(spec, now)), never from the stale next_run_at. So a server that was
 * down through N missed slots fires ONCE on the first tick after boot, then
 * resumes the cadence — no burst of N make-up runs.
 */
import { z } from "zod";
import { HttpError } from "./errors.ts";
import type { Db, Schedule } from "./db/db.ts";
import { startUserTurn, type TurnCtx } from "./turn.ts";
import type { Session } from "./schema/parts.ts";
import { normalizeWorkspace, workspaceProblem } from "./supervisor/workspace.ts";

// ---- spec parsing + next-run math (pure; `now` injected for tests) ---------

export type ParsedSpec =
  | { kind: "every"; ms: number }
  | { kind: "daily"; hh: number; mm: number };

const UNIT_MS: Record<string, number> = { m: 60_000, h: 3_600_000, d: 86_400_000 };

/** Parse a spec string, or null when it doesn't match the grammar. */
export function parseSpec(spec: string): ParsedSpec | null {
  const every = /^every:(\d+)(m|h|d)$/.exec(spec);
  if (every) {
    const n = Number(every[1]);
    if (n < 1) return null;
    return { kind: "every", ms: n * UNIT_MS[every[2]] };
  }
  const daily = /^daily@(\d{1,2}):(\d{2})$/.exec(spec);
  if (daily) {
    const hh = Number(daily[1]);
    const mm = Number(daily[2]);
    if (hh > 23 || mm > 59) return null;
    return { kind: "daily", hh, mm };
  }
  return null;
}

/**
 * The next fire time strictly after `from` (epoch ms). daily@ resolves in LOCAL
 * time via Date; a wall-clock time already past today lands tomorrow. Date's
 * calendar arithmetic absorbs DST — the run stays at HH:MM local either side of
 * a transition.
 */
export function nextRun(spec: string | ParsedSpec, from: number): number {
  const p = typeof spec === "string" ? parseSpec(spec) : spec;
  if (!p) throw new Error(`invalid schedule spec: ${spec}`);
  if (p.kind === "every") return from + p.ms;
  const d = new Date(from);
  d.setHours(p.hh, p.mm, 0, 0);
  if (d.getTime() <= from) d.setDate(d.getDate() + 1);
  return d.getTime();
}

// ---- request bodies (validated at the app.ts edge) -------------------------

export const ScheduleCreateBody = z.object({
  title: z.string().min(1),
  prompt: z.string().min(1),
  workspace: z.string().min(1).optional(),
  spec: z.string(),
  enabled: z.boolean().optional(),
});
export type ScheduleCreateBody = z.infer<typeof ScheduleCreateBody>;

export const SchedulePatchBody = z.object({
  title: z.string().min(1).optional(),
  prompt: z.string().min(1).optional(),
  workspace: z.string().min(1).nullable().optional(),
  spec: z.string().optional(),
  enabled: z.boolean().optional(),
});
export type SchedulePatchBody = z.infer<typeof SchedulePatchBody>;

// ---- validated CRUD (shared by the REST routes AND the schedule host fn) ---

/**
 * Create a schedule: spec validated against the grammar, workspace normalized and
 * required to exist NOW (same rule as session create — a bad path must not surface
 * later as sandbox failures inside every fired session). Throws HttpError(400);
 * the route dispatcher turns that into the response, the host fn into a program
 * exception.
 */
export async function scheduleCreate(
  db: Db,
  body: ScheduleCreateBody,
  now = Date.now(),
): Promise<Schedule> {
  if (!parseSpec(body.spec)) {
    throw new HttpError(
      400,
      `invalid spec ${JSON.stringify(body.spec)} — every:<N><m|h|d> or daily@HH:MM`,
    );
  }
  let workspace: string | null = null;
  if (body.workspace) {
    workspace = normalizeWorkspace(body.workspace);
    const problem = await workspaceProblem(workspace);
    if (problem) throw new HttpError(400, problem);
  }
  return db.createSchedule({
    id: crypto.randomUUID(),
    title: body.title,
    prompt: body.prompt,
    workspace,
    spec: body.spec,
    enabled: body.enabled ?? true,
    createdAt: now,
    lastRunAt: null,
    nextRunAt: nextRun(body.spec, now),
  });
}

/**
 * Patch a schedule. next_run_at recomputes from now when the spec changes or when
 * the schedule flips disabled→enabled — re-enabling must NOT count the disabled
 * stretch as downtime and fire immediately. Throws HttpError 404/400.
 */
export async function schedulePatch(
  db: Db,
  id: string,
  patch: SchedulePatchBody,
  now = Date.now(),
): Promise<Schedule> {
  const cur = db.getSchedule(id);
  if (!cur) throw new HttpError(404, "schedule not found");
  if (patch.spec !== undefined && !parseSpec(patch.spec)) {
    throw new HttpError(
      400,
      `invalid spec ${JSON.stringify(patch.spec)} — every:<N><m|h|d> or daily@HH:MM`,
    );
  }
  let workspace = cur.workspace;
  if (patch.workspace !== undefined) {
    workspace = patch.workspace === null ? null : normalizeWorkspace(patch.workspace);
    if (workspace) {
      const problem = await workspaceProblem(workspace);
      if (problem) throw new HttpError(400, problem);
    }
  }
  const next: Schedule = {
    ...cur,
    title: patch.title ?? cur.title,
    prompt: patch.prompt ?? cur.prompt,
    workspace,
    spec: patch.spec ?? cur.spec,
    enabled: patch.enabled ?? cur.enabled,
  };
  if (next.spec !== cur.spec || (next.enabled && !cur.enabled)) {
    next.nextRunAt = nextRun(next.spec, now);
  }
  db.updateSchedule(next);
  return db.getSchedule(id)!;
}

export function scheduleRemove(db: Db, id: string): void {
  if (!db.getSchedule(id)) throw new HttpError(404, "schedule not found");
  db.deleteSchedule(id);
}

/**
 * The `schedule.*` host-fn dispatcher (run_steps bridges one function; the worker
 * fans it out as a method object, like lsp.*). Same validated code path as the
 * REST CRUD above. `defaultWorkspace` — the calling session's workspace — fills
 * in when `add` omits one, so "check the deploy each morning" schedules against
 * the repo the conversation is about.
 */
export async function scheduleVerb(
  db: Db,
  verb: string,
  args: unknown,
  defaultWorkspace?: string | null,
): Promise<unknown> {
  switch (verb) {
    case "list":
      return db.listSchedules();
    case "add": {
      const parsed = ScheduleCreateBody.safeParse(args);
      if (!parsed.success) {
        throw new HttpError(400, "schedule.add: invalid args: " + parsed.error.message);
      }
      const body = parsed.data;
      if (!body.workspace && defaultWorkspace) body.workspace = defaultWorkspace;
      return await scheduleCreate(db, body);
    }
    case "enable":
    case "disable": {
      if (typeof args !== "string" || !args) {
        throw new HttpError(400, `schedule.${verb}: schedule id (string) required`);
      }
      return await schedulePatch(db, args, { enabled: verb === "enable" });
    }
    case "remove": {
      if (typeof args !== "string" || !args) {
        throw new HttpError(400, "schedule.remove: schedule id (string) required");
      }
      scheduleRemove(db, args);
      return { ok: true, removed: args };
    }
    default:
      throw new HttpError(400, `unknown schedule verb: ${verb} (list|add|enable|disable|remove)`);
  }
}

// ---- firing + the ticker ---------------------------------------------------

/**
 * Fire one schedule: create a fresh root session titled from it and start a turn
 * with its prompt. The session.created announce keeps live TUIs in sync — after
 * that the session is indistinguishable from a hand-started one.
 */
export function fireSchedule(ctx: TurnCtx, s: Schedule): Session {
  const session: Session = {
    id: crypto.randomUUID(),
    parentId: null,
    title: s.title,
    kind: "root",
    createdAt: Date.now(),
    ...(s.workspace ? { workspace: s.workspace, originDir: s.workspace } : {}),
  };
  ctx.db.createSession(session);
  ctx.bus.publish({ type: "session.created", sessionId: session.id, data: session });
  startUserTurn(ctx, session.id, s.prompt);
  return session;
}

/**
 * One ticker pass at `now`: fire every due schedule via `fire`, stamping
 * last_run_at = now and next_run_at = nextRun(spec, now) — the advance happens
 * BEFORE firing so a throwing fire can't hot-loop, and always from `now` so a
 * catch-up after downtime fires once (see module doc). Returns how many fired.
 */
export function tickSchedules(db: Db, now: number, fire: (s: Schedule) => void): number {
  const due = db.dueSchedules(now);
  for (const s of due) {
    db.markScheduleRun(s.id, now, nextRun(s.spec, now));
    try {
      fire(s);
    } catch (e) {
      console.error(`schedule ${s.id} (${s.title}) failed to fire: ${(e as Error).message}`);
    }
  }
  return due.length;
}

/** The production loop: a ~30s interval over tickSchedules. Returns a stopper. */
export function startScheduleTicker(ctx: TurnCtx, intervalMs = 30_000): () => void {
  const timer = setInterval(
    () => tickSchedules(ctx.db, Date.now(), (s) => fireSchedule(ctx, s)),
    intervalMs,
  );
  // Don't let the ticker alone keep a test/CLI process alive.
  Deno.unrefTimer(timer);
  return () => clearInterval(timer);
}
