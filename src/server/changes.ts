/**
 * The Changes-tab backend: turn a session's snapshot state into review payloads and
 * apply/revert reviewed edits. Two snapshot sources feed the same Diff contract
 * (src/schema/changes.ts, see sandbox/INTEGRATION.md §2-4):
 *   - shadow — repo work. A session with a repo workspace runs in a shadow-git
 *     worktree; shadow.diff is the base..tip diff. Present when the workspace's
 *     `.git` file resolves to a bough shadow store.
 *   - clonefile — non-git config. Present when a snapshot dir with a manifest exists.
 * A session may have both, neither (→ empty), or one.
 *
 * Apply / revert semantics:
 *   - clonefile apply → copy the approved originals back (applyBack). This is the real
 *     mutation for config edits.
 *   - shadow apply → deliver the selected paths into the origin's working tree
 *     (shadow.materialize, content-level 3-way); when every changed path is covered,
 *     also seal the change (shadow.accept, described with the session title) so the
 *     rail clears.
 *   - shadow revert → per-path when `paths` is given (restore just those paths back to
 *     the session base), whole-change otherwise (shadow.undoAll).
 *   - clonefile revert is implicit: the originals stay pristine until you apply, so
 *     "revert" is simply not applying; there is no clonefile revert path.
 */
import { HttpError } from "../errors.ts";
import * as shadow from "../vcs/shadow.ts";
import * as clonefile from "../vcs/clonefile.ts";
import type { Db } from "../db/db.ts";
import type { ChangesApplyBody, Diff } from "../schema/changes.ts";

const MANIFEST = ".bough-manifest.json";

export interface ChangesOpts {
  /** clonefile snapshot root override (tests); else BOUGH_SNAPSHOT_BASE, else default. */
  snapshotBase?: string;
}

/** 400 for an unrevertable session, etc. */
export class ChangesError extends HttpError {}

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

/** The session's shadow worktree, or null (its `.git` file resolves to a bough store). */
async function hasShadowWorkspace(db: Db, sessionId: string): Promise<string | null> {
  const { workspace } = db.getSessionRuntime(sessionId);
  if (!workspace) return null;
  return (await shadow.originRepo(workspace)) !== null ? workspace : null;
}

/** All review payloads for a session (0..2), one per active snapshot source. */
export async function sessionChanges(
  db: Db,
  sessionId: string,
  opts: ChangesOpts = {},
): Promise<Diff[]> {
  const diffs: Diff[] = [];

  const sdir = await hasShadowWorkspace(db, sessionId);
  if (sdir) {
    try {
      diffs.push(await shadow.diff(sdir, sessionId));
    } catch (e) {
      console.error(`changes: shadow diff failed for ${sessionId}: ${(e as Error).message}`);
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

/** What an apply actually did — the UI's feedback line is built from this. */
export interface ApplyResult {
  /** Paths delivered/accepted this call. */
  applied: string[];
  /** The user's checkout the files landed in (external mode), else null. */
  origin: string | null;
  /** The session's git branch in the origin repo, when one exists. */
  branch: string | null;
  /** True when the whole change was covered and the jj change was sealed. */
  sealed: boolean;
}

/**
 * Apply reviewed changes. clonefile copies approved originals back. jj delivers:
 * for an external-mode session (isolated workspace, user's checkout elsewhere)
 * the selected paths are materialized into the origin checkout's working tree
 * (3-way, so user edits merge rather than clobber); when every changed path is
 * covered the change is also sealed (accept & advance, commit message = session
 * title) so the rail clears. Colocated sessions keep the legacy whole-change
 * accept — the edits are already on disk there.
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
  const title0 = db.getSession(sessionId)?.title;
  const sealMsg = title0 ? `bough: ${title0}` : "bough: session changes";
  if (body.source === "shadow") {
    // Shadow sessions are always external-style: materialize into the origin,
    // seal when every changed path was covered.
    const dir = await hasShadowWorkspace(db, sessionId);
    if (!dir) throw new ChangesError(400, "no shadow workspace to apply");
    const origin = await shadow.originRepo(dir);
    if (!origin) throw new ChangesError(400, "shadow workspace has no origin");
    const changed = (await shadow.diff(dir, sessionId)).files.map((f) => f.path);
    const paths = body.paths.length > 0 ? body.paths.filter((p) => changed.includes(p)) : changed;
    if (paths.length === 0) return { applied: [], origin, branch: null, sealed: false };
    try {
      await shadow.materialize(dir, sessionId, origin, paths);
    } catch (e) {
      throw new ChangesError(
        409,
        `apply to ${origin} failed — session history is at ${
          shadow.refFor(sessionId)
        } in its shadow store: ${(e as Error).message.split("\n").at(-1)}`,
      );
    }
    const coversAll = changed.every((p) => paths.includes(p));
    if (coversAll) await shadow.accept(dir, sessionId, sealMsg);
    return { applied: paths, origin, branch: null, sealed: coversAll };
  }
  throw new ChangesError(400, `unknown source ${body.source}`);
}

/**
 * Revert a jj-workspace session. With a non-empty `paths`, restore ONLY those paths
 * of the change back to its parent (`jj.revertPaths`), leaving the rest of the change
 * intact. With empty/absent `paths`, undo the whole change (`jj undo`). Returns the
 * list of paths actually reverted (empty for a whole-change undo). Throws if there's
 * no repo.
 */
export async function revertChanges(
  db: Db,
  sessionId: string,
  paths?: string[],
): Promise<string[]> {
  const sdir = await hasShadowWorkspace(db, sessionId);
  if (sdir) {
    if (paths && paths.length > 0) {
      await shadow.revertPaths(sdir, sessionId, paths);
      return paths;
    }
    return await shadow.undoAll(sdir, sessionId);
  }
  throw new ChangesError(400, "no workspace to revert");
}
