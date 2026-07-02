/**
 * The Changes-tab backend: turn a session's snapshot state into review payloads and
 * apply/revert reviewed edits. Two snapshot sources feed the same Diff contract
 * (src/schema/changes.ts, see sandbox/INTEGRATION.md §2-4):
 *   - jj — repo work. A session with a git-repo workspace has a `bough/<id>` change;
 *     jj.diff is the change-vs-parent diff. Present only when runtime.workspace is set
 *     and jj is initialised there (`.jj/`).
 *   - clonefile — non-git config. Present when a snapshot dir with a manifest exists.
 * A session may have both, neither (→ empty), or one.
 *
 * Apply / revert semantics (v1, per INTEGRATION §4):
 *   - clonefile apply → copy the approved originals back (applyBack). This is the real
 *     mutation for config edits.
 *   - jj apply → NO-OP acceptance: the working tree already holds the change, so there
 *     is nothing to move; apply just marks it reviewed.
 *   - jj revert → whole-change: `jj undo` (undo the most recent jj operation on the
 *     workspace). Per-path revert is deferred — `paths` is accepted but ignored — so
 *     revert immediately after review, before other jj ops intervene.
 *   - clonefile revert is implicit: the originals stay pristine until you apply, so
 *     "revert" is simply not applying; there is no clonefile revert path.
 */
import * as jj from "../vcs/jj.ts";
import * as clonefile from "../vcs/clonefile.ts";
import type { Db } from "../db/db.ts";
import type { ChangesApplyBody, Diff } from "../schema/changes.ts";

const MANIFEST = ".bough-manifest.json";

export interface ChangesOpts {
  /** clonefile snapshot root override (tests); else BOUGH_SNAPSHOT_BASE, else default. */
  snapshotBase?: string;
}

/** 400 for an unrevertable session, etc. */
export class ChangesError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "ChangesError";
  }
}

function snapBase(opts: ChangesOpts): string {
  return opts.snapshotBase ?? Deno.env.get("BOUGH_SNAPSHOT_BASE") ?? clonefile.snapshotBase();
}

async function isDir(path: string): Promise<boolean> {
  try {
    return (await Deno.stat(path)).isDirectory;
  } catch {
    return false;
  }
}
async function isFile(path: string): Promise<boolean> {
  try {
    return (await Deno.stat(path)).isFile;
  } catch {
    return false;
  }
}

/** True when the session's workspace is an initialised jj repo. */
async function hasJjWorkspace(db: Db, sessionId: string): Promise<string | null> {
  const { workspace } = db.getSessionRuntime(sessionId);
  return workspace && (await isDir(`${workspace}/.jj`)) ? workspace : null;
}

/** All review payloads for a session (0..2), one per active snapshot source. */
export async function sessionChanges(db: Db, sessionId: string, opts: ChangesOpts = {}): Promise<Diff[]> {
  const diffs: Diff[] = [];

  const repo = await hasJjWorkspace(db, sessionId);
  if (repo) {
    try {
      diffs.push(await jj.diff(repo, sessionId));
    } catch (e) {
      console.error(`changes: jj diff failed for ${sessionId}: ${(e as Error).message}`);
    }
  }

  const base = snapBase(opts);
  if (await isFile(`${clonefile.sessionDir(sessionId, base)}/${MANIFEST}`)) {
    try {
      diffs.push(await clonefile.diff(sessionId, base));
    } catch (e) {
      console.error(`changes: clonefile diff failed for ${sessionId}: ${(e as Error).message}`);
    }
  }

  return diffs;
}

/** Apply reviewed changes. clonefile copies approved originals back; jj is a no-op. */
export async function applyChanges(
  db: Db,
  sessionId: string,
  body: ChangesApplyBody,
  opts: ChangesOpts = {},
): Promise<void> {
  if (body.source === "clonefile") {
    await clonefile.applyBack(sessionId, body.paths, snapBase(opts));
  }
  // jj: acceptance only — the working tree already holds the change.
}

/** Revert a jj-workspace session (whole-change `jj undo`). Throws if there's no repo. */
export async function revertChanges(db: Db, sessionId: string): Promise<void> {
  const repo = await hasJjWorkspace(db, sessionId);
  if (!repo) throw new ChangesError(400, "no jj workspace to revert");
  await jj.undo(repo);
}
