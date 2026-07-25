/**
 * The Changes-tab backend: turn a session's snapshot state into review payloads and
 * apply/revert reviewed edits. Two snapshot sources feed the same Diff contract
 * (src/schema/changes.ts):
 *   - repo — the session edits the user's checkout directly, so the change set is
 *     `git diff <session base>` plus untracked files (vcs/repodiff.ts). Present
 *     when the workspace is a git repo.
 *   - clonefile — non-git config. Present when a snapshot dir with a manifest exists.
 * A session may have both, neither (→ empty), or one.
 *
 * Apply / revert semantics:
 *   - clonefile apply → copy the approved originals back (applyBack). This is the real
 *     mutation for config edits.
 *   - repo apply → nothing to deliver. The edits are already in the user's working
 *     tree; delivery is the reviewer's own `git commit`. Reported as such rather than
 *     silently succeeding.
 *   - repo revert → restore the selected paths from the session's base sha, or
 *     delete them when the session created them (repodiff.revertPaths). With no
 *     paths, revert everything the session changed.
 *   - clonefile revert is implicit: the originals stay pristine until you apply, so
 *     "revert" is simply not applying; there is no clonefile revert path.
 */
import { HttpError } from "../errors.ts";
import * as repodiff from "../vcs/repodiff.ts";
import * as clonefile from "../vcs/clonefile.ts";
import type { Db } from "../db/db.ts";
import type { ChangesApplyBody, Diff } from "../schema/changes.ts";

export interface ChangesOpts {
  /** clonefile snapshot root override (tests); else BOUGH_SNAPSHOT_BASE, else default. */
  snapshotBase?: string;
}

/** 400 for an unrevertable session, etc. */
export class ChangesError extends HttpError {}

function snapBase(opts: ChangesOpts): string {
  return opts.snapshotBase ?? Deno.env.get("BOUGH_SNAPSHOT_BASE") ?? clonefile.snapshotBase();
}

// Display noise: build/cache artifacts that clutter the review list. Filtered
// from what the Changes rail shows AND from the apply "covers everything" seal
// check, so they neither distract the reviewer nor block sealing. Apply/revert
// otherwise read paths straight from the client's selection or repodiff.diffSince.
const NOISE_SEGMENTS = ["__pycache__", "node_modules", ".pytest_cache", ".mypy_cache"];
const NOISE_BASENAMES = [".DS_Store"];
const NOISE_SUFFIXES = [".pyc", ".pyo"];

function isNoise(path: string): boolean {
  const segs = path.split("/");
  const base = segs.at(-1) ?? "";
  return NOISE_SEGMENTS.some((s) => segs.includes(s)) ||
    NOISE_BASENAMES.includes(base) ||
    NOISE_SUFFIXES.some((s) => base.endsWith(s));
}

async function isFile(path: string): Promise<boolean> {
  try {
    return (await Deno.stat(path)).isFile;
  } catch {
    return false;
  }
}

/**
 * A session's repo change set, or null when it has none. The workspace IS the
 * user's checkout — git ops run right there — and `base` is the sha the session
 * started from, recorded on its first turn.
 */
interface RepoSource {
  dir: string;
  /** The sha the session started from; the rail diffs against it. */
  base: string | null;
}

async function repoSource(db: Db, sessionId: string): Promise<RepoSource | null> {
  const { workspace, base } = db.getSessionRuntime(sessionId);
  if (!workspace || !(await repodiff.isRepo(workspace))) return null;
  return { dir: workspace, base: base && base !== "empty" ? base : null };
}

/** All review payloads for a session: one per active snapshot source (0..2). */
export async function sessionChanges(
  db: Db,
  sessionId: string,
  opts: ChangesOpts = {},
): Promise<Diff[]> {
  const diffs: Diff[] = [];

  const src = await repoSource(db, sessionId);
  if (src) {
    try {
      diffs.push(await repodiff.diffSince(src.dir, src.base));
    } catch (e) {
      console.error(`changes: repo diff failed for ${sessionId}: ${(e as Error).message}`);
    }
  }

  const base = snapBase(opts);
  if (await isFile(`${clonefile.sessionDir(sessionId, base)}/${clonefile.MANIFEST}`)) {
    try {
      diffs.push(await clonefile.diff(sessionId, base));
    } catch (e) {
      console.error(`changes: clonefile diff failed for ${sessionId}: ${(e as Error).message}`);
    }
  }

  // No subagent sections: subagents share the spawner's workspace now (there are
  // no per-session branches to adopt), so their work is already in the diff above.

  // Display filter only — drop build/cache noise from each section's file list.
  return diffs.map((d) => ({ ...d, files: d.files.filter((f) => !isNoise(f.path)) }));
}

/** What an apply actually did — the UI's feedback line is built from this. */
export interface ApplyResult {
  /** Paths delivered/accepted this call. */
  applied: string[];
  /** The user's checkout the files landed in (external mode), else null. */
  origin: string | null;
  /** The session's git branch in the origin repo, when one exists. */
  branch: string | null;
  /** True when every changed path was covered, so the change was also sealed. */
  sealed: boolean;
}

/**
 * Apply reviewed changes, by source. `clonefile` copies the approved originals back
 * over the real config paths — a real mutation. `shadow` has nothing to apply: the
 * session edits the checkout directly, so the files are already in place.
 */
export async function applyChanges(
  db: Db,
  sessionId: string,
  body: ChangesApplyBody,
  opts: ChangesOpts = {},
): Promise<ApplyResult> {
  if (body.source === "clonefile") {
    await clonefile.applyBack(sessionId, body.paths, snapBase(opts));
    return { applied: body.paths, origin: null, branch: null, sealed: false };
  }
  if (body.source === "repo") {
    // Nothing to deliver: the session edited the user's checkout in place, so the
    // work is already where an apply used to put it. Say so instead of reporting a
    // successful delivery that moved no bytes — committing is the reviewer's call
    // (or the agent's `git commit`).
    const src = await repoSource(db, sessionId);
    if (!src) throw new ChangesError(400, "no repo workspace");
    return { applied: [], origin: src.dir, branch: null, sealed: false };
  }
  throw new ChangesError(400, `unknown source ${body.source}`);
}

/**
 * Revert the session's work: restore `paths` from the session's base sha (deleting
 * the ones it created), or everything it changed when `paths` is empty. Returns the
 * paths actually reverted. Throws if the session has no repo workspace.
 */
export async function revertChanges(
  db: Db,
  sessionId: string,
  paths?: string[],
): Promise<string[]> {
  const src = await repoSource(db, sessionId);
  if (!src) throw new ChangesError(400, "no workspace to revert");
  const targets = paths && paths.length > 0
    ? paths
    : (await repodiff.diffSince(src.dir, src.base)).files.map((f) => f.path);
  return await repodiff.revertPaths(src.dir, src.base, targets);
}
