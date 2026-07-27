/**
 * The journal — the record of what every `agent()` call asked for, and the thing
 * that makes a rerun cost only what the author edited.
 *
 * WHY THIS EXISTS. A workflow is the only place in bough where the same work is
 * deliberately run twice: you write a 40-agent script, one prompt is wrong, you fix
 * it and run it again. Without a journal that second run pays for all 40. With one it
 * pays for 1. That is the entire iteration loop for a workflow (spec §8), and it is
 * the reason `workflow_agents` exists as a table rather than as an event.
 *
 * THE INVARIANT THIS HOLDS: **`key` is a pure, total function of what the subagent
 * will be asked — and of nothing else.** Everything else here follows from that one
 * property, in both directions:
 *
 *   - *Stable.* Re-running an unchanged call must produce a byte-identical key, so
 *     the replay index hits. Nothing the run happens to be doing may leak in: not the
 *     run id, not the call's index, not the wall clock, not the display label the run
 *     view computed for it (`distinctLabel` picks a label from what the SIBLINGS
 *     already took, so a hash over it would make a call's key depend on the order its
 *     neighbours arrived in), and not the phase the run happened to be in when the
 *     call was made — only a phase the call itself named.
 *   - *Sensitive.* Changing the prompt, or any option that changes what the agent is
 *     asked — `label`, `phase`, `model`, `schema` — must produce a different key, or
 *     the rerun silently replays a stale answer to a question nobody asked. This is
 *     the failure mode that does not announce itself: it looks like a fast rerun and
 *     reads as wrong output.
 *
 * Two consequences worth stating, because they are load-bearing and not obvious:
 *
 *   - **Reordering a script invalidates nothing.** The key hashes the call, not its
 *     position, and the replay index is keyed rather than sequential. Moving three
 *     `agent()` calls around a script therefore replays all three — which is what a
 *     user editing a script expects, and what a naive index-based journal would get
 *     wrong on the first edit.
 *   - **Only successful calls replay.** A failed call re-runs live, because the
 *     failure is very often the thing the author just fixed. A `stopped` one likewise
 *     never ran to completion, so there is no answer to replay.
 *
 * DETERMINISM IS THE OTHER HALF OF THE BARGAIN. Replay is only sound if the script
 * asks the same questions when run twice, which is why `Date.now()`, argless
 * `new Date()` and `Math.random()` are unavailable inside the workflow worker
 * (`harness/wf_worker.ts`, plan §6.15). Without that ban this module is a lie the
 * first time a script stamps a timestamp into a prompt — and it fails as wrong
 * output, not as an error.
 *
 * THE MIRROR. A run's script is written to `~/.bough/workflows/<id>.js` so the
 * iteration loop is a file edit and not a re-POST (spec §8). The database row stays
 * canonical — the mirror is a working copy that `rerun()` prefers when the caller
 * named no script, and `syncScriptMirrors()` recreates at boot when the file is gone
 * (a fresh checkout, a cleaned `~/.bough`, a database restored on another machine).
 * Without that a run's "edit the file and press r" surface silently is not there, and
 * `rerun` quietly falls back to the stored script — replaying the user's edit away.
 *
 * WHAT IS NOT HERE. The engine — worker, semaphore, pause gate, journal WRITES — is
 * `workflow/run.ts`. This module is the read side, the key, the mirror and the rerun
 * decision, and it depends on no engine at all: `rerun()` takes its starter as a
 * parameter (plan §0, dependency injection over globals). That keeps the whole file
 * pure string/SQL/filesystem math, drivable with no worker and no LLM.
 *
 * Ported from `src/workflow.ts` (`callKey`, the `replay` map in `startWorkflow`,
 * `rerunWorkflow`, `scriptPath`). Deltas from that port are marked `NOTE:`.
 */

import { z } from "zod";
import { BadRequestError, ConflictError, NotFoundError } from "../errors.ts";
import { confine, workflowScriptPath, workflowsDir } from "../paths.ts";
import type { WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import type { Db } from "../types.ts";

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

/** The label budget, ellipsis included — it renders into a fixed-width rail. */
const LABEL_WIDTH = 40;

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

/**
 * The label a call gets when it passed none: the prompt's first non-empty line,
 * clipped. Deterministic on the prompt ALONE — see the header on why the key may not
 * hash the run view's `distinctLabel`, which is a function of the siblings.
 */
export function journalLabel(prompt: string): string {
  const first = prompt.trim().split("\n").find((l) => l.trim()) ?? "";
  return clip(first.trim(), LABEL_WIDTH);
}

/**
 * What one `agent()` call asks for. Structurally the engine's `AgentCall` with an
 * optional label, so either shape can be keyed: the engine defaults the label before
 * it journals, and this defaults it the same way for anyone keying a raw call.
 */
export interface JournalCall {
  prompt: string;
  /** Absent = derived from the prompt by `journalLabel`. */
  label?: string;
  phase?: string;
  model?: string;
  /** A JSON Schema (T5.3). Opaque — hashed, never interpreted. */
  schema?: unknown;
}

/**
 * Re-exported from the engine, deliberately. This module briefly carried its own
 * copy, and the two DRIFTED: the engine's default label is the first physical line
 * of the prompt, this one's was the first non-empty line, trimmed. Same script,
 * different key — so wiring this module would have made every rerun of a workflow
 * whose prompts carry CRLF or trailing whitespace miss cache entirely. Money spent,
 * no error, no signal. One key, one definition, one place to change it.
 */
export { callKey } from "./run.ts";

// ---------------------------------------------------------------------------
// Reading the journal
// ---------------------------------------------------------------------------

/**
 * May this row's result be replayed into a rerun?
 *
 * `done` is a call that ran and answered; `cached` is one that replayed an earlier
 * run's answer — both are answers, and a chain of reruns must not lose the result of
 * a call that has now replayed twice. Everything else (`error`, `stopped`, `queued`,
 * `running`) re-runs live. A null result is treated as no answer regardless of
 * status, because it is: there is nothing to hand the script.
 */
export function isReplayable(agent: WorkflowAgent): boolean {
  return (agent.status === "done" || agent.status === "cached") && agent.result !== null;
}

/**
 * A key → results-in-order map, consumed FIFO.
 *
 * FIFO per key rather than one result per key because N identical `agent()` calls are
 * a legitimate script (the same prompt against N different files it read from
 * `args`, say). They share a key, and they must replay their N results in the order
 * the source run produced them; a Map<key, result> would hand all N the first one.
 */
export type ReplayIndex = Map<string, string[]>;

/**
 * Build the replay index from a source run's journal, in call order.
 *
 * `runId` that does not exist, or a run with no journal, yields an empty index — a
 * rerun of a run that never got an agent out of the gate is a full live run, not an
 * error.
 */
export function replayIndex(db: Db, sourceRunId: string): ReplayIndex {
  const index: ReplayIndex = new Map();
  for (const agent of db.listWorkflowAgents(sourceRunId)) {
    if (!isReplayable(agent)) continue;
    const queue = index.get(agent.key) ?? [];
    queue.push(agent.result!);
    index.set(agent.key, queue);
  }
  return index;
}

/** Claim one replay for `key`, or `undefined` when the index has none left. */
export function takeReplay(index: ReplayIndex, key: string): string | undefined {
  return index.get(key)?.shift();
}

/** How many replays the index still holds — the ceiling on a rerun's instant hits. */
export function replayableCount(index: ReplayIndex): number {
  let n = 0;
  for (const queue of index.values()) n += queue.length;
  return n;
}

/**
 * What a finished rerun actually cost: which calls replayed and which ran.
 *
 * This is the surface behind the run view's "37 replayed, 3 ran" — the `cached`
 * status on a row is what makes that legible, and a rerun where every row says `done`
 * is a journal that silently missed (spec §8).
 */
export interface RerunReport {
  runId: string;
  /** The run this one replayed from, or null when it is not a rerun. */
  sourceId: string | null;
  total: number;
  /** Rows served from the journal — no agent call, no cost. */
  replayed: number;
  /** Rows that ran live and succeeded. */
  ran: number;
  failed: number;
  stopped: number;
  /** Still queued or running — non-zero only while the run is in flight. */
  pending: number;
  /** The prompts that cost an agent call, in call order. The edit, made visible. */
  ranPrompts: string[];
}

export function rerunReport(db: Db, runId: string): RerunReport {
  const run = db.getWorkflow(runId);
  if (!run) throw new NotFoundError(`workflow ${runId} not found`);
  const rows = db.listWorkflowAgents(runId);
  return {
    runId,
    sourceId: run.resumeOf,
    total: rows.length,
    replayed: rows.filter((r) => r.status === "cached").length,
    ran: rows.filter((r) => r.status === "done").length,
    failed: rows.filter((r) => r.status === "error").length,
    stopped: rows.filter((r) => r.status === "stopped").length,
    pending: rows.filter((r) => r.status === "queued" || r.status === "running").length,
    ranPrompts: rows.filter((r) => r.status !== "cached").map((r) => r.prompt),
  };
}

// ---------------------------------------------------------------------------
// The on-disk mirror
// ---------------------------------------------------------------------------

/**
 * `~/.bough/workflows/<id>.js`, confined.
 *
 * A run id reaches this from a URL and from a program's `workflow.rerun({id})`, so it
 * is not trusted to be a plain uuid: `confine` rejects `../../etc/crontab` before the
 * server writes anything (`paths.ts`). Confinement is on the server's own path
 * construction, not on the program — programs already write any file they like with
 * the user's authority (spec §2).
 */
export function mirrorPath(runId: string): string {
  // The RELATIVE name is what is confined, not the already-joined path: `join()`
  // swallows a leading slash, so `/etc/crontab` would land back under the workflows
  // directory and pass a check made after the join. `paths.ts` still owns the layout —
  // the guard runs first, and the accessor produces the value.
  confine(workflowsDir(), `${runId}.js`);
  return workflowScriptPath(runId);
}

/**
 * Write a run's script to its mirror. Returns whether the file is now on disk.
 *
 * Best-effort by contract: the database row is canonical and a run must not fail to
 * start because `~/.bough` is read-only or full. The boolean is for callers that
 * report the surface (`syncScriptMirrors`), not for control flow.
 */
export async function mirrorScript(runId: string, script: string): Promise<boolean> {
  try {
    const path = mirrorPath(runId);
    await Deno.mkdir(workflowsDir(), { recursive: true });
    await Deno.writeTextFile(path, script);
    return true;
  } catch {
    return false;
  }
}

/** A run's mirrored script, or `null` when there is no readable file. */
export async function readMirror(runId: string): Promise<string | null> {
  try {
    return await Deno.readTextFile(mirrorPath(runId));
  } catch {
    return null;
  }
}

/**
 * Recreate missing mirrors so "edit the script on disk" is true for every run the
 * database knows about. Returns the ids it wrote.
 *
 * Boot wiring (`server/main.ts`). Idempotent and cheap in the steady state: an
 * existing file is never read, compared or rewritten, so a user's edit is never
 * clobbered by a restart — the whole point of the file is that it may differ from the
 * row, and the difference is what the next rerun consumes.
 *
 * Bounded to the most recent `limit` runs: the mirror is an editing surface for work
 * someone is still iterating on, not an export of every run ever made.
 */
export async function syncScriptMirrors(
  db: Db,
  opts: { limit?: number } = {},
): Promise<string[]> {
  const limit = opts.limit ?? 50;
  const written: string[] = [];
  // `listWorkflows()` is newest-first, so this is the N most recent runs.
  for (const run of db.listWorkflows().slice(0, limit)) {
    let path: string;
    try {
      path = mirrorPath(run.id);
    } catch {
      continue; // an id that cannot name a file has no mirror; not fatal at boot
    }
    try {
      await Deno.stat(path);
      continue; // present — never overwritten, it may hold the user's edit
    } catch {
      // absent (or unreadable): write it below
    }
    if (await mirrorScript(run.id, run.script)) written.push(run.id);
  }
  return written;
}

// ---------------------------------------------------------------------------
// Rerun
// ---------------------------------------------------------------------------

/** Where a rerun's script came from — reported, because it decides what runs. */
export type ScriptSource = "explicit" | "mirror" | "stored";

/**
 * Resolve the script a rerun should run: an explicit one wins, else the mirror the
 * user may have edited, else the stored row.
 *
 * The mirror before the row is the whole "edit the file, press r" loop (spec §8). The
 * row as the last resort is what keeps a rerun possible after `~/.bough/workflows`
 * has been cleaned out.
 */
export async function resolveRerunScript(
  run: WorkflowRun,
  override?: string,
): Promise<{ script: string; from: ScriptSource }> {
  if (typeof override === "string" && override.trim()) {
    return { script: override, from: "explicit" };
  }
  const mirrored = await readMirror(run.id);
  if (mirrored !== null && mirrored.trim()) return { script: mirrored, from: "mirror" };
  return { script: run.script, from: "stored" };
}

/**
 * `workflow.rerun({id, script?})` at the boundary (spec §6, §8). Validated here so
 * the REST route and the program-side verb share one message when the shape is wrong.
 */
export const RerunRequest = z.object({
  id: z.string().min(1),
  /** Absent = the mirror, then the stored script. */
  script: z.string().min(1).optional(),
  /** Absent = the source run's input, verbatim. */
  args: z.unknown().optional(),
}).strict();
export type RerunRequest = z.infer<typeof RerunRequest>;

/** What a rerun needs from the engine, injected so this module depends on none. */
export interface RerunDeps {
  /**
   * Start the new run. Production passes `workflow/run.ts`'s `startWorkflow` bound to
   * a `WorkflowCtx`; a test passes a recorder.
   */
  start(opts: {
    sessionId: string;
    script: string;
    args?: unknown;
    resumeOf: string;
  }): Promise<WorkflowRun>;
  /**
   * Is the source run still executing in this process? Production passes
   * `isWorkflowLive`. Absent = assume not, which is right for a caller that has no
   * engine registry to ask.
   */
  isLive?(id: string): boolean;
}

export interface RerunResult {
  /** The new run. A rerun is a NEW row pointing back — nothing is rewritten. */
  run: WorkflowRun;
  /** The run it replays from. */
  source: WorkflowRun;
  /** Which script it is running. */
  script: ScriptSource;
  /**
   * How many journal entries are available to replay — the ceiling on the instant
   * hits, before the edited script decides how many it actually claims.
   */
  replayable: number;
}

/**
 * Rerun a finished run with journal replay: unchanged `agent()` calls return the
 * source run's results instantly, edited and new ones run live (spec §8).
 *
 * The rerun is a NEW run carrying `resumeOf`, never an edit of the old one — history
 * is a tree and nothing in bough is destructively rewritten (spec §2.4). That is also
 * what makes a chain of reruns work: each one journals `cached` rows that the next
 * one can replay in turn, so ten edits to one 40-agent script cost ten agent calls,
 * not four hundred.
 *
 * A run still live in this process is refused rather than raced: it is still writing
 * the journal this rerun would read, so its replay set is not yet a fact.
 */
export async function rerun(
  db: Db,
  request: unknown,
  deps: RerunDeps,
): Promise<RerunResult> {
  const parsed = RerunRequest.safeParse(request);
  if (!parsed.success) {
    throw new BadRequestError(
      `workflow.rerun({id, script?, args?}): ${
        parsed.error.issues.map((i) => `${i.path.join(".") || "request"}: ${i.message}`).join("; ")
      }`,
    );
  }
  const { id, script: override, args } = parsed.data;

  const source = db.getWorkflow(id);
  if (!source) throw new NotFoundError(`workflow ${id} not found`);
  if (deps.isLive?.(id)) {
    throw new ConflictError(
      `workflow ${id} is still running — stop it first, then rerun. Its journal is ` +
        `still being written, so a rerun now would replay a partial run.`,
    );
  }

  const { script, from } = await resolveRerunScript(source, override);
  const replayable = replayableCount(replayIndex(db, id));

  const run = await deps.start({
    sessionId: source.sessionId,
    script,
    // `undefined` means "keep the source run's input"; the engine reads it off the
    // source row. Passing `null` would silently blank a rerun's args.
    ...(args === undefined ? {} : { args }),
    resumeOf: id,
  });
  return { run, source, script: from, replayable };
}
