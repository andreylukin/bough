/**
 * Schedules: the spec grammar, the next-run math, the validated CRUD, and the
 * `schedule.*` host function that sits on top of all three.
 *
 * THE INVARIANT THIS HOLDS: **`next_run_at` is always computed FROM NOW, never from
 * the stale stored value** (spec §9, plan §6.8). Every function here that produces a
 * next-run time takes `now` and adds to *that*, and nothing anywhere adds an
 * interval to the row it just read. The failure it prevents is specific and
 * expensive: a laptop closed overnight with an `every:30m` schedule wakes up 16 slots
 * behind, and a catch-up loop that advanced from the stored value would open sixteen
 * fresh sessions and run sixteen turns against the model before the user finished
 * logging in. Advancing from `now` means one run, then the cadence resumes. The
 * ticker (`../schedules.ts`) is where that is exercised; the rule lives here because
 * this is where the arithmetic is.
 *
 * The same rule shows up twice more in `schedulePatch`, and both are the invariant
 * seen from a different angle: changing the spec recomputes from now (the old
 * cadence's next slot means nothing under a new cadence), and re-enabling a disabled
 * schedule recomputes from now — otherwise the disabled stretch reads as downtime and
 * the schedule fires the instant it is switched back on, which is not what "enable"
 * means to anybody.
 *
 * PURITY. `parseSpec` and `nextRun` are pure with `now` injected, which is what lets
 * the catch-up behavior be proven in a unit test rather than observed over an hour.
 * Everything below them is thin: the CRUD validates and writes, the host fn adapts
 * strings to it.
 *
 * WHY THE CRUD LIVES HERE AND NOT IN `server/`. Two callers need exactly the same
 * validation — the REST routes and the model, through `schedule.add(...)` — and a
 * spec that parses over HTTP but not from a program (or, worse, the reverse) is a
 * bug nobody would find until a schedule silently never fired. One validated path,
 * used by both. It is in `hostfn/` rather than `server/` because of the module
 * boundary rule (plan §3): `hostfn/` never imports from `server/`, so the shared code
 * has to sit on this side of the line. `../schedules.ts` — the ticker and the routes —
 * imports from here, never the other way around.
 *
 * Ported from `src/schedules.ts`. Deltas from that port are marked `NOTE:`.
 */
import { homedir } from "node:os";
import { resolve } from "node:path";
import { ScheduleError } from "../errors.ts";
import { CreateScheduleBody, PatchScheduleBody } from "../schema/requests.ts";
import type { Schedule } from "../schema/parts.ts";
import type { Db, HostFns, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// The grammar (pure)
// ---------------------------------------------------------------------------

/** A spec that parsed. `every` is a fixed interval; `daily` is a local wall clock. */
export type ParsedSpec =
  | { kind: "every"; ms: number }
  | { kind: "daily"; hh: number; mm: number };

const UNIT_MS: Record<string, number> = { m: 60_000, h: 3_600_000, d: 86_400_000 };

/**
 * The grammar, stated once, in the words the error message uses.
 *
 * Error text is a product surface (spec §6): a rejected spec must say what the legal
 * shapes are, because the model's next move is to write another one and "invalid
 * spec" alone gets a second guess rather than a fix.
 */
export const SPEC_HELP = "every:<N><m|h|d> with N ≥ 1 (every:30m, every:2h, every:1d) " +
  "or daily@HH:MM in local wall-clock time (daily@09:00)";

/** Parse a spec string, or `null` when it does not match the grammar. */
export function parseSpec(spec: string): ParsedSpec | null {
  const every = /^every:(\d+)(m|h|d)$/.exec(spec);
  if (every) {
    const n = Number(every[1]);
    // N ≥ 1. `every:0m` would parse to a zero interval, and a schedule whose next run
    // is always "now" fires on every single tick forever.
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
 * The next fire time strictly after `from` (epoch ms).
 *
 * `daily@` resolves in LOCAL time through `Date`, so a wall-clock time already past
 * today lands tomorrow, and `Date`'s own calendar arithmetic absorbs DST — the run
 * stays at HH:MM local on either side of a transition, which is what a user who asked
 * for "every morning at nine" means.
 *
 * Strictly after, never equal: `nextRun(spec, now)` is called at fire time with the
 * firing instant as `now`, and a result equal to `now` would be due again on the very
 * next tick.
 */
export function nextRun(spec: string | ParsedSpec, from: number): number {
  const parsed = typeof spec === "string" ? parseSpec(spec) : spec;
  if (!parsed) throw new ScheduleError(400, `invalid schedule spec: ${spec} — use ${SPEC_HELP}`);
  if (parsed.kind === "every") return from + parsed.ms;
  const d = new Date(from);
  d.setHours(parsed.hh, parsed.mm, 0, 0);
  if (d.getTime() <= from) d.setDate(d.getDate() + 1);
  return d.getTime();
}

// ---------------------------------------------------------------------------
// Workspace resolution
// ---------------------------------------------------------------------------

/**
 * Resolve and validate a workspace path, or throw a 400 naming the problem.
 *
 * Injected so the CRUD is testable without a real directory, and so a test never
 * needs `--allow-read` on someone's home. The default below is the production one.
 *
 * NOTE: this restates `server/sessions.ts`'s `normalizeWorkspace` + its directory
 * check rather than importing them, for the same structural reason `agents/notes.ts`
 * restates `TurnStarter`: `hostfn/` must not import from `server/` (plan §3). The
 * rule it implements is the session-create rule, deliberately: a schedule pointed at
 * a path that does not exist would otherwise surface a year of shell failures inside
 * every fired session, and read as the agent being broken rather than the schedule
 * being wrong.
 */
export type WorkspaceResolver = (raw: string) => Promise<string>;

/** Expand `~`, make absolute, require it to be a directory that exists now. */
export const resolveWorkspace: WorkspaceResolver = async (raw) => {
  const home = homedir();
  const trimmed = raw.trim();
  const expanded = trimmed === "~"
    ? home
    : trimmed.startsWith("~/")
    ? resolve(home, trimmed.slice(2))
    : trimmed;
  const abs = resolve(expanded);
  let stat: Deno.FileInfo;
  try {
    stat = await Deno.stat(abs);
  } catch {
    throw new ScheduleError(
      400,
      `workspace does not exist: ${abs}. Point the schedule at a checkout that is ` +
        `there now — every firing opens a session in it.`,
    );
  }
  if (!stat.isDirectory) throw new ScheduleError(400, `workspace is not a directory: ${abs}`);
  return abs;
};

/** The seams the CRUD takes. Both default to production behavior. */
export interface ScheduleDeps {
  /** Injected clock. Absent = `Date.now`. */
  now?: () => number;
  /** Absent = `resolveWorkspace`. */
  workspace?: WorkspaceResolver;
}

// ---------------------------------------------------------------------------
// Validated CRUD — shared by the REST routes and the host fn
// ---------------------------------------------------------------------------

function requireSpec(spec: string): void {
  if (parseSpec(spec)) return;
  throw new ScheduleError(400, `invalid spec ${JSON.stringify(spec)} — use ${SPEC_HELP}`);
}

function requireSchedule(db: Db, id: string): Schedule {
  const found = db.getSchedule(id);
  if (!found) {
    throw new ScheduleError(
      404,
      `schedule ${id} not found — schedule.list() returns the ids that exist`,
    );
  }
  return found;
}

/**
 * Create a schedule. The first `next_run_at` is computed from `now`, like every
 * other one: a schedule created at 09:00 with `every:2h` is next due at 11:00, not
 * immediately.
 */
export async function scheduleCreate(
  db: Db,
  body: CreateScheduleBody,
  deps: ScheduleDeps = {},
): Promise<Schedule> {
  const now = (deps.now ?? Date.now)();
  requireSpec(body.spec);
  const workspace = body.workspace
    ? await (deps.workspace ?? resolveWorkspace)(body.workspace)
    : null;
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
 * Patch a schedule. `next_run_at` is recomputed from now in exactly two cases, and
 * both are the module header's invariant seen from a different angle: the spec
 * changed, or the schedule went disabled → enabled.
 */
export async function schedulePatch(
  db: Db,
  id: string,
  patch: PatchScheduleBody,
  deps: ScheduleDeps = {},
): Promise<Schedule> {
  const now = (deps.now ?? Date.now)();
  const current = requireSchedule(db, id);
  if (patch.spec !== undefined) requireSpec(patch.spec);

  let workspace = current.workspace;
  if (patch.workspace !== undefined) {
    workspace = patch.workspace === null
      ? null
      : await (deps.workspace ?? resolveWorkspace)(patch.workspace);
  }

  const next: Schedule = {
    ...current,
    title: patch.title ?? current.title,
    prompt: patch.prompt ?? current.prompt,
    workspace,
    spec: patch.spec ?? current.spec,
    enabled: patch.enabled ?? current.enabled,
  };
  if (next.spec !== current.spec || (next.enabled && !current.enabled)) {
    next.nextRunAt = nextRun(next.spec, now);
  }
  db.updateSchedule(next);
  return db.getSchedule(id)!;
}

/** Delete a schedule. 404s rather than silently succeeding on an unknown id. */
export function scheduleRemove(db: Db, id: string): void {
  requireSchedule(db, id);
  db.deleteSchedule(id);
}

// ---------------------------------------------------------------------------
// The host function
// ---------------------------------------------------------------------------

/**
 * The `schedule.*` verbs, over the same validated CRUD the routes use.
 *
 * `defaultWorkspace` fills in when `add` omits one, so "check the deploy each
 * morning" schedules against the checkout the conversation is already about rather
 * than against the server's cwd — which is almost never the thing the user meant.
 */
export async function scheduleVerb(
  db: Db,
  verb: string,
  args: unknown,
  defaultWorkspace: string | null,
  deps: ScheduleDeps = {},
): Promise<unknown> {
  switch (verb) {
    case "list":
      return db.listSchedules();
    case "add": {
      const parsed = CreateScheduleBody.safeParse(args);
      if (!parsed.success) {
        throw new ScheduleError(
          400,
          `schedule.add: ${issues(parsed.error)}. It takes ` +
            `{title, prompt, spec, workspace?, enabled?} — spec is ${SPEC_HELP}.`,
        );
      }
      const body = parsed.data;
      return await scheduleCreate(
        db,
        body.workspace || !defaultWorkspace ? body : { ...body, workspace: defaultWorkspace },
        deps,
      );
    }
    case "enable":
    case "disable": {
      const id = scheduleId(verb, args);
      return await schedulePatch(db, id, { enabled: verb === "enable" }, deps);
    }
    case "remove": {
      const id = scheduleId(verb, args);
      scheduleRemove(db, id);
      return { ok: true, removed: id };
    }
    default:
      throw new ScheduleError(
        400,
        `unknown schedule verb: ${verb}. The verbs are list, add, enable, disable, remove.`,
      );
  }
}

/** The single-argument verbs all take a bare id string. Say so when they do not. */
function scheduleId(verb: string, args: unknown): string {
  if (typeof args === "string" && args.trim()) return args;
  throw new ScheduleError(
    400,
    `schedule.${verb}: pass the schedule id as a string — schedule.${verb}("<id>"). ` +
      `schedule.list() returns the ids.`,
  );
}

/** Zod issues, flattened to one line — these travel into a program as an exception. */
function issues(error: { issues: { path: PropertyKey[]; message: string }[] }): string {
  return error.issues
    .map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`)
    .join("; ");
}

/**
 * Build the bridged `schedule` host function for one turn.
 *
 * The wire is string-in/string-out (`harness/protocol.ts`), so the verb's argument
 * arrives as JSON and the result goes back as JSON; the worker rebuilds the
 * `schedule.add(...)` method object the program actually calls.
 */
export function createScheduleHostFn(
  ctx: TurnCtx,
  deps: ScheduleDeps = {},
): Pick<HostFns, "schedule"> {
  return {
    schedule: async (verb: string, argsJson: string): Promise<string> => {
      let args: unknown;
      try {
        args = argsJson === "" ? null : JSON.parse(argsJson);
      } catch {
        throw new ScheduleError(400, `schedule.${verb}: arguments were not valid JSON`);
      }
      // The session's own checkout is the default. `TurnCtx.workspace` is already
      // resolved for this turn (the runner falls back to the server's cwd when the
      // session pinned none), so a schedule created from a program always names a
      // real directory instead of inheriting whatever the server's cwd happens to be
      // at fire time, months later.
      const result = await scheduleVerb(ctx.db, verb, args, ctx.workspace ?? null, {
        now: deps.now ?? ctx.now,
        ...(deps.workspace ? { workspace: deps.workspace } : {}),
      });
      return JSON.stringify(result ?? null);
    },
  };
}
