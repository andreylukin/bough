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
 * Apply / revert semantics:
 *   - clonefile apply → copy the approved originals back (applyBack). This is the real
 *     mutation for config edits.
 *   - jj apply, external-mode session (isolated workspace) → deliver the selected
 *     paths into the origin checkout's working tree (jj.materialize, 3-way); when
 *     every changed path is covered, also seal the change (jj.accept, described with
 *     the session title) so the rail clears.
 *   - jj apply, colocated session → accept & advance (jj.accept): the edits already
 *     live in the checkout, sealing is the whole apply. Whole-change.
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
export async function sessionChanges(
  db: Db,
  sessionId: string,
  opts: ChangesOpts = {},
): Promise<Diff[]> {
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
  const repo = await hasJjWorkspace(db, sessionId);
  if (!repo) throw new ChangesError(400, "no jj workspace to apply");
  const title = db.getSession(sessionId)?.title;
  const message = title ? `bough: ${title}` : "bough: session changes";
  const origin = await jj.originRepo(repo);
  const external = origin !== null && (await Deno.realPath(origin)) !== (await Deno.realPath(repo));
  if (!external) {
    // Colocated: edits already live in the checkout; accepting is the whole apply.
    await jj.accept(repo, sessionId, message);
    return { applied: body.paths, origin: null, branch: jj.bookmarkFor(sessionId), sealed: true };
  }
  // External: resolve [] to "every changed path" so delivery and the seal test
  // work from one concrete list.
  const changed = (await jj.diff(repo, sessionId)).files.map((f) => f.path);
  const paths = body.paths.length > 0 ? body.paths.filter((p) => changed.includes(p)) : changed;
  if (paths.length === 0) return { applied: [], origin, branch: null, sealed: false };
  try {
    await jj.materialize(repo, sessionId, origin, paths);
  } catch (e) {
    throw new ChangesError(
      409,
      `apply to ${origin} failed — resolve by hand from branch ${jj.bookmarkFor(sessionId)}: ${
        (e as Error).message.split("\n").at(-1)
      }`,
    );
  }
  const coversAll = changed.every((p) => paths.includes(p));
  if (coversAll) await jj.accept(repo, sessionId, message);
  return { applied: paths, origin, branch: jj.bookmarkFor(sessionId), sealed: coversAll };
}

/** Revert a jj-workspace session (whole-change `jj undo`). Throws if there's no repo. */
export async function revertChanges(db: Db, sessionId: string): Promise<void> {
  const repo = await hasJjWorkspace(db, sessionId);
  if (!repo) throw new ChangesError(400, "no jj workspace to revert");
  await jj.undo(repo);
}
