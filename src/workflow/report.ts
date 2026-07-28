/**
 * What a run replayed, what it cost, and whether it is bigger than the user asked for.
 *
 * WHY THIS EXISTS. Two sentences in spec §8, both of them about a number nobody can
 * infer from the outcome:
 *
 *   - *"Any operation that replays returns how many calls were served from the journal
 *     and how many ran live."* A relaunch whose keys all HIT looks like a fast
 *     relaunch. A relaunch whose keys all MISS looks like a relaunch too — same 201,
 *     same run row, same eventual result — it just costs forty agents and several
 *     minutes instead of nothing. The two are indistinguishable from the response,
 *     from the events, and from the result, so a key defect (a label that drifted, an
 *     unhashed resolved model, a schema literal that reordered) can sit in the tree
 *     for a whole milestone with nothing looking wrong. Three did exactly that here,
 *     until an adversarial review counted rows by hand.
 *   - *"A run can spawn hundreds of agents and quietly become the most expensive thing
 *     in the product."* Cost is therefore a surface, not something reconstructed from
 *     the bill: tokens and elapsed time per agent and per phase, so an expensive stage
 *     is visible WHILE it runs.
 *
 * THE INVARIANT THIS HOLDS: **every journaled call is counted exactly once, in exactly
 * one bucket, and the buckets sum to the total.** `replayed + ranLive + pending ===
 * total`, always, for a run in any state. That is what makes the number safe to read as
 * money — a replayed call cost nothing, a live call cost an agent, and there is no
 * third thing quietly outside the arithmetic.
 *
 * `available` is the other half of the signal, and it is the half that names the defect
 * rather than the symptom. It counts the answers the SOURCE run offers, so
 * `available: 40, replayed: 0` says "there were forty answers here and this run's keys
 * matched none of them" — a broken key. `available: 0` says the source had nothing to
 * give, which is an ordinary full run and no defect at all. Reporting `replayed` alone
 * cannot tell those apart, and the second is the common case for a first run, so a bare
 * zero would cry wolf on every workflow anyone ever started.
 *
 * EVERYTHING HERE IS A FOLD OVER ROWS THE ENGINE WROTE. The counts come off
 * `workflow_agents`, which `workflow/run.ts` owns; nothing in this module decides what
 * replays and nothing here can turn a miss into a hit. That is deliberate: a report
 * that recomputed replay a second way could disagree with the journal, and then the
 * number that exists to expose a defect would be one. It also means these functions
 * answer for a finished run, a run still in flight, and a run the previous process
 * orphaned, with no engine, no worker and no LLM anywhere near them (plan §0: pure core,
 * clock injected).
 *
 * THE LARGE-RUN FLAG IS ADVICE, AND SO IS THE SIZE GUIDELINE. Neither pauses, throttles
 * or refuses anything — the flag is computed at VIEW time from rows that already exist,
 * so there is no code path from it back into the engine, which is the strongest form
 * "advisory" can take (spec §8: "it does not pause or throttle"). The guideline is a
 * target handed to whoever writes the script; a request that plainly calls for a
 * different scale overrides it.
 *
 * Supersedes `rerunReport`, which `workflow/journal.ts` carried and no response ever
 * called; T5.8 deleted it. One counting implementation, one place to change it.
 */

import { readFileSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";

import { BadRequestError, NotFoundError } from "../errors.ts";
import { workflowsDir } from "../paths.ts";
import type { WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import type { Db } from "../types.ts";
import {
  type CallPos,
  type Divergence,
  emptyReplayPlan,
  replayablePrefix,
  replayAudit,
  replayPlan,
} from "./run.ts";

// ---------------------------------------------------------------------------
// Replay reporting
// ---------------------------------------------------------------------------

/**
 * What one replaying operation cost: the `{replayed, ranLive, total}` spec §8 requires,
 * plus the context that makes a zero legible.
 */
export interface ReplaySummary {
  runId: string;
  /** The run this one replays from — `null` when it is a first run, not a relaunch. */
  sourceId: string | null;
  /** Calls served from the journal: no subagent, no cost. */
  replayed: number;
  /** Calls that ran an agent and settled — the ones this run paid for. */
  ranLive: number;
  /** Every call journaled so far. `replayed + ranLive + pending`. */
  total: number;
  /** Queued or running. Non-zero only while the run is in flight. */
  pending: number;
  /** `ranLive` split three ways, because "3 ran" and "3 failed" are different news. */
  succeeded: number;
  failed: number;
  stopped: number;
  /**
   * The ceiling on `replayed`: the longest LEADING run of answered calls the source
   * holds, because replay is prefix-bounded (spec §8) — an answer sitting behind a
   * failed call was never available to this run at all. Zero when there is no source.
   * A non-zero `available` with a zero `replayed` is the key-drift signal.
   */
  available: number;
  /** Has the run ended? Until it has, these are counts so far, not a bill. */
  final: boolean;
  /**
   * Where replay stopped in the SCRIPT, and why — edited, moved, added, or unanswered.
   * `null` when the prefix held, or when there was no journal to replay.
   *
   * This is the field that turns `replayed: 0, available: 40` from an alarm into a
   * diagnosis. The four kinds have four different fixes, and the one that used to be
   * misreported — a call whose key is untouched but whose POSITION moved — read as "its
   * key changed", which is the opposite of what happened and sent the reader looking at
   * prompts that were fine.
   *
   * Optional rather than required so a client can fabricate a summary — a TUI fixture,
   * a test — without asserting anything about replay. `summarize` always sets it.
   */
  diverged?: Divergence | null;
  /** `diverged?.pos`, lifted so a client can sort or link on it without unpacking. */
  divergedPos?: CallPos | null;
  /**
   * The prompts this run did NOT replay, in call order — the ones an agent ran, is
   * running, or is queued to run. On a relaunch this is the edit, made visible: if it
   * holds a prompt you did not touch, a key drifted.
   */
  livePrompts: string[];
  /** The one-line human form. Carried on the wire so every client says the same thing. */
  line: string;
}

/** Buckets a row exactly once. The one place the status → bucket mapping lives. */
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

/** A run whose worker is no longer executing — its counts will not move again. */
function isFinal(run: WorkflowRun): boolean {
  return run.status !== "running" && run.status !== "paused";
}

/**
 * Count one run's journal.
 *
 * Throws `NotFoundError` on an unknown id rather than reporting zeroes, because
 * "nothing replayed" and "no such run" are the same shape and opposite problems: the
 * first is a defect in the key, the second is a defect in the caller.
 */
export function replaySummary(db: Db, runId: string): ReplaySummary {
  const run = db.getWorkflow(runId);
  if (!run) throw new NotFoundError(`workflow ${runId} not found`);
  return summarize(db, run);
}

/** `replaySummary` for a row the caller already has. Saves the re-read, same answer. */
export function summarize(db: Db, run: WorkflowRun): ReplaySummary {
  const rows = db.listWorkflowAgents(run.id);
  const counts = { replayed: 0, pending: 0, succeeded: 0, failed: 0, stopped: 0 };
  const livePrompts: string[] = [];
  for (const row of rows) {
    const where = bucket(row);
    counts[where]++;
    if (where !== "replayed") livePrompts.push(row.prompt);
  }
  // Read through the ENGINE's own fold, for the same reason `available` is: a report
  // that re-derived where the prefix broke could disagree with the run that broke it.
  const plan = run.resumeOf ? replayPlan(db, run.resumeOf) : emptyReplayPlan();
  const audit = replayAudit(plan, rows);
  const summary: Omit<ReplaySummary, "line"> = {
    runId: run.id,
    sourceId: run.resumeOf,
    replayed: counts.replayed,
    ranLive: counts.succeeded + counts.failed + counts.stopped,
    total: rows.length,
    pending: counts.pending,
    succeeded: counts.succeeded,
    failed: counts.failed,
    stopped: counts.stopped,
    // The SOURCE's journal, not this run's: what was on offer, whether or not any of it
    // was claimed. Read through the ENGINE's own plan (`workflow/run.ts`) rather than a
    // second walk over the rows — a ceiling computed a different way could exceed what
    // the engine would ever hand out, and then `available > replayed` would read as
    // drift on a run that replayed everything it could.
    available: run.resumeOf ? replayablePrefix(plan) : 0,
    final: isFinal(run),
    diverged: audit.diverged,
    divergedPos: audit.diverged?.pos ?? null,
    livePrompts,
  };
  return { ...summary, line: replayLine(summary) };
}

/**
 * The one-line human form — the completion note, a CLI line, a run-view header.
 *
 * Written so the failure reads as a failure. "0 replayed of 12 available" is a sentence
 * someone stops on; "12 agents ran" is one they scroll past, and they are the same run.
 */
export function replayLine(s: Omit<ReplaySummary, "line">): string {
  if (s.total === 0) {
    if (s.pending > 0) return "no calls journaled yet";
    return s.sourceId && s.available > 0
      ? `no agent calls — ${s.available} were available to replay`
      : "no agent calls";
  }
  const parts = [`${s.replayed} replayed`, `${s.ranLive} ran live`];
  if (s.pending > 0) parts.push(`${s.pending} still going`);
  const head = `${parts.join(", ")} of ${s.total}`;
  if (s.sourceId && s.available > 0 && s.replayed === 0) {
    // NOT "every key changed". That sentence was true for an edited script and false —
    // in the most misleading possible way — for a run whose calls kept their keys and
    // changed POSITION, which is the shape a barrier-free pipeline used to produce on
    // every relaunch. The surface that exists to make a key defect visible has to say
    // which defect it is looking at.
    return `${head} — replayed NOTHING of ${s.available} available: ${
      s.diverged?.reason ?? "the first call already differed"
    }`;
  }
  if (s.sourceId && s.diverged) {
    return `${head} (${s.available} available to replay); replay stopped at ` +
      `${s.diverged.pos} — ${s.diverged.reason}`;
  }
  if (s.sourceId) return `${head} (${s.available} available to replay)`;
  return head;
}

// ---------------------------------------------------------------------------
// Cost: tokens and elapsed time, per agent and per phase
// ---------------------------------------------------------------------------

/** One `agent()` call's bill. A replayed call has no session, and therefore no cost. */
export interface AgentCost {
  agentId: string;
  label: string;
  phase: string | null;
  status: WorkflowAgent["status"];
  sessionId: string | null;
  /** Input + output tokens on the backing subagent session. `0` for a replay. */
  tokens: number;
  /** `finishedAt - startedAt`, or time so far for a call still running. */
  elapsedMs: number;
  /** Did this call cost an agent, or was it served from the journal? */
  replayed: boolean;
}

/**
 * One phase's bill.
 *
 * `elapsedMs` is AGENT time, not wall time: calls inside a phase run concurrently up to
 * the run's semaphore, so summing them overstates the clock and understates nothing.
 * That is the number that answers "which stage is expensive" — the wall clock of a
 * phase is mostly a statement about the semaphore.
 */
export interface PhaseCost {
  phase: string | null;
  agents: number;
  replayed: number;
  tokens: number;
  elapsedMs: number;
}

export interface RunCost {
  runId: string;
  agents: number;
  replayed: number;
  tokens: number;
  /** Summed agent time. See `PhaseCost.elapsedMs`. */
  agentMs: number;
  /** The run's own clock: `finishedAt - createdAt`, or time so far. */
  wallMs: number;
  byPhase: PhaseCost[];
  byAgent: AgentCost[];
}

/**
 * Tokens and elapsed time for one run, per agent and per phase.
 *
 * Tokens come from the backing subagent session's usage totals, which the turn runner
 * writes as each round settles — so a running agent's number grows while you watch it,
 * which is the entire point of putting it in the run view (spec §8: "visible while it
 * is running rather than in the bill").
 */
export function runCost(db: Db, run: WorkflowRun, now: () => number = Date.now): RunCost {
  const at = now();
  const byAgent: AgentCost[] = db.listWorkflowAgents(run.id).map((a) => {
    const replayed = a.status === "cached";
    const usage = a.sessionId ? db.sessionUsage(a.sessionId) : null;
    return {
      agentId: a.id,
      label: a.label,
      phase: a.phase,
      status: a.status,
      sessionId: a.sessionId,
      // A replay has no session and no usage: it did not call a model. Counting it as
      // zero is the accounting claim the journal makes, stated in the ledger.
      tokens: usage ? usage.inputTokens + usage.outputTokens : 0,
      elapsedMs: Math.max(0, (a.finishedAt ?? at) - a.startedAt),
      replayed,
    };
  });

  const phases = new Map<string, PhaseCost>();
  for (const a of byAgent) {
    const key = a.phase ?? "";
    let row = phases.get(key);
    if (!row) {
      phases.set(key, row = { phase: a.phase, agents: 0, replayed: 0, tokens: 0, elapsedMs: 0 });
    }
    row.agents++;
    if (a.replayed) row.replayed++;
    row.tokens += a.tokens;
    row.elapsedMs += a.elapsedMs;
  }

  return {
    runId: run.id,
    agents: byAgent.length,
    replayed: byAgent.filter((a) => a.replayed).length,
    tokens: byAgent.reduce((n, a) => n + a.tokens, 0),
    agentMs: byAgent.reduce((n, a) => n + a.elapsedMs, 0),
    wallMs: Math.max(0, (run.finishedAt ?? at) - run.createdAt),
    byPhase: [...phases.values()],
    byAgent,
  };
}

// ---------------------------------------------------------------------------
// The size guideline
// ---------------------------------------------------------------------------

/**
 * How many agents a generated script should AIM for. Advice to whoever writes the
 * script — the model, or a person — and never a cap: nothing consults this before
 * dispatching a call, and a request that plainly calls for a different scale overrides
 * it (spec §8).
 */
export type SizeGuideline = "small" | "medium" | "large" | "unrestricted";

/** The count each guideline targets. `unrestricted` has none, hence `Infinity`. */
export const GUIDELINE_TARGET: Record<SizeGuideline, number> = {
  small: 5,
  medium: 15,
  large: 50,
  unrestricted: Infinity,
};

export const DEFAULT_GUIDELINE: SizeGuideline = "medium";

const GUIDELINES: readonly SizeGuideline[] = ["small", "medium", "large", "unrestricted"];

/** Parse a stored or posted value. Returns `null` for anything that is not one. */
export function parseGuideline(value: unknown): SizeGuideline | null {
  const name = String(value ?? "").trim().toLowerCase();
  return (GUIDELINES as readonly string[]).includes(name) ? name as SizeGuideline : null;
}

/** Parse or throw the 400 the route renders — one message, both entry points. */
export function requireGuideline(value: unknown): SizeGuideline {
  const parsed = parseGuideline(value);
  if (parsed) return parsed;
  throw new BadRequestError(
    `unknown workflow size guideline ${JSON.stringify(value)} — it is one of ` +
      `${GUIDELINES.join(", ")}. It is advice to whoever writes the script (aim for ` +
      `fewer than this many agents), never a cap on what a run may do.`,
  );
}

/**
 * Where the setting lives: `~/.bough/workflows/size-guideline`, one word.
 *
 * NOTE / surfaced rather than worked around: `paths.ts` owns the `~/.bough` layout and
 * has no accessor for this file, and it is not this task's to edit. The name is built
 * from `workflowsDir()` here, in one function, so it is still one place — but the
 * honest home for it is a `sizeGuidelinePath()` beside `workflowScriptPath()`.
 */
export function guidelinePath(): string {
  return `${workflowsDir()}/size-guideline`;
}

/**
 * The active guideline: the stored setting, else `BOUGH_WORKFLOW_SIZE`, else `medium`.
 *
 * Read SYNCHRONOUSLY and on every call, because its readers are view functions
 * (`workflowDetail`) that a route renders per request. The file is one word; a cache
 * here would be a staleness bug traded for nothing measurable, and the plan forbids
 * speculative optimization (§0).
 */
export function activeGuideline(): SizeGuideline {
  try {
    const stored = parseGuideline(readFileSync(guidelinePath(), "utf8"));
    if (stored) return stored;
  } catch {
    // No file, or no permission to read it: fall through to the environment.
  }
  return parseGuideline(process.env["BOUGH_WORKFLOW_SIZE"]) ?? DEFAULT_GUIDELINE;
}

/** Persist the guideline. Returns what was stored, so a caller can echo it back. */
export async function setGuideline(value: unknown): Promise<SizeGuideline> {
  const guideline = requireGuideline(value);
  await mkdir(workflowsDir(), { recursive: true });
  await writeFile(guidelinePath(), `${guideline}\n`);
  return guideline;
}

/**
 * The sentence handed to whoever writes the script. Phrased as a target with an
 * explicit override clause, because a guideline the model reads as a hard cap produces
 * a script that under-fans a job that genuinely needs 200 agents.
 */
export function guidelineAdvice(guideline: SizeGuideline = activeGuideline()): string {
  if (guideline === "unrestricted") {
    return "Workflow size guideline: unrestricted — fan out as wide as the job needs.";
  }
  const target = GUIDELINE_TARGET[guideline];
  return `Workflow size guideline: ${guideline} — aim for fewer than ${target} agents in a ` +
    `generated script. This is advice, not a cap: if the request plainly needs a wider ` +
    `fan-out, write it and say why.`;
}

// ---------------------------------------------------------------------------
// The large-run flag
// ---------------------------------------------------------------------------

/** Projected tokens above which a run is flagged. `BOUGH_WORKFLOW_TOKEN_WARN` moves it. */
export function tokenWarnThreshold(): number {
  const n = Number(process.env["BOUGH_WORKFLOW_TOKEN_WARN"]);
  return Number.isFinite(n) && n > 0 ? n : 1_000_000;
}

/**
 * A run that is bigger than the guideline, or on course to cost more than the token
 * threshold.
 *
 * ADVISORY, and structurally so: this is computed from rows that already exist, at the
 * moment a view is rendered, and nothing in the engine reads it. There is no path from
 * a flag to a pause, a throttle or a refused call — the run proceeds exactly as it
 * would with the flag absent (spec §8). `stop` names the control that DOES stop it,
 * because a warning with no adjacent action is a warning people learn to ignore.
 */
export interface LargeRunFlag {
  flagged: true;
  advisory: true;
  guideline: SizeGuideline;
  /** The guideline's count, or `null` for `unrestricted`. */
  target: number | null;
  /** Calls journaled so far. A run still scheduling may exceed this. */
  scheduled: number;
  tokens: number;
  projectedTokens: number;
  tokenThreshold: number;
  /** One sentence per reason it is flagged. Never empty. */
  reasons: string[];
  /** The control that stops it. A warning names its own remedy. */
  stop: string;
}

/**
 * Project this run's final token total from what has settled.
 *
 * Live calls only: a replayed call spends nothing, so averaging it in would drag the
 * projection toward zero exactly when a relaunch is running the expensive tail live.
 * A run with nothing settled projects what it has spent — a floor, never a guess.
 */
export function projectTokens(cost: RunCost): number {
  const settled = cost.byAgent.filter((a) => !a.replayed && a.status !== "queued");
  const finished = settled.filter((a) => a.status !== "running");
  if (finished.length === 0) return cost.tokens;
  const average = finished.reduce((n, a) => n + a.tokens, 0) / finished.length;
  const unfinished = cost.byAgent.filter(
    (a) => !a.replayed && (a.status === "queued" || a.status === "running"),
  ).length;
  return Math.round(cost.tokens + average * unfinished);
}

/**
 * Flag a run that schedules more than the guideline's count, or whose projected tokens
 * cross the threshold. `null` when neither is true — an ordinary run carries no flag.
 */
export function largeRunFlag(
  cost: RunCost,
  guideline: SizeGuideline = activeGuideline(),
  threshold: number = tokenWarnThreshold(),
): LargeRunFlag | null {
  const target = GUIDELINE_TARGET[guideline];
  const projectedTokens = projectTokens(cost);
  const reasons: string[] = [];
  if (Number.isFinite(target) && cost.agents > target) {
    reasons.push(
      `${cost.agents} agents scheduled, past the ${guideline} guideline of ${target}`,
    );
  }
  if (projectedTokens > threshold) {
    reasons.push(
      `projected ${projectedTokens.toLocaleString("en-US")} tokens, past the ` +
        `${threshold.toLocaleString("en-US")} warning threshold`,
    );
  }
  if (reasons.length === 0) return null;
  return {
    flagged: true,
    advisory: true,
    guideline,
    target: Number.isFinite(target) ? target : null,
    scheduled: cost.agents,
    tokens: cost.tokens,
    projectedTokens,
    tokenThreshold: threshold,
    reasons,
    stop: `POST /workflows/${cost.runId}/stop`,
  };
}

/** The whole cost surface for one run, as `GET /workflows/:id` carries it. */
export interface RunAccounting {
  replay: ReplaySummary;
  cost: RunCost;
  /** `null` when the run is within its guideline and under the token threshold. */
  warning: LargeRunFlag | null;
  guideline: SizeGuideline;
}

export function runAccounting(
  db: Db,
  run: WorkflowRun,
  opts: { guideline?: SizeGuideline; threshold?: number; now?: () => number } = {},
): RunAccounting {
  const guideline = opts.guideline ?? activeGuideline();
  const cost = runCost(db, run, opts.now ?? Date.now);
  return {
    replay: summarize(db, run),
    cost,
    warning: largeRunFlag(cost, guideline, opts.threshold ?? tokenWarnThreshold()),
    guideline,
  };
}
