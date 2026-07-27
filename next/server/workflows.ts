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
import { pauseWorkflow, resumeWorkflow, stopWorkflow, workflowSummary } from "../workflow/run.ts";
import { type Handler, json, parseBody } from "./app.ts";

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
  return json(run, 201);
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
