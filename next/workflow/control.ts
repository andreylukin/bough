/**
 * Workflow lifecycle control: the verbs that act on a LIVE run, and the one place a
 * run is assembled with real subagents behind it.
 *
 * WHY THIS EXISTS. `workflow/run.ts` owns the engine — the worker, the journal, the
 * semaphore and the pause gate — and takes its `AgentRunner` as a parameter so the
 * whole thing is drivable offline. That parameter is a hole exactly the size of this
 * module: something has to turn `agent(prompt, opts)` into a real subagent session,
 * carry the run's abort into that session's TURN, and hand the engine back a report.
 *
 * THE INVARIANT THIS HOLDS: **a control verb is not a status write.** `stop` is only
 * honest if the fan-out actually stops — the worker dies AND every subagent turn the
 * run started is interrupted (spec §8). Marking the row `stopped` while three
 * subagents keep billing against a dead reader is the failure this module exists to
 * prevent, and it is why the runner below wires `signal` → `interruptTurn(child)`
 * rather than merely refusing to launch anything new. `pause` is the mirror image:
 * it must NOT reach a running agent, because a paused run is one that stops
 * *admitting* work, not one that discards work already paid for. The engine's gate
 * gives that for free — it parks the next `agent()` call and never touches the ones
 * in flight — so this module's whole contribution to pause is to not undo it.
 *
 * Two more things follow from "a run outlives the turn that started it":
 *
 *   - **The launch context is fabricated.** A REST-started run has no live turn to
 *     borrow a `TurnCtx` from, so one is built here from the owning session (its
 *     workspace, its model pin, its last message as the lineage anchor). The signal
 *     on it is deliberately inert: a workflow agent's interrupt travels on the RUN's
 *     signal, which arrives per call, not on the ctx.
 *   - **Subagent caps do not apply.** Every launch takes an exempt lease (spec §8):
 *     the run's own semaphore is the meter, and a 200-agent audit would not fit under
 *     a per-turn cap of 8. The NESTING rule still applies, because that one is about
 *     lifetime, not width.
 *
 * SINGLE-AGENT CONTROL. The run view can stop or restart one agent while the rest of
 * the run continues, which needs a handle the engine does not keep: it holds one
 * abort controller for the whole run. So this module claims the journal row each call
 * is running against and owns a per-attempt controller under it — aborting that one
 * fails exactly that `agent()` call (the script's `parallel()` slot goes `null`,
 * `pipeline()` drops the item), and a restart re-issues the same call on a fresh
 * subagent session while the script stays parked on the promise it is already
 * awaiting.
 *
 * NOTE / accepted deltas from the port (`src/server/app.ts` + `src/workflow.ts`), both
 * consequences of file ownership rather than design changes:
 *   - A single-agent stop lands its journal row as `error`, not `stopped`. The row's
 *     terminal write belongs to `run.ts`, which maps only a RUN-level abort to
 *     `stopped`; the error text says plainly that the agent was stopped.
 *   - A workflow agent's session is kind `subagent` (that is what `launchSubagent`
 *     creates), not `workflow_agent`. Both kinds collapse under their origin
 *     identically (spec §4), so nothing user-visible differs — but see the notes.
 */
import { z } from "zod";
import { cappedLaunch } from "../agents/caps.ts";
import { postSystemNote } from "../agents/notes.ts";
import { ConflictError, NotFoundError, WorkflowError } from "../errors.ts";
import type { LaunchDeps } from "../agents/subagent.ts";
import { launchSubagent } from "../agents/subagent.ts";
import type { LaunchFn } from "../hostfn/delegate.ts";
import { workflowScriptPath } from "../paths.ts";
import type { Message, Part, WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import { DEFAULT_MODEL, interruptTurn } from "../turn/runner.ts";
import { type TurnRegistry, turns } from "../turn/queue.ts";
import type { AppCtx, Db, HostFns, TurnCtx } from "../types.ts";
import { resolveRerunScript } from "./journal.ts";
import { extractMeta } from "./meta.ts";
import { type RunAccounting, runAccounting, summarize } from "./report.ts";
import {
  type AgentCall,
  type AgentRunner,
  isWorkflowLive,
  pauseWorkflow,
  rerunWorkflow,
  resumeWorkflow,
  startWorkflow,
  stopWorkflow,
  type WorkflowCtx,
  workflowSummary,
} from "./run.ts";
import type { WithStructuredWorkflow } from "./schema.ts";

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

/** Re-read a journal row. Rows are small and a run holds at most a few hundred. */
function agentRow(db: Db, runId: string, agentId: string): WorkflowAgent | undefined {
  return db.listWorkflowAgents(runId).find((a) => a.id === agentId);
}

function publishAgent(ctx: Pick<AppCtx, "db" | "bus">, runId: string, agentId: string): void {
  const run = ctx.db.getWorkflow(runId);
  const row = run && agentRow(ctx.db, runId, agentId);
  if (run && row) ctx.bus.publish({ type: "workflow.agent", sessionId: run.sessionId, data: row });
}

// ---------------------------------------------------------------------------
// The live-agent registry
// ---------------------------------------------------------------------------

/** One in-flight `agent()` call, as the control verbs see it. */
export interface WorkflowAgentHandle {
  runId: string;
  /** The journal row this call is running against. */
  agentId: string;
  /** The CURRENT attempt's interrupt. Replaced on a restart. */
  ctrl: AbortController;
  /** Set by `restart` so the runner re-issues instead of failing the call. */
  restart: boolean;
  /** The subagent session, once it exists. */
  sessionId: string | null;
}

/**
 * Which journal rows are live in this process, and how to reach them.
 *
 * Process-wide by default, for the same reason the engine's own registry is: a run
 * outlives the request that started it, so a per-caller instance would hold nothing
 * by the time anyone pressed `x`. A test constructs its own and stays isolated.
 */
export class WorkflowAgentRegistry {
  readonly #byRun = new Map<string, Map<string, WorkflowAgentHandle>>();

  /**
   * Bind a starting call to its journal row.
   *
   * The engine flips the row to `running` and calls the runner with no `await`
   * between the two, so at this instant the row for THIS call is the lowest-indexed
   * running row nobody has claimed — every earlier one was claimed at its own start.
   * That is why the pairing does not need (and must not use) the call key: the
   * structured-output decorator rewrites the prompt before the runner sees it, so
   * `callKey(call)` no longer matches the row the engine journaled.
   *
   * Returns `null` when no unclaimed running row exists, which is not an error: the
   * call still runs, it simply cannot be singled out until it registers a session.
   */
  claim(db: Db, runId: string): WorkflowAgentHandle | null {
    let held = this.#byRun.get(runId);
    if (!held) this.#byRun.set(runId, held = new Map());
    const row = db.listWorkflowAgents(runId)
      .filter((a) => a.status === "running" && !held.has(a.id))
      .sort((a, b) => a.idx - b.idx)[0];
    if (!row) return null;
    const handle: WorkflowAgentHandle = {
      runId,
      agentId: row.id,
      ctrl: new AbortController(),
      restart: false,
      sessionId: row.sessionId,
    };
    held.set(row.id, handle);
    return handle;
  }

  /** Drop a settled call. Idempotent — the runner releases in a `finally`. */
  release(handle: WorkflowAgentHandle): void {
    const held = this.#byRun.get(handle.runId);
    if (!held) return;
    if (held.get(handle.agentId) === handle) held.delete(handle.agentId);
    if (held.size === 0) this.#byRun.delete(handle.runId);
  }

  get(runId: string, agentId: string): WorkflowAgentHandle | undefined {
    return this.#byRun.get(runId)?.get(agentId);
  }

  /** The run's live calls. The run view's "what is actually in flight" answer. */
  forRun(runId: string): WorkflowAgentHandle[] {
    return [...(this.#byRun.get(runId)?.values() ?? [])];
  }
}

/** The process-wide instance. Injected everywhere; this is production's. */
export const workflowAgents: WorkflowAgentRegistry = new WorkflowAgentRegistry();

// ---------------------------------------------------------------------------
// Injection seams
// ---------------------------------------------------------------------------

export interface WorkflowControlDeps {
  /** The turn registry a child's interrupt goes through. Absent = the process one. */
  registry?: TurnRegistry;
  /** Absent = the process-wide live-agent registry. */
  agents?: WorkflowAgentRegistry;
  /** Absent = `launchSubagent`. A test injects a fake and never runs a turn. */
  launch?: LaunchFn;
  /** The child's launch deps — its turn deps, its wall clock, its diff seam. */
  child?: (ctx: TurnCtx) => LaunchDeps;
  /** Absent = `postSystemNote`, which wakes the owning session when a run ends. */
  notify?: (ctx: AppCtx, sessionId: string, text: string) => void;
  /**
   * Decorate the assembled `WorkflowCtx`. Absent = `ctx.workflowCtx` (T5.3's
   * structured-output wrapper, installed at boot), then the identity.
   */
  decorate?: (base: WorkflowCtx) => WorkflowCtx;
  /** Injected clock. Absent = `ctx.now`, then `Date.now`. */
  now?: () => number;
}

/**
 * The optional ctx field, declared here because `AppCtx` (T-1) is frozen — same
 * shape as `WithTurnStarter` in `server/sessions.ts`. `main.ts` fills it; a handler
 * that finds it absent falls back to the production defaults above, so an unwired
 * seam degrades to "no test doubles", never to a broken route.
 */
export interface WithWorkflowControl {
  workflowControl?: WorkflowControlDeps;
}

export function workflowControlOf(ctx: AppCtx): WorkflowControlDeps {
  return (ctx as AppCtx & WithWorkflowControl).workflowControl ?? {};
}

// ---------------------------------------------------------------------------
// The production agent runner
// ---------------------------------------------------------------------------

/**
 * Turn one `agent()` call into a real subagent, and carry the run's stop into it.
 *
 * The abort listener is the whole point of this function. A workflow's `stop`
 * aborts the run's controller; without the cascade the script would stop and the
 * children would keep running — a fan-out billing with nobody left to read it. With
 * it, the child's turn is interrupted, persists its partial work, and reports
 * `interrupted`, which this rejects with so the engine's combinators see a failure
 * rather than a truncated report (spec §8).
 *
 * The `isRunning` guard on the cascade matters as much: a child that already
 * resolved has its report and its outcome persisted on its own branch, and
 * interrupting it now would flip a finished session to `interrupted`.
 */
export function createSubagentRunner(
  turnCtx: TurnCtx,
  deps: WorkflowControlDeps = {},
): AgentRunner {
  const launch = deps.launch ?? launchSubagent;
  const registry = deps.registry ?? turns;
  const childDeps = deps.child ?? (() => ({}));

  return async (call: AgentCall, signal: AbortSignal, onSpawned): Promise<string> => {
    if (signal.aborted) {
      throw new WorkflowError(409, "workflow stopped — this agent was never launched");
    }

    // Exempt from the width caps, never from the nesting rule (spec §8, T4.3).
    const child = cappedLaunch(
      turnCtx,
      { mode: "blocking", verb: "workflow agent()", exempt: true },
      () =>
        launch(
          turnCtx,
          call.prompt,
          { name: call.label, ...(call.model ? { model: call.model } : {}) },
          childDeps(turnCtx),
        ),
    );
    onSpawned(child.sessionId);

    const cascade = () => {
      if (registry.isRunning(child.sessionId)) interruptTurn(child.sessionId, registry);
    };
    signal.addEventListener("abort", cascade, { once: true });
    try {
      const result = await child.result;
      if (!result.ok) {
        // Named by status, not by a bare "failed": a stopped run, a child that
        // errored and a child the server restarted under call for different moves
        // from the script and from the person reading the run view (plan T4.4).
        throw new WorkflowError(
          result.status === "interrupted" ? 409 : 424,
          `workflow agent "${call.label}" ${result.status}: ` +
            clip(result.report || "(no report)", 400),
        );
      }
      return result.report;
    } finally {
      signal.removeEventListener("abort", cascade);
    }
  };
}

// ---------------------------------------------------------------------------
// Single-agent control
// ---------------------------------------------------------------------------

/** The run id, known only after `startWorkflow` returns — see `workflowCtxFor`. */
interface RunBinding {
  id: string | null;
  ready: Promise<string | null>;
  settle: (id: string | null) => void;
}

function newBinding(): RunBinding {
  let settle!: (id: string | null) => void;
  const binding: RunBinding = {
    id: null,
    ready: new Promise<string | null>((resolve) => (settle = resolve)),
    settle: (id) => {
      binding.id = id;
      settle(id);
    },
  };
  return binding;
}

/**
 * Wrap the engine-facing runner so each call owns a claimable handle.
 *
 * Wrapped OUTSIDE the structured-output decorator on purpose: one journal row is one
 * `agent()` call however many times a schema mismatch made it retry, so the claim —
 * and the restart loop — must span the retries rather than sit inside one attempt.
 */
function controlledRunner(
  ctx: Pick<AppCtx, "db" | "bus">,
  binding: RunBinding,
  inner: AgentRunner,
  deps: WorkflowControlDeps,
): AgentRunner {
  const registry = deps.agents ?? workflowAgents;

  return async (call, runSignal, onSpawned): Promise<string> => {
    const runId = binding.id ?? await binding.ready;
    const handle = runId ? registry.claim(ctx.db, runId) : null;
    try {
      for (;;) {
        const own = new AbortController();
        if (runSignal.aborted) own.abort();
        const relay = () => own.abort();
        runSignal.addEventListener("abort", relay, { once: true });
        if (handle) {
          handle.ctrl = own;
          handle.restart = false;
        }
        try {
          return await inner(call, own.signal, (sessionId) => {
            if (handle) handle.sessionId = sessionId;
            onSpawned(sessionId);
          });
        } catch (err) {
          // A restart re-issues the SAME call on a fresh subagent session. The
          // script is still parked on the promise it was already awaiting, and the
          // journal row is still `running` — the engine writes it only on settle —
          // so the only repair needed is to unpoint it from the abandoned session.
          if (handle?.restart && !runSignal.aborted && runId) {
            ctx.db.updateWorkflowAgent(handle.agentId, { sessionId: null, error: null });
            publishAgent(ctx, runId, handle.agentId);
            continue;
          }
          throw err;
        } finally {
          runSignal.removeEventListener("abort", relay);
        }
      }
    } finally {
      if (handle) registry.release(handle);
    }
  };
}

/**
 * The run view's `x` / `r` on one selected agent; the rest of the run continues.
 *
 * `stop` fails just that `agent()` call — the script sees the rejection and its
 * `parallel()` slot goes `null` or its `pipeline()` item drops. `restart` re-issues
 * it on a fresh subagent session.
 */
export function controlWorkflowAgent(
  ctx: Pick<AppCtx, "db" | "bus">,
  runId: string,
  agentId: string,
  action: "stop" | "restart",
  deps: WorkflowControlDeps = {},
): WorkflowAgent {
  if (!ctx.db.getWorkflow(runId)) throw new NotFoundError(`workflow ${runId} not found`);
  const row = agentRow(ctx.db, runId, agentId);
  if (!row) throw new NotFoundError(`workflow agent ${agentId} not found in run ${runId}`);
  if (row.status !== "running") {
    throw new ConflictError(
      `workflow agent "${row.label}" is ${row.status}, not running — only a running agent ` +
        `can be ${action === "stop" ? "stopped" : "restarted"}. Rerun the workflow to ` +
        `re-issue a finished call.`,
    );
  }
  const handle = (deps.agents ?? workflowAgents).get(runId, agentId);
  if (!handle) {
    throw new ConflictError(
      `workflow agent "${row.label}" is not live in this process — the server restarted ` +
        `since it started, so there is nothing here to ${action}. Stop the run and rerun ` +
        `it: the journal replays everything that already succeeded.`,
    );
  }
  handle.restart = action === "restart";
  handle.ctrl.abort();
  return agentRow(ctx.db, runId, agentId) ?? row;
}

// ---------------------------------------------------------------------------
// Assembling a run
// ---------------------------------------------------------------------------

/**
 * The lineage anchor: the message a workflow's agents hang off in the tree view.
 *
 * The owning session's latest message, because that is the one the user was looking
 * at when the run started. A session with no messages yet (a REST-started run on a
 * fresh session) gets a synthetic id rather than an empty string — `originMessageId`
 * is a pointer for the tree, not a foreign key, and an empty one would read as "this
 * branch came from nowhere".
 */
export function workflowAnchor(db: Db, sessionId: string): string {
  const thread: Message[] = db.threadFor(sessionId);
  return thread.at(-1)?.id ?? `workflow:${sessionId}`;
}

/**
 * The `TurnCtx` a workflow's launches run under.
 *
 * `signal` is inert by construction. A workflow outlives every turn, so there is no
 * turn interrupt to inherit; the run's abort arrives per call, as the `AgentRunner`'s
 * own signal, and `launchSubagent` reads nothing off this one.
 */
export function workflowLaunchCtx(
  ctx: AppCtx,
  sessionId: string,
  anchorMessageId?: string,
): TurnCtx {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new NotFoundError(`session ${sessionId} not found`);
  const runtime = ctx.db.getSessionRuntime(sessionId);
  return {
    ...ctx,
    sessionId,
    // No turn owns a workflow. The id is a label for the caps ledger, which this
    // path is exempt from anyway.
    turnId: `workflow:${sessionId}`,
    messageId: anchorMessageId ?? workflowAnchor(ctx.db, sessionId),
    workspace: runtime.workspace ?? Deno.cwd(),
    model: session.model ?? ctx.model ?? DEFAULT_MODEL,
    signal: new AbortController().signal,
    depth: 0,
  };
}

/**
 * Build the production `WorkflowCtx`, plus the binding that tells its runner which
 * run it belongs to.
 *
 * The order of the three wrappers is the design: `createSubagentRunner` launches,
 * `decorate` (T5.3) enforces `{schema}` around it with retries, and
 * `controlledRunner` sits outermost so one claim and one restart loop cover the
 * whole call. `bind` must be invoked with the run id the instant `startWorkflow`
 * returns it — nothing can reach the runner before then, because the worker cannot
 * send a host call in the same tick.
 */
export function workflowCtxFor(
  ctx: AppCtx,
  sessionId: string,
  deps: WorkflowControlDeps = {},
  anchorMessageId?: string,
): { workflowCtx: WorkflowCtx; bind: (runId: string | null) => void } {
  const turnCtx = workflowLaunchCtx(ctx, sessionId, anchorMessageId);
  const notify = deps.notify ?? ((c: AppCtx, sid: string, text: string) => {
    postSystemNote(c, sid, text);
  });
  const decorate = deps.decorate ??
    (ctx as AppCtx & WithStructuredWorkflow).workflowCtx ??
    ((base: WorkflowCtx) => base);

  const base: WorkflowCtx = {
    db: ctx.db,
    bus: ctx.bus,
    runner: createSubagentRunner(turnCtx, deps),
    notify: (sid, text) => notify(ctx, sid, text),
    ...(deps.now ?? ctx.now ? { now: deps.now ?? ctx.now } : {}),
  };
  const decorated = decorate(base);
  const binding = newBinding();
  return {
    workflowCtx: { ...decorated, runner: controlledRunner(ctx, binding, decorated.runner, deps) },
    bind: binding.settle,
  };
}

// ---------------------------------------------------------------------------
// Start and rerun
// ---------------------------------------------------------------------------

export interface StartRunOpts {
  sessionId: string;
  script: string;
  args?: unknown;
  /** Absent = the owning session's latest message. */
  anchorMessageId?: string;
  concurrency?: number;
  timeoutMs?: number;
}

/**
 * The model a call that names none will actually run on. Mirrors the resolution in
 * `workflowSubagentCtx` — session pin, else the ctx default, else the built-in — and
 * exists so the journal key can hash the RESOLVED model rather than only one the
 * script named. Without it, repinning a session and rerunning an unchanged script
 * replayed every row and returned the previous model's answers as a fresh run.
 */
export function workflowCtxModel(ctx: AppCtx, sessionId: string): string {
  return ctx.db.getSession(sessionId)?.model ?? ctx.model ?? DEFAULT_MODEL;
}

/**
 * Start a run with real subagents behind it.
 *
 * `meta` is extracted and validated HERE, at the submit boundary, so a script whose
 * meta is missing or computed is refused with a 400 before a worker is spawned or a
 * row is written — rather than failing mid-run, after the user has paid for agents
 * (spec §8, T5.3's "reject at submit time").
 */
export async function startWorkflowRun(
  ctx: AppCtx,
  opts: StartRunOpts,
  deps: WorkflowControlDeps = workflowControlOf(ctx),
): Promise<WorkflowRun> {
  const meta = extractMeta(opts.script);
  const { workflowCtx, bind } = workflowCtxFor(ctx, opts.sessionId, deps, opts.anchorMessageId);
  try {
    const run = await startWorkflow(workflowCtx, {
      sessionId: opts.sessionId,
      script: opts.script,
      meta,
      args: opts.args,
      effectiveModel: workflowCtxModel(ctx, opts.sessionId),
      ...(opts.concurrency !== undefined ? { concurrency: opts.concurrency } : {}),
      ...(opts.timeoutMs !== undefined ? { timeoutMs: opts.timeoutMs } : {}),
    });
    bind(run.id);
    return run;
  } catch (err) {
    // Nothing started, so nothing can claim: settle the binding rather than leaving
    // a promise nobody will ever resolve.
    bind(null);
    throw err;
  }
}

export interface RerunRunOpts {
  /** Absent = the `~/.bough/workflows/<id>.js` mirror, then the stored script. */
  script?: string;
  args?: unknown;
}

/**
 * Rerun a finished run: unchanged `agent()` calls replay from its journal, edited and
 * new ones run live.
 *
 * The script is resolved HERE rather than left to the engine, because meta travels
 * with the script: a user who edited the mirror may have renamed the run or changed
 * its phases, and a rerun that kept the source run's meta would label the new run
 * after the old script.
 */
export async function rerunWorkflowRun(
  ctx: AppCtx,
  id: string,
  opts: RerunRunOpts = {},
  deps: WorkflowControlDeps = workflowControlOf(ctx),
): Promise<WorkflowRun> {
  const src = ctx.db.getWorkflow(id);
  if (!src) throw new NotFoundError(`workflow ${id} not found`);
  if (isWorkflowLive(id)) {
    throw new ConflictError(
      `workflow ${id} is still running — stop it first, then rerun. A rerun replays the ` +
        `journal of a finished run; replaying one that is still writing to it would race.`,
    );
  }
  // One script resolution for the whole tree (`workflow/journal.ts`): an explicit
  // script wins, else the mirror the user edited, else the stored row. Resolved HERE
  // and passed down rather than left to the engine, because meta travels with the
  // script — see the header.
  const { script } = await resolveRerunScript(src, opts.script);
  const meta = extractMeta(script);
  const { workflowCtx, bind } = workflowCtxFor(ctx, src.sessionId, deps);
  try {
    const run = await rerunWorkflow(workflowCtx, id, {
      script,
      meta,
      args: opts.args,
      // Same resolution as the original run. This is the whole point of hashing the
      // resolved model: if the session has been repinned since, the keys no longer
      // match and the calls re-run instead of replaying the old model's answers.
      effectiveModel: workflowCtxModel(ctx, src.sessionId),
    });
    bind(run.id);
    return run;
  } catch (err) {
    bind(null);
    throw err;
  }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/**
 * One journal row plus what the run view needs WITHOUT opening the agent's session:
 * cumulative tokens, how many programs it ran, and the last few call gists.
 *
 * The activity trail is the difference between "running" and "running, and here is
 * what it is doing" — a stuck agent is otherwise indistinguishable from a slow one.
 */
export interface WorkflowAgentView extends WorkflowAgent {
  tokens: number;
  toolCalls: number;
  activity: string[];
  /** Is this call reachable by `stop`/`restart` in this process right now? */
  live: boolean;
}

const ACTIVITY_LINES = 4;

/** First line of a tool call's input, clipped — enough to recognize, never a dump. */
function gist(input: unknown, max: number): string {
  const text = typeof input === "string"
    ? input
    : ((input as { code?: unknown } | null)?.code ?? "") as string;
  const line = String(text ?? "").trim().split("\n")[0] ?? "";
  return clip(line, max);
}

export function workflowAgentViews(
  db: Db,
  runId: string,
  registry: WorkflowAgentRegistry = workflowAgents,
): WorkflowAgentView[] {
  const liveIds = new Set(registry.forRun(runId).map((h) => h.agentId));
  return db.listWorkflowAgents(runId).map((a) => {
    const live = liveIds.has(a.id);
    if (!a.sessionId) return { ...a, tokens: 0, toolCalls: 0, activity: [], live };
    const usage = db.sessionUsage(a.sessionId);
    const calls = db.messagesFor(a.sessionId)
      .flatMap((m: Message) => m.parts as Part[])
      .filter((p): p is Extract<Part, { type: "tool_call" }> => p.type === "tool_call");
    return {
      ...a,
      tokens: usage.inputTokens + usage.outputTokens,
      toolCalls: calls.length,
      activity: calls.slice(-ACTIVITY_LINES).map((c) => `${c.name}(${gist(c.input, 48)})`),
      live,
    };
  });
}

/**
 * `GET /workflows/:id`'s body, and `workflow.status({id})`'s.
 *
 * Carries the run, its journal rows with live activity, the script file, whether the
 * run is live in THIS process — and, since T5.8, the three accounting fields spec §8
 * requires of a run view:
 *
 *   - `replay` — how many calls were served from the journal and how many ran live.
 *     Required, not decorative: a relaunch that replayed nothing is otherwise
 *     indistinguishable from one that replayed everything.
 *   - `cost` — tokens and elapsed time per agent and per phase, so an expensive stage
 *     is visible while it runs rather than in the bill.
 *   - `warning` — the advisory large-run flag, or `null`. Computed here, at view time,
 *     from rows that already exist; nothing in the engine reads it, which is what makes
 *     "it does not pause or throttle" a property of the code rather than a promise.
 */
export function workflowDetail(
  db: Db,
  run: WorkflowRun,
  registry: WorkflowAgentRegistry = workflowAgents,
  accounting: RunAccounting = runAccounting(db, run),
): Record<string, unknown> {
  return {
    workflow: run,
    agents: workflowAgentViews(db, run.id, registry),
    scriptFile: workflowScriptPath(run.id),
    live: isWorkflowLive(run.id),
    replay: accounting.replay,
    cost: accounting.cost,
    warning: accounting.warning,
    guideline: accounting.guideline,
  };
}

// ---------------------------------------------------------------------------
// The program-side `workflow.*` verb
// ---------------------------------------------------------------------------

const StartArgs = z.object({
  script: z.string().min(1),
  args: z.unknown().optional(),
});
const RerunArgs = z.object({
  id: z.string().min(1),
  script: z.string().min(1).optional(),
  args: z.unknown().optional(),
});
const IdArgs = z.object({ id: z.string().min(1) });

function parseArgs<S extends z.ZodTypeAny>(schema: S, verb: string, shape: string, raw: unknown) {
  const parsed = schema.safeParse(raw ?? {});
  if (!parsed.success) {
    throw new WorkflowError(
      400,
      `workflow.${verb}(${shape}): ${
        parsed.error.issues.map((i) => `${i.path.join(".") || "args"}: ${i.message}`).join("; ")
      }`,
    );
  }
  return parsed.data as z.infer<S>;
}

/**
 * One verb-dispatched entry point for the program-side `workflow.*` methods.
 *
 * Every verb answers with the SUMMARY, never the run row: the row carries the whole
 * script, and a `workflow.list()` that shipped N copies of it would flood the
 * model's context for no purpose (`run.ts`, `workflowSummary`).
 */
export async function workflowVerb(
  ctx: AppCtx,
  sessionId: string,
  verb: string,
  args: unknown,
  deps: WorkflowControlDeps = workflowControlOf(ctx),
  anchorMessageId?: string,
): Promise<unknown> {
  const { db } = ctx;
  switch (verb) {
    case "start": {
      const a = parseArgs(StartArgs, "start", "{script, args?}", args);
      const run = await startWorkflowRun(
        ctx,
        {
          sessionId,
          script: a.script,
          args: a.args,
          ...(anchorMessageId ? { anchorMessageId } : {}),
        },
        deps,
      );
      return workflowSummary(db, run);
    }
    case "rerun": {
      const a = parseArgs(RerunArgs, "rerun", "{id, script?, args?}", args);
      const run = await rerunWorkflowRun(
        ctx,
        a.id,
        { ...(a.script !== undefined ? { script: a.script } : {}), args: a.args },
        deps,
      );
      // `replay` is REQUIRED on an operation that replays (spec §8). The run is
      // detached, so at this instant the live counts are still zero and `available` is
      // the number that matters: it is the ceiling the new run's keys will be measured
      // against, and `available: 40` next to a later `replayed: 0` is the whole signal.
      return { ...workflowSummary(db, run), replay: summarize(db, run) };
    }
    case "stop":
    case "pause":
    case "resume":
    case "status": {
      const a = parseArgs(IdArgs, verb, "{id}", args);
      if (verb === "status") {
        const run = db.getWorkflow(a.id);
        if (!run) throw new NotFoundError(`workflow ${a.id} not found`);
        const accounting = runAccounting(db, run);
        return {
          ...workflowSummary(db, run),
          agentRows: workflowAgentViews(db, run.id, deps.agents ?? workflowAgents),
          replay: accounting.replay,
          cost: accounting.cost,
          warning: accounting.warning,
        };
      }
      const run = verb === "stop"
        ? stopWorkflow(ctx, a.id)
        : verb === "pause"
        ? pauseWorkflow(ctx, a.id)
        : resumeWorkflow(ctx, a.id);
      return workflowSummary(db, run);
    }
    case "list":
      return db.listWorkflows(sessionId).map((r: WorkflowRun) => workflowSummary(db, r));
    default:
      throw new WorkflowError(
        400,
        `unknown workflow verb: ${verb} — it is one of ` +
          `start|rerun|stop|pause|resume|status|list, called as workflow.<verb>({…}).`,
      );
  }
}

/**
 * The bridged `workflow` host function for one turn.
 *
 * Bound to the turn's session and its in-flight supervisor message: a run started
 * from a program belongs to the session that started it, and its agents hang off the
 * message that was streaming when the program called — which is what puts them in
 * the right place in the tree.
 */
export function createWorkflowHostFn(
  turnCtx: TurnCtx,
  deps: WorkflowControlDeps = workflowControlOf(turnCtx),
): NonNullable<HostFns["workflow"]> {
  return async (verb: string, argsJson: string): Promise<string> => {
    let args: unknown = null;
    try {
      args = JSON.parse(argsJson ?? "null");
    } catch {
      throw new WorkflowError(400, `workflow.${verb}(…): arguments must be a JSON value`);
    }
    const value = await workflowVerb(
      turnCtx,
      turnCtx.sessionId,
      verb,
      args,
      deps,
      turnCtx.messageId,
    );
    return JSON.stringify(value ?? null);
  };
}
