/**
 * The workflow REST surface: list, start, inspect, and the four lifecycle verbs.
 *
 * The invariant this module holds is the router's own — **HTTP lives in `server/`
 * and nowhere else** — applied to a subsystem that is entirely asynchronous. Every
 * handler here is a thin translation: parse the body with the frozen Zod schema,
 * call one function in `workflow/control.ts`, answer with what it returned. Domain
 * failures (no such run, a rerun of a run that is still live, a script whose `meta`
 * is computed) arrive as `HttpError` subclasses and are rendered by the single catch
 * in `app.ts`, so there is not one try/catch in this file.
 *
 * Why start answers 201 and the verbs answer 200: a start CREATES a run and hands
 * back its row — and it does so IMMEDIATELY, because the script is detached from the
 * request that started it (spec §8). The response is the receipt, not the result;
 * progress arrives on `/events` as `workflow.updated` / `workflow.agent` /
 * `workflow.log`, and completion posts a system note into the owning session. A
 * client that waits for the fan-out to finish before rendering has misread the
 * contract.
 *
 * `GET /workflows/:id` is therefore the reconnect path for a run, the same way
 * `GET /sessions/:id` is for a session: it returns the run, every journal row with
 * its live activity, the mirrored script path, and whether the run is live in THIS
 * process. That last flag is not cosmetic — a run left `running` by a dead process
 * is reconciled to `orphaned` at boot, and a client that cannot tell the two apart
 * shows a fan-out that will never advance.
 */
import { z } from "zod";
import { BadRequestError, NotFoundError } from "../errors.ts";
import { CreateWorkflowBody, RerunWorkflowBody } from "../schema/requests.ts";
import type { WorkflowRun } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import {
  controlWorkflowAgent,
  rerunWorkflowRun,
  startWorkflowRun,
  workflowControlOf,
  workflowDetail,
} from "../workflow/control.ts";
import {
  activeGuideline,
  GUIDELINE_TARGET,
  guidelineAdvice,
  replaySummary,
  setGuideline,
  tokenWarnThreshold,
} from "../workflow/report.ts";
import {
  MAX_AGENTS_PER_RUN,
  pauseWorkflow,
  resumeWorkflow,
  stopWorkflow,
  workflowConcurrency,
  workflowSummary,
} from "../workflow/run.ts";
import {
  listSavedWorkflows,
  readSavedWorkflow,
  saveRunAs,
  saveWorkflow,
} from "../workflow/saved.ts";
import { type Handler, json, parseBody } from "./http.ts";

/** 404 naming the id, so a client's log says which run was wrong. */
function requireWorkflow(ctx: AppCtx, id: string): WorkflowRun {
  const run = ctx.db.getWorkflow(id);
  if (!run) throw new NotFoundError(`workflow ${id} not found`);
  return run;
}

/**
 * `GET /workflows` — every run, newest first. `?session=<id>` scopes to one
 * session's runs.
 *
 * Summaries, not rows: the script text is the largest field by far and a list that
 * carried N copies of it is a payload nobody reads (`workflow/run.ts`).
 */
export const listWorkflows: Handler = (req, ctx) => {
  const url = new URL(req.url);
  const sessionId = url.searchParams.get("session") ?? url.searchParams.get("sessionId") ??
    undefined;
  return json({
    workflows: ctx.db.listWorkflows(sessionId).map((r) => workflowSummary(ctx.db, r)),
  });
};

/**
 * `POST /workflows` — start a run, detached, and answer with its row.
 *
 * 201 the instant the worker is launched. The run outlives this request by design;
 * everything after this point is events and, at the end, a system note.
 */
export const createWorkflow: Handler = async (req, ctx) => {
  const body = await parseBody(req, CreateWorkflowBody);
  const run = await startWorkflowRun(ctx, {
    sessionId: body.sessionId,
    script: body.script,
    args: body.args,
  }, workflowControlOf(ctx));
  return json(run, 201);
};

/** `GET /workflows/:id` — the run, its agents with live activity, and the script file. */
export const getWorkflow: Handler = (_req, ctx, params) => {
  const run = requireWorkflow(ctx, params.id);
  return json(workflowDetail(ctx.db, run, workflowControlOf(ctx).agents));
};

/**
 * `POST /workflows/:id/stop` — kill the worker AND interrupt every subagent turn
 * the run started.
 *
 * Both halves, always: terminating the worker only stops the script, and a stop that
 * left four subagents running would leave a fan-out billing with nobody reading it
 * (spec §8). The interrupt travels on the run's abort signal, which the agent runner
 * in `workflow/control.ts` cascades into each child's turn.
 *
 * Idempotent on a run that is already finished: it answers with the row rather than
 * 409ing, because "stop" on something already stopped is the state the caller wanted.
 */
export const stopWorkflowH: Handler = (_req, ctx, params) => {
  requireWorkflow(ctx, params.id);
  return json(stopWorkflow(ctx, params.id));
};

/**
 * `POST /workflows/:id/pause` — gate NEW `agent()` calls; the ones in flight finish.
 *
 * A 409 on a run that is not live in this process is the honest answer, not a
 * courtesy 200: pausing is an instruction to a running worker, and there is no
 * worker to instruct.
 */
export const pauseWorkflowH: Handler = (_req, ctx, params) => {
  requireWorkflow(ctx, params.id);
  return json(pauseWorkflow(ctx, params.id));
};

/** `POST /workflows/:id/resume` — open the gate; parked calls release FIFO. */
export const resumeWorkflowH: Handler = (_req, ctx, params) => {
  requireWorkflow(ctx, params.id);
  return json(resumeWorkflow(ctx, params.id));
};

/**
 * `POST /workflows/:id/rerun` — a NEW run that replays this one's journal.
 *
 * Never an edit of the run it replays (spec §2.4). With no `script` the edited
 * `~/.bough/workflows/<id>.js` mirror wins, which is what makes "edit the file, press
 * r" the whole iteration loop; unchanged `agent()` calls replay instantly and only
 * the calls whose key changed cost anything.
 */
export const rerunWorkflowH: Handler = async (req, ctx, params) => {
  requireWorkflow(ctx, params.id);
  const body = await parseBody(req, RerunWorkflowBody, {});
  const run = await rerunWorkflowRun(ctx, params.id, {
    ...(body.script !== undefined ? { script: body.script } : {}),
    args: body.args,
  }, workflowControlOf(ctx));
  // The run row, plus what this relaunch is replaying (spec §8: "any operation that
  // replays returns how many calls were served from the journal and how many ran
  // live"). The run is detached, so the live counts here are the counts SO FAR — zero
  // at this instant — and `replay.available` is the number that carries information
  // now: it is the ceiling the new run's keys will be measured against, and it is what
  // makes a later `replayed: 0` legible as a key defect rather than as a first run.
  // The live counts arrive on `GET /workflows/:id`, which carries the same block.
  return json({ ...run, replay: replaySummary(ctx.db, run.id) }, 201);
};

// ---------------------------------------------------------------------------
// Saved workflows (spec §8, "Saving a run")
// ---------------------------------------------------------------------------

/** `POST /workflows/:id/save` — keep this run's script as a named workflow. */
const SaveWorkflowBody = z.object({ name: z.string().min(1) }).strict();

/** `PUT /saved-workflows/:name` — save a script, or the script a run ran. */
const PutSavedBody = z.object({
  script: z.string().min(1).optional(),
  runId: z.string().min(1).optional(),
}).strict();

/** `POST /saved-workflows/:name/runs` — invoke a saved workflow, parameterized. */
const RunSavedBody = z.object({
  sessionId: z.string().min(1),
  args: z.unknown().optional(),
}).strict();

/** `PUT /workflow-settings` — the size guideline. Advice to the script's author. */
const SettingsBody = z.object({ sizeGuideline: z.unknown() }).strict();

/**
 * `POST /workflows/:id/save` — save a finished run's script under a name.
 *
 * The script saved is the one the run would relaunch: the edited mirror if there is
 * one, else the stored row (`workflow/journal.ts`). Saving the row instead would
 * quietly save the version the user replaced — the opposite of "the script that did
 * what you wanted".
 */
export const saveWorkflowH: Handler = async (req, ctx, params) => {
  requireWorkflow(ctx, params.id);
  const body = await parseBody(req, SaveWorkflowBody);
  return json(await saveRunAs(ctx.db, params.id, body.name), 201);
};

/** `GET /saved-workflows` — every named workflow, with its `meta.description`. */
export const listSavedWorkflowsH: Handler = async () => {
  return json({ saved: await listSavedWorkflows() });
};

/** `GET /saved-workflows/:name` — one saved workflow, script included. */
export const getSavedWorkflowH: Handler = async (_req, _ctx, params) => {
  return json(await readSavedWorkflow(params.name));
};

/**
 * `PUT /saved-workflows/:name` — save a script directly, or copy a run's.
 *
 * Idempotent on the name: a saved workflow is a command, not a version history. The
 * run's own journal is where history lives.
 */
export const putSavedWorkflowH: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, PutSavedBody, {});
  if (body.runId) return json(await saveRunAs(ctx.db, body.runId, params.name), 201);
  if (!body.script) {
    throw new BadRequestError(
      "PUT /saved-workflows/:name needs {script} or {runId} — the script to save, or " +
        "the finished run whose script to save.",
    );
  }
  return json(await saveWorkflow(params.name, body.script), 201);
};

/**
 * `POST /saved-workflows/:name/runs` — invoke a saved workflow by name.
 *
 * `args` is the parameterization: the same orchestration against a different branch,
 * a different file list, a different threshold (spec §8). A new run every time, with
 * no `resumeOf` — invoking a saved workflow is not a relaunch of anything, and nothing
 * replays.
 */
export const runSavedWorkflowH: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, RunSavedBody);
  const saved = await readSavedWorkflow(params.name);
  const run = await startWorkflowRun(ctx, {
    sessionId: body.sessionId,
    script: saved.script,
    args: body.args,
  }, workflowControlOf(ctx));
  return json({ ...run, savedAs: saved.name }, 201);
};

/**
 * `GET /workflow-settings` — the size guideline and the thresholds derived from it.
 *
 * `advice` is the sentence to hand whoever writes the next script; a client that shows
 * the setting without it turns a guideline into a mystery number.
 */
export const getWorkflowSettingsH: Handler = () => {
  const guideline = activeGuideline();
  const target = GUIDELINE_TARGET[guideline];
  return json({
    sizeGuideline: guideline,
    target: Number.isFinite(target) ? target : null,
    advice: guidelineAdvice(guideline),
    tokenWarnThreshold: tokenWarnThreshold(),
    concurrency: workflowConcurrency(),
    maxAgentsPerRun: MAX_AGENTS_PER_RUN,
    advisory: true,
  });
};

/**
 * `PUT /workflow-settings` — set the size guideline.
 *
 * It changes what the next script is ADVISED to aim for and what the run view flags.
 * It caps nothing: no run is refused, paused or throttled by this value, and a run
 * already flagged stays exactly as fast as it was (spec §8).
 */
export const putWorkflowSettingsH: Handler = async (req) => {
  const body = await parseBody(req, SettingsBody);
  const guideline = await setGuideline(body.sizeGuideline);
  const target = GUIDELINE_TARGET[guideline];
  return json({
    sizeGuideline: guideline,
    target: Number.isFinite(target) ? target : null,
    advice: guidelineAdvice(guideline),
    tokenWarnThreshold: tokenWarnThreshold(),
    advisory: true,
  });
};

/**
 * `POST /workflows/:id/agents/:agentId/:action` — the run view's `x` / `r` on one
 * agent, while the rest of the run keeps going.
 *
 * The action is validated here rather than defaulted, because the two are not
 * interchangeable: a typo that silently became `stop` would kill work the user meant
 * to retry.
 */
export const controlWorkflowAgentH: Handler = (_req, ctx, params) => {
  const action = params.action;
  if (action !== "stop" && action !== "restart") {
    throw new BadRequestError(
      `unknown workflow agent action '${action}' — it is 'stop' (fail this one call, the ` +
        `run continues) or 'restart' (re-issue it on a fresh subagent session)`,
    );
  }
  requireWorkflow(ctx, params.id);
  return json(
    controlWorkflowAgent(ctx, params.id, params.agentId, action, workflowControlOf(ctx)),
  );
};
