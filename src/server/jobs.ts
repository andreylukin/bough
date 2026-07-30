/**
 * The jobs API: list, kill, read output — for a session AND its subagents.
 *
 * THE INVARIANT THIS HOLDS: **a session's job list covers the work done on its
 * behalf, not just the work its own turn started.** A spawner that fans out to four
 * subagents and asks "what is running?" means all of it. Subagents are separate
 * sessions (spec §7), and their background shells are registered under their own ids,
 * so a listing keyed only on the open session shows an empty jobs tab while four
 * builds are running underneath it — and the user has nothing to kill.
 *
 * That is also why the walk is TRANSITIVE and why each row carries its owning
 * `sessionId`: a subagent may delegate one level further (spec §7), and a row that
 * did not say whose it was would be unattributable in a merged list.
 *
 * KILL IS BY ID, ACROSS SESSIONS. `killJob` resolves the shell wherever it lives
 * (`hostfn/jobs.ts`), deliberately not within the open session's registry: anything
 * this endpoint can LIST it must also be able to kill, and scoping the kill to the
 * session in the URL 404'd on every subagent row the list had just returned.
 *
 * REST EXISTS SO A KILL DOES NOT COST A TURN. `bashKill` is the model's verb; this is
 * the human's. Without it, stopping a runaway `npm run dev` means asking the agent to
 * do it, which is an LLM round-trip to send one signal.
 *
 * READING OUTPUT HERE IS NON-DESTRUCTIVE. `jobOutput` returns the whole retained
 * buffer without advancing the model's `bashOutput` cursor — a human glancing at a
 * log must not make that output vanish from the agent's next tool result.
 *
 * Ported from `src/server/app.ts` (the three job handlers). Deltas are marked `NOTE:`.
 */
import { NotFoundError } from "../errors.ts";
import { RunShellBody } from "../schema/requests.ts";
import { parseBody } from "./http.ts";
import { JobRegistry, jobs as processJobs } from "../hostfn/jobs.ts";
import type { BackgroundJob } from "../schema/parts.ts";
import type { AppCtx, Db } from "../types.ts";
import { type Handler, json } from "./http.ts";

/**
 * The registry the handlers read.
 *
 * A module-level seam rather than a ctx field because `AppCtx` is frozen and the
 * registry is process-wide by necessity — a job outlives the turn that started it
 * (`hostfn/jobs.ts`). Production leaves the default; a test installs its own and puts
 * it back, so nothing leaks between files.
 */
let registry: JobRegistry = processJobs;

/** Swap the registry the handlers read. Returns the previous one, for restore. */
export function setJobRegistry(next: JobRegistry): JobRegistry {
  const previous = registry;
  registry = next;
  return previous;
}

/**
 * The session and every branch collapsed under it, transitively, ids only.
 *
 * `sessionsByOrigin` is the drill-in query — the branches whose `originId` is this
 * session — which is exactly the set whose work belongs to this one. Forks and
 * compactions surface there too and are excluded by kind: a fork is a sibling
 * conversation the user drives, not delegated work, and folding its jobs in would
 * make one branch's runaway process appear in another's list.
 *
 * The `seen` set is not paranoia about a well-formed tree; it is what stops a cycle
 * from a bad write hanging every request that touches this session.
 */
export function jobSessionIds(db: Db, sessionId: string): string[] {
  const out: string[] = [sessionId];
  const seen = new Set<string>([sessionId]);
  for (let i = 0; i < out.length; i++) {
    for (const child of db.sessionsByOrigin(out[i])) {
      if (child.kind !== "subagent" && child.kind !== "workflow_agent") continue;
      if (seen.has(child.id)) continue;
      seen.add(child.id);
      out.push(child.id);
    }
  }
  return out;
}

/** Every live-or-recent shell of a session and its delegates. */
export function jobsForTree(db: Db, sessionId: string): BackgroundJob[] {
  return jobSessionIds(db, sessionId).flatMap((id) => registry.listJobs(id));
}

function requireSession(ctx: AppCtx, id: string): void {
  if (!ctx.db.getSession(id)) {
    throw new NotFoundError(
      `no session ${id} — jobs are listed per session, so open one that exists ` +
        `(GET /sessions lists them).`,
    );
  }
}

/**
 * `GET /sessions/:id/jobs` — live and recently-exited background shells.
 *
 * Includes exited shells for a bounded window on purpose (`hostfn/jobs.ts`): the
 * outcome of a job you started should still be there when you look up from something
 * else, and a list that dropped a shell the instant it died would show a failed build
 * as nothing at all.
 */
export const listJobsH: Handler = (_req, ctx, params) => {
  requireSession(ctx, params.id);
  return json({ jobs: jobsForTree(ctx.db, params.id) });
};

/**
 * `POST /sessions/:id/jobs` — the USER starts a shell, in the session's workspace.
 *
 * The one thing the jobs API could not do. Every other verb here exists so a human
 * does not have to spend a turn to manage a shell — list, read, kill — and yet
 * starting one was the model's privilege alone: `!ls` in the composer had nowhere to
 * go, so the TUI printed "! is not a shell — this goes to the model" and a reflex
 * every comparable harness honours was a dead end.
 *
 * It is a `bashBg` shell, deliberately: it runs in the workspace, it publishes
 * `job.spawned`/`job.exited` like any other, it appears in the rail with its output
 * readable, and it does NOT carry a turn's abort signal — a command the user started
 * is not something the agent's stop button should kill. It also does not consume a
 * turn and never enters the thread, so nothing here is billed or replayed.
 */
export const runShellH: Handler = async (req, ctx, params) => {
  requireSession(ctx, params.id);
  const body = await parseBody(req, RunShellBody);
  const workspace = ctx.db.getSessionRuntime(params.id).workspace ?? process.cwd();
  // The name is what the rail shows. The command IS the name here: the user typed it
  // and knows exactly what they meant, so a summary would be a worse label.
  const started = registry.bashBg(body.command.trim().slice(0, 60), body.command, {
    sessionId: params.id,
    workspace,
  });
  return json(JSON.parse(started), 201);
};

/**
 * `POST /sessions/:id/jobs/:jobId/kill` — SIGTERM a running shell.
 *
 * SIGTERM first with a SIGKILL backstop, and it waits for the process to actually die
 * (`hostfn/jobs.ts`), so the response reports the outcome rather than the intent. The
 * `job.exited` event follows from the registry, which is what updates every attached
 * client rather than just the one that clicked.
 */
export const killJobH: Handler = async (_req, ctx, params) => {
  requireSession(ctx, params.id);
  return json({ message: await registry.killJob(params.jobId) });
};

/**
 * `GET /sessions/:id/jobs/:jobId/output` — the shell's whole retained buffer.
 *
 * Head and tail verbatim with an explicit omission marker in between, exactly as the
 * model sees it (spec §6) — a UI that showed a differently-truncated view would have
 * the human and the agent reading different logs while discussing the same job.
 */
export const jobOutputH: Handler = (_req, ctx, params) => {
  requireSession(ctx, params.id);
  const found = registry.jobOutput(params.jobId);
  if (!found) {
    throw new NotFoundError(
      `no background shell ${params.jobId} — it may have aged out of the job list, or ` +
        `belong to a session this server has not seen since it restarted (shells are ` +
        `in-memory and die with the process).`,
    );
  }
  return json(found);
};
