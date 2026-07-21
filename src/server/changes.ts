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
// otherwise read paths straight from the client's selection or shadow.diff, and
// parseGitDiff is untouched, so nothing here changes what a materialize can touch.
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

/** The session's shadow worktree, or null (its `.git` file resolves to a bough store). */
async function hasShadowWorkspace(db: Db, sessionId: string): Promise<string | null> {
  const { workspace } = db.getSessionRuntime(sessionId);
  if (!workspace) return null;
  return (await shadow.originRepo(workspace)) !== null ? workspace : null;
}

/** All review payloads for a session: one per active snapshot source (0..2),
 * plus one labeled section per direct subagent with unadopted branch work. */
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
  if (await isFile(`${clonefile.sessionDir(sessionId, base)}/${clonefile.MANIFEST}`)) {
    try {
      diffs.push(await clonefile.diff(sessionId, base));
    } catch (e) {
      console.error(`changes: clonefile diff failed for ${sessionId}: ${(e as Error).message}`);
    }
  }

  // Direct subagents with a branched workspace: their unadopted work is invisible
  // to the spawner's rail otherwise (the finish note asks the MODEL to adopt(),
  // which rarely happens). Each contributes a labeled, review-only section the UI
  // can adopt from; an adopted branch's diff is empty (its base advanced), so it
  // naturally drops out here.
  const spawnerDir = db.getSessionRuntime(sessionId).workspace;
  for (const sub of db.listSessions()) {
    if (sub.kind !== "subagent" || sub.originId !== sessionId) continue;
    const subDir = await hasShadowWorkspace(db, sub.id);
    if (!subDir || subDir === spawnerDir) continue;
    try {
      const d = await shadow.diff(subDir, sub.id);
      if (d.files.length === 0) continue;
      diffs.push({
        ...d,
        subagentId: sub.id,
        label: `${sub.title || "subagent"} (unadopted)`,
      });
    } catch (e) {
      console.error(`changes: subagent diff failed for ${sub.id}: ${(e as Error).message}`);
    }
  }

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
 * Apply reviewed changes, by source. `clonefile` copies the approved originals
 * back over the real config paths. `shadow` sessions are always external-style —
 * the session edits an isolated workspace and the user's checkout lives
 * elsewhere — so the selected paths are materialized into the origin checkout's
 * working tree (3-way, so concurrent user edits merge rather than clobber); when
 * the selection covers every changed path the change is also sealed (accept &
 * advance, commit message = session title) so the rail clears.
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
    // Match the display filter: noise files are never shown/selected, so they
    // must not count toward "covers everything" (else the seal never fires).
    const changed = (await shadow.diff(dir, sessionId)).files
      .map((f) => f.path).filter((p) => !isNoise(p));
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
 * Revert a shadow-workspace session. With a non-empty `paths`, restore ONLY those
 * paths of the change back to its parent (`shadow.revertPaths`), leaving the rest
 * of the change intact. With empty/absent `paths`, undo the whole change
 * (`shadow.undoAll`). Returns the list of paths actually reverted. Throws if the
 * session has no shadow workspace.
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
