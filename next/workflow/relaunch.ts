/**
 * Relaunching a workflow from a stopped run's journal — the only way to change what a
 * run is doing, and the accounting that says what the change cost.
 *
 * WHY THIS EXISTS. There is no mid-run user input (spec §8): a workflow script is not
 * editable while it executes and it never stops to ask. Pause gates NEW `agent()`
 * calls and lets the dispatched ones finish; that is steering the THROTTLE, not the
 * work. To steer the work you stop the run, edit the script — the mirror at
 * `~/.bough/workflows/<id>.js`, an explicit body over HTTP, or an agent rewriting it —
 * and relaunch seeded from the stopped run's journal. The result is a NEW run with its
 * own id that READS the old run's rows and never writes to them, because history is a
 * tree and nothing in bough is destructively rewritten (spec §2.4). A rerun is the same
 * operation with an unchanged script; this module is what both go through.
 *
 * THE INVARIANT THIS HOLDS: **replay never crosses the first changed call, and what it
 * did cross is reported.** Both halves, because either one alone is a defect.
 *
 *   - *Never crosses.* The engine (`workflow/run.ts`) replays the longest unchanged
 *     PREFIX of the source run's calls and runs the first divergent call — and every
 *     call after it — live, even ones whose own key is byte-identical. The reason is
 *     bough-specific and not negotiable: workflow agents share ONE checkout. A key
 *     covers a call's prompt, not the filesystem that prompt runs against, so two
 *     agents can both say "run the test suite" and mean different questions because an
 *     upstream agent rewrote the code in between. A cache miss costs money; a stale hit
 *     is a wrong answer presented as a fresh one, and it arrives with no error attached.
 *   - *Reported.* A relaunch that replayed 38 of 40 and one that replayed 0 of 40
 *     produce the same 201, the same events and eventually the same result — they
 *     differ only in an invoice nobody sees for a month. So every relaunch answers with
 *     what the journal offered (`relaunchPreview`), the finished run's note carries the
 *     counts (`run.ts`), and `relaunchReport` re-derives them from the rows at any
 *     time. `forced` is the number this module exists to expose: calls that ran live
 *     even though their key still matched, which is precisely what the prefix rule
 *     costs. If it is large and `divergedAt` is small, the fix is to move the edited
 *     call later in the script, and no other surface would have said so.
 *
 * WHAT IS NOT HERE. The prefix mechanism itself is the engine's (`replayPlan` +
 * `StartOpts.resumeOf` in `workflow/run.ts`), because that is where the journal is
 * written and one definition of a replay rule is the maximum safe number. Script
 * mirrors and the "explicit → mirror → stored" resolution are `workflow/journal.ts`.
 * Building a `WorkflowCtx` with real subagents behind it is `workflow/control.ts`, and
 * it is INJECTED here rather than imported: this module is reachable from the route
 * table, and an import back into the control layer would close a cycle through
 * `server/app.ts`. `main.ts` fills the seam at boot; a caller that finds it missing
 * gets an explicit error rather than a run with no agents behind it.
 */

import { ConflictError, NotFoundError, WorkflowError } from "../errors.ts";
import type { WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import { RerunWorkflowBody } from "../schema/requests.ts";
import type { AppCtx, Db } from "../types.ts";
import { resolveRerunScript, type ScriptSource } from "./journal.ts";
import { extractMeta } from "./meta.ts";
import {
  type CallPos,
  type Divergence,
  emptyReplayPlan,
  isWorkflowLive,
  replayablePrefix,
  replayAudit,
  type ReplayPlan,
  replayPlan,
  startWorkflow,
  type WorkflowCtx,
} from "./run.ts";
// The two response helpers, and only those. `server/app.ts` imports the handlers at
// the bottom of this file, which forms a cycle — safe in exactly the way that file
// documents: both of these are called at REQUEST time, never while a module is
// evaluating, and the handlers below are function DECLARATIONS, so their bindings are
// initialized before either module's body runs.
import { json, parseBody } from "../server/http.ts";

// ---------------------------------------------------------------------------
// The injection seam
// ---------------------------------------------------------------------------

/**
 * A `WorkflowCtx` with real subagents behind it, plus the binding that tells its
 * runner which run it belongs to. `workflow/control.ts` builds one; `bind` must be
 * called with the new run's id the instant `startWorkflow` returns it.
 */
export interface RelaunchEngine {
  workflowCtx: WorkflowCtx;
  bind: (runId: string | null) => void;
}

/** What a relaunch needs from the layers it must not import. */
export interface RelaunchDeps {
  /** Production: `workflowCtxFor(ctx, sessionId, workflowControl)`. */
  ctxFor(ctx: AppCtx, sessionId: string): RelaunchEngine;
  /**
   * The model a call that names none resolves to, folded into the journal key.
   * MUST resolve the way the source run's did, or every key misses and a relaunch
   * silently re-runs the whole script (production: `workflowCtxModel`).
   */
  effectiveModel?(ctx: AppCtx, sessionId: string): string;
}

/**
 * The optional ctx field, declared here because `AppCtx` (T-1) is frozen — the same
 * shape as `WithTurnStarter`, `WithWorkflowControl` and `WithStructuredWorkflow`.
 */
export interface WithRelaunch {
  relaunch?: RelaunchDeps;
}

/**
 * The wired seam, or a loud failure.
 *
 * Deliberately NOT a silent default. Every other seam in the tree degrades to
 * production behaviour when unwired; this one cannot, because the thing it supplies is
 * the agent runner. A default would start a run whose every `agent()` call fails, mark
 * it `error`, and look like the script's fault. An unwired boot is a wiring bug and
 * says so.
 */
export function relaunchDepsOf(ctx: AppCtx): RelaunchDeps {
  const deps = (ctx as AppCtx & WithRelaunch).relaunch;
  if (!deps) {
    throw new WorkflowError(
      500,
      "workflow relaunch is not wired into this server: `ctx.relaunch` is unset, so " +
        "there is nothing to run the relaunched script's agents. This is a boot-wiring " +
        "bug in server/main.ts, not a problem with the run.",
    );
  }
  return deps;
}

// ---------------------------------------------------------------------------
// What the source journal offers (pure)
// ---------------------------------------------------------------------------

/**
 * What a relaunch could replay at best, known BEFORE the new script runs.
 *
 * A ceiling, never a promise: which of these the relaunch actually claims depends on
 * the keys the edited script produces, and the answer only exists once it has run.
 * Reported anyway, because `available: 40, replayed: 0` afterwards is the signature of
 * a broken key and `available: 0` is an ordinary first run — and a bare `replayed: 0`
 * cannot tell them apart.
 */
export interface RelaunchPreview {
  sourceId: string;
  /** Calls the source run journaled. */
  journaled: number;
  /** Of those, the ones that ANSWERED — `done`/`cached` with a result. */
  answers: number;
  /**
   * The leading run of answered calls: the most a relaunch can replay however
   * unchanged the script is. Smaller than `answers` whenever the source failed or was
   * stopped part-way, because replay stops at the first call it cannot serve.
   */
  replayablePrefix: number;
}

export function relaunchPreview(db: Db, sourceId: string): RelaunchPreview {
  const plan = replayPlan(db, sourceId);
  return {
    sourceId,
    journaled: plan.steps.length,
    answers: plan.steps.filter((s) => s.result !== null).length,
    replayablePrefix: replayablePrefix(plan),
  };
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

export interface RelaunchOpts {
  /**
   * The edited script. Absent = the `~/.bough/workflows/<id>.js` mirror the user may
   * have edited, then the stored row — which makes "edit the file, relaunch" the whole
   * loop and a rerun the case where the file is untouched.
   */
  script?: string;
  /** Absent = the source run's input, verbatim. */
  args?: unknown;
}

export interface RelaunchResult {
  /** The NEW run. Its `resumeOf` points at the source; the source is untouched. */
  run: WorkflowRun;
  source: WorkflowRun;
  /** Where the script came from — it decides what actually runs, so it is reported. */
  script: ScriptSource;
  replay: RelaunchPreview;
}

/**
 * Stop-edit-relaunch, the second half: start a new run seeded from `sourceId`'s
 * journal.
 *
 * A source that is still live is REFUSED rather than raced. Its journal is still being
 * written, so the prefix a relaunch would replay is not yet a fact — and the two runs
 * would then be driving agents against one checkout with no idea about each other. The
 * error says to stop it first, and pausing before stopping is what preserves the most
 * work: a dispatched agent that finishes is journaled and replays, one killed in flight
 * is not and starts over (spec §8).
 *
 * `meta` is extracted from the EDITED script, at this boundary, so a script whose meta
 * was broken by the edit is refused before a worker spawns or a row is written — and
 * so a renamed run is named after the script that is actually running.
 */
export async function relaunchWorkflow(
  ctx: AppCtx,
  sourceId: string,
  opts: RelaunchOpts = {},
  deps: RelaunchDeps = relaunchDepsOf(ctx),
): Promise<RelaunchResult> {
  const source = ctx.db.getWorkflow(sourceId);
  if (!source) throw new NotFoundError(`workflow ${sourceId} not found`);
  if (isWorkflowLive(sourceId)) {
    throw new ConflictError(
      `workflow ${sourceId} is still running — stop it first, then relaunch. A relaunch ` +
        `replays the journal of a run that has finished writing one; seeding from a run ` +
        `that is still journaling would replay a prefix that is still moving. Pause ` +
        `before you stop: agents already dispatched finish and are journaled, so they ` +
        `replay instead of starting over.`,
    );
  }

  const { script, from } = await resolveRerunScript(source, opts.script);
  const meta = extractMeta(script);
  const preview = relaunchPreview(ctx.db, sourceId);
  const { workflowCtx, bind } = deps.ctxFor(ctx, source.sessionId);
  try {
    const run = await startWorkflow(workflowCtx, {
      sessionId: source.sessionId,
      script,
      meta,
      // `undefined` means "keep the source run's input" — the engine reads it off the
      // source row. Passing `null` would silently blank a relaunch's args.
      ...(opts.args === undefined ? {} : { args: opts.args }),
      resumeOf: sourceId,
      ...(deps.effectiveModel
        ? { effectiveModel: deps.effectiveModel(ctx, source.sessionId) }
        : {}),
    });
    bind(run.id);
    return { run, source, script: from, replay: preview };
  } catch (err) {
    // Nothing started, so nothing can claim: settle the binding rather than leaving a
    // promise nobody will ever resolve.
    bind(null);
    throw err;
  }
}

// ---------------------------------------------------------------------------
// Reporting what the relaunch cost
// ---------------------------------------------------------------------------

/**
 * What a run actually did with its journal. Derived entirely from rows, so it reads the
 * same for a finished run, one still in flight, and one a restart orphaned.
 */
export interface RelaunchReport {
  runId: string;
  /** The run this one replays from, or `null` when it is a first run. */
  sourceId: string | null;
  total: number;
  /** Served from the journal: no subagent, no cost. */
  replayed: number;
  /** Ran an agent and settled — what this run paid for. */
  ranLive: number;
  /** Queued or running. Non-zero only while the run is in flight. */
  pending: number;
  succeeded: number;
  failed: number;
  stopped: number;
  /** Answers the source run offered — the ceiling on `replayed`. */
  available: number;
  /**
   * The dispatch index of the call replay stopped at, or `null` when the prefix held
   * all the way. "Call N of this run" — a human coordinate for a human line.
   */
  divergedAt: number | null;
  /**
   * Where replay stopped in the SCRIPT, and why: edited, moved, added, or unanswered.
   * `null` when the prefix held.
   *
   * The structural coordinate is the load-bearing half. `divergedAt` is a dispatch
   * index, and dispatch index is precisely the thing that is not reproducible across
   * runs of a barrier-free `pipeline()` — quoting it alone is how a transposed position
   * came to be reported as an edited prompt.
   */
  diverged: Divergence | null;
  /** `diverged?.pos`, lifted so a client can sort or link on it without unpacking. */
  divergedPos: CallPos | null;
  /**
   * Calls that ran live even though their key still matched the source at their own
   * position — the price of the prefix rule, stated rather than hidden. Every one of
   * them is a call that a key-matching cache would have served and this engine
   * deliberately did not, because an earlier call changed and agents share a checkout.
   */
  forced: number;
  /** Has the run ended? Until it has, these are counts so far, not a bill. */
  final: boolean;
  /** The prompts that cost an agent, in call order. On a relaunch: the edit, visible. */
  livePrompts: string[];
}

/** Buckets a row exactly once, so the buckets always sum to the total. */
function bucket(a: WorkflowAgent): "replayed" | "pending" | "succeeded" | "failed" | "stopped" {
  switch (a.status) {
    case "cached":
      return "replayed";
    case "queued":
    case "running":
      return "pending";
    case "done":
      return "succeeded";
    case "error":
      return "failed";
    default:
      return "stopped";
  }
}

export function relaunchReport(db: Db, runId: string): RelaunchReport {
  const run = db.getWorkflow(runId);
  if (!run) throw new NotFoundError(`workflow ${runId} not found`);
  const rows = db.listWorkflowAgents(runId);
  const plan: ReplayPlan = run.resumeOf ? replayPlan(db, run.resumeOf) : emptyReplayPlan();

  const counts = { replayed: 0, pending: 0, succeeded: 0, failed: 0, stopped: 0 };
  const livePrompts: string[] = [];
  for (const row of rows) {
    const where = bucket(row);
    counts[where]++;
    if (where !== "replayed") livePrompts.push(row.prompt);
  }
  // The divergence and the forced count come from the ENGINE's own fold (`replayAudit`),
  // not a second walk here. A report that re-derived the prefix rule its own way could
  // disagree with the journal, and then the number that exists to expose a defect would
  // be one.
  const audit = replayAudit(plan, rows);
  return {
    runId,
    sourceId: run.resumeOf,
    total: rows.length,
    replayed: counts.replayed,
    ranLive: counts.succeeded + counts.failed + counts.stopped,
    pending: counts.pending,
    succeeded: counts.succeeded,
    failed: counts.failed,
    stopped: counts.stopped,
    available: plan.steps.filter((s) => s.result !== null).length,
    divergedAt: audit.divergedAt,
    diverged: audit.diverged,
    divergedPos: audit.diverged?.pos ?? null,
    forced: audit.forced,
    final: run.status !== "running" && run.status !== "paused",
    livePrompts,
  };
}

/**
 * The one-line human form — a run-view header, a CLI line, a note.
 *
 * Written so a failure reads as a failure: "0 replayed of 12 available" is a sentence
 * someone stops on, and "12 agents ran" is one they scroll past. They are the same run.
 */
export function relaunchLine(r: RelaunchReport): string {
  if (r.total === 0) return r.pending > 0 ? "no calls journaled yet" : "no agent calls";
  const parts = [`${r.replayed} replayed`, `${r.ranLive} ran live`];
  if (r.pending > 0) parts.push(`${r.pending} still going`);
  let line = `${parts.join(", ")} of ${r.total}`;
  if (!r.sourceId) return line;
  line += ` (${r.available} available from ${r.sourceId})`;
  if (r.available > 0 && r.replayed === 0) {
    // WHY it replayed nothing, not just that it did. "every key changed" was wrong for a
    // transposed position and right for an edit, and the two need opposite fixes.
    return `${line} — replayed NOTHING: ${r.diverged?.reason ?? "the first call already differed"}`;
  }
  if (r.diverged !== null) {
    line += `; replay stopped at ${r.diverged.pos} (call ${r.divergedAt}) — ` +
      `${r.diverged.reason}`;
    if (r.forced > 0) {
      line += `, so ${r.forced} unchanged call${r.forced === 1 ? "" : "s"} ran live behind it`;
    }
  }
  return line;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------
//
// Declared as `function`, not `const`: `server/app.ts` imports these into a route
// table it builds while evaluating, and this module imports `json`/`parseBody` back
// from it. Function declarations are hoisted and initialized before either module's
// body runs, so the cycle cannot resolve to a binding in its temporal dead zone
// whichever side the process happens to enter from.

/**
 * `POST /workflows/:id/relaunch` — a NEW run seeded from this one's journal.
 *
 * 201, immediately: the script is detached from this request the moment the worker is
 * launched (spec §8), so the body is a receipt and not a result. It carries the replay
 * PREVIEW — what the source journal has on offer — because the counts of what was
 * actually claimed do not exist yet. `GET /workflows/:id/replay` has those, and the
 * completion note carries them into the session unasked.
 */
export async function relaunchWorkflowH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  const body = await parseBody(req, RerunWorkflowBody, {});
  const result = await relaunchWorkflow(ctx, params.id, {
    ...(body.script !== undefined ? { script: body.script } : {}),
    ...(body.args === undefined ? {} : { args: body.args }),
  });
  return json({
    workflow: result.run,
    source: result.source.id,
    script: result.script,
    replay: result.replay,
  }, 201);
}

/**
 * `GET /workflows/:id/replay` — how many of this run's calls were served from a
 * journal and how many cost an agent.
 *
 * Its own endpoint rather than a field on the run, because it answers a question about
 * MONEY and it must be readable while the run is still going: an audit that is
 * replaying nothing is worth catching at call 3, not in the bill.
 */
export function workflowReplayH(
  _req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Response {
  const report = relaunchReport(ctx.db, params.id);
  return json({ ...report, line: relaunchLine(report) });
}
