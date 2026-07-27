/**
 * The journal's on-disk half: the script mirror, and which script a relaunch runs.
 *
 * WHY THIS EXISTS. A workflow is the only place in bough where the same work is
 * deliberately run twice: you write a 40-agent script, one prompt is wrong, you fix it
 * and relaunch. The RECORD that makes the second run cheap is the `workflow_agents`
 * table, which `workflow/run.ts` writes and reads. The FILE that makes the edit
 * possible is this module: a run's script is mirrored to `~/.bough/workflows/<id>.js`
 * so the iteration loop is a file edit and not a re-POST (spec §8).
 *
 * THE INVARIANT THIS HOLDS: **the mirror is a working copy that may differ from the
 * row, and the difference is what the next relaunch consumes.** Everything here follows
 * from that:
 *
 *   - An existing mirror is never read-compared-rewritten by `syncScriptMirrors`, so a
 *     restart cannot clobber an edit the user has not run yet.
 *   - `resolveRerunScript` prefers the mirror over the stored row, because a relaunch
 *     that silently ran the row would replay the user's edit away — the failure would
 *     look like a working relaunch that ignored the fix.
 *   - The row stays canonical, so a relaunch is still possible after `~/.bough` has
 *     been cleaned out.
 *
 * PATH CONFINEMENT. A run id arrives from a URL and from a program's
 * `workflow.rerun({id})`, so it is not trusted to be a uuid: `mirrorPath` hands the
 * RELATIVE name to `confine()` before anything is written. The relative name and not
 * the joined path, because `join()` swallows a leading slash — `/etc/crontab` would
 * land back under the workflows directory and pass a check made after the join.
 *
 * WHAT IS NOT HERE, AND WHY IT WAS REMOVED (T5.8). This module used to carry a second
 * copy of the journal: its own `callKey`, its own replay index, its own rerun boundary
 * and its own rerun report — none of them reachable from the live path, and one of them
 * already drifted. The engine's default label is the prompt's first physical line; this
 * module's was the first non-empty line, trimmed. Same script, different key, so wiring
 * it would have made every relaunch of a workflow whose prompts carry CRLF or leading
 * whitespace miss cache entirely: money spent, no error, no signal. Two hashes of the
 * same thing is not redundancy, it is a bug with a delay fuse.
 *
 * So there is now exactly one of each, and none of them is here:
 *   - the key, the journal writes, and prefix-bounded replay — `workflow/run.ts`;
 *   - the relaunch decision (which run, which meta, which resolved model) —
 *     `workflow/control.ts`, which calls `resolveRerunScript` below for the script;
 *   - the counting — "37 replayed, 3 ran live" — `workflow/report.ts`.
 *
 * What remains is pure filesystem/string math with no engine dependency at all,
 * drivable with no worker and no LLM (plan §0).
 *
 * Ported from `src/workflow.ts` (`scriptPath` and the rerun script fallback).
 */

import { confine, workflowScriptPath, workflowsDir } from "../paths.ts";
import type { WorkflowRun } from "../schema/parts.ts";
import type { Db } from "../types.ts";

// ---------------------------------------------------------------------------
// The on-disk mirror
// ---------------------------------------------------------------------------

/**
 * `~/.bough/workflows/<id>.js`, confined.
 *
 * Confinement is on the server's own path construction, not on the program — programs
 * already write any file they like with the user's authority (spec §2).
 */
export function mirrorPath(runId: string): string {
  // The RELATIVE name is what is confined, not the already-joined path. `paths.ts`
  // still owns the layout — the guard runs first, and the accessor produces the value.
  confine(workflowsDir(), `${runId}.js`);
  return workflowScriptPath(runId);
}

/**
 * Write a run's script to its mirror. Returns whether the file is now on disk.
 *
 * Best-effort by contract: the database row is canonical and a run must not fail to
 * start because `~/.bough` is read-only or full. The boolean is for callers that report
 * the surface (`syncScriptMirrors`), not for control flow.
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
 * Boot wiring (`server/main.ts`). Idempotent and cheap in the steady state: an existing
 * file is never read, compared or rewritten, so a user's edit is never clobbered by a
 * restart — the whole point of the file is that it may differ from the row.
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
// Which script a relaunch runs
// ---------------------------------------------------------------------------

/** Where a relaunch's script came from — reported, because it decides what runs. */
export type ScriptSource = "explicit" | "mirror" | "stored";

/**
 * Resolve the script a relaunch should run: an explicit one wins, else the mirror the
 * user may have edited, else the stored row.
 *
 * The mirror before the row is the whole "edit the file, relaunch" loop (spec §8). The
 * row as the last resort is what keeps a relaunch possible after `~/.bough/workflows`
 * has been cleaned out. A blank override is not an override — an empty string is what a
 * form posts when the user cleared the box, not an instruction to run nothing.
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
