/**
 * The Changes rail: a session's review payload, and the one mutation over it.
 *
 * THE INVARIANT THIS HOLDS: **revert never touches a path the session did not
 * change** (spec §13). That is enforced here rather than assumed of the caller. A
 * revert request is intersected with the change set the rail is showing right now,
 * and anything outside it is reported back as skipped instead of being restored —
 * because `git checkout <base> -- <path>` is perfectly happy to rewrite a file this
 * session never opened, and a client passing a stale or hand-typed path would
 * otherwise silently clobber the user's own uncommitted work. The old tree left
 * this "true by construction" (`src/vcs/repodiff.ts`: "the caller passes what the
 * diff reported"), which is a comment, not a guarantee.
 *
 * Second: **a workspace that is not a repository degrades, it does not fail.** No
 * repo means no base, which means no change set — the rail says exactly that, with
 * the reason (spec §13), and the session keeps working. The only 400 here is a
 * revert asked of a session that has nothing to revert against, and its message is
 * that same reason.
 *
 * There is no apply. It existed for the non-git snapshotting this design dropped
 * (spec §17): the agent edits the user's checkout in place, so the work is already
 * where an apply would have put it, and delivery is the reviewer's own `git commit`.
 *
 * NO EVENT IS PUBLISHED on revert, deliberately. Spec §3's event set is closed and
 * has no changes event; the rail is a fetch-on-demand surface, and the response
 * carries the whole outcome, so a client re-reads `GET /sessions/:id/changes` and
 * reconciles — the same rule as reconnect (events are display transport, the
 * database and the working tree are the truth).
 *
 * Ported from `src/server/changes.ts`, dropping the clonefile source and the apply
 * path. Deltas are marked `NOTE:`.
 */
import { BadRequestError, NotFoundError } from "../errors.ts";
import { RevertChangesBody } from "../schema/requests.ts";
import type { AppCtx, Db } from "../types.ts";
import { type ChangeSet, changeSet, type FileDiff, revertPaths } from "../vcs/repodiff.ts";
import { type Handler, json, parseBody } from "./http.ts";

/** The rail's payload: the git change set plus the checkout it was measured in. */
export interface SessionChangeSet extends ChangeSet {
  /** The session's checkout, or null when it never named one. */
  workspace: string | null;
}

// ---- display noise -----------------------------------------------------------
//
// Build and cache artifacts clutter the review list and are never what a reviewer
// came to look at. Filtered from what the rail SHOWS — and therefore, since revert
// can only touch what was shown, from what a revert can delete. That direction is
// deliberate: leaving a stale `__pycache__` behind is a nuisance, deleting files the
// user was never shown is a surprise.

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

// ---- the change set ----------------------------------------------------------

function requireSession(ctx: AppCtx, id: string): void {
  if (!ctx.db.getSession(id)) {
    throw new NotFoundError(
      `no session ${id} — changes are per session, so open one that exists ` +
        `(GET /sessions lists them).`,
    );
  }
}

/**
 * A session's change set: `git diff <base>` plus untracked files, in the session's
 * own checkout.
 *
 * A session with no workspace answers unavailable rather than falling back to the
 * server's own directory the way a turn does. The fallback is right for RUNNING a
 * program — something has to be the cwd — and wrong here: attributing whatever is
 * uncommitted in bough's own checkout to a session that never named one would
 * report a stranger's work as the agent's, and offer to revert it.
 */
export async function sessionChanges(db: Db, sessionId: string): Promise<SessionChangeSet> {
  const { workspace, base } = db.getSessionRuntime(sessionId);
  if (!workspace) {
    return {
      available: false,
      reason: "this session has no workspace, so there is no checkout to diff. " +
        "Create a session with a `workspace` to get a Changes rail.",
      base: null,
      files: [],
      workspace: null,
    };
  }
  const set = await changeSet(workspace, base);
  return { ...set, files: set.files.filter((f) => !isNoise(f.path)), workspace };
}

// ---- revert ------------------------------------------------------------------

/** What a revert did, said in full: nothing here is inferred by the client. */
export interface RevertOutcome {
  /** Paths restored from the base sha, or deleted because the session created them. */
  reverted: string[];
  /** Requested paths that are not in the session's change set — left untouched. */
  skipped: string[];
  /** Paths that are the session's but could not be reverted, with git's reason. */
  failed: { path: string; error: string }[];
}

/**
 * A requested path as it appears in the change set, or null if it is not one.
 *
 * Only cosmetic normalization (`./x` → `x`, trailing slash) is done. An absolute
 * path or a `..` escape is not resolved into a match — it is simply not found, and
 * lands in `skipped`. Resolving them would be re-implementing path confinement in
 * the one place where being lenient means writing outside the change set.
 */
function matchPath(requested: string, changed: Set<string>): string | null {
  const trimmed = requested.trim().replace(/^\.\//, "").replace(/\/+$/, "");
  return changed.has(trimmed) ? trimmed : null;
}

/**
 * Revert the session's work on `paths` — or on everything the rail is showing when
 * `paths` is ABSENT.
 *
 * **An explicit `paths: []` selects nothing and is refused**, and the difference
 * between "absent" and "explicitly empty" is the whole point. Revert is the only
 * destructive operation in the product and it is unbounded — the change set of a
 * session opened in a dirty checkout is every uncommitted file in it, because
 * `base` is the sha the session started from (spec §13) and nothing distinguishes
 * work the agent did from work that was already there. So the one input a caller
 * produces by ACCIDENT — a selection loop that yielded no rows, a UI with nothing
 * highlighted, a `paths` variable that came back empty — must not be the input that
 * means "destroy all of it". Conflating them makes an empty selection the most
 * destructive request in the API, which is precisely backwards.
 *
 * Revert-all is still reachable and still one call: omit `paths` entirely. That is
 * a request nobody sends by mistake, and it is what `api.revertChanges(id)` sends.
 *
 * Throws 400 when the session has no change set to revert against, carrying the
 * reason the rail displays, so the human reads the same sentence in both places.
 */
export async function revertChanges(
  db: Db,
  sessionId: string,
  paths?: string[],
): Promise<RevertOutcome> {
  if (paths && paths.length === 0) {
    throw new BadRequestError(
      "revert was given an empty `paths` selection, so it reverted nothing. An empty " +
        "list is not a wildcard — it is almost always a client that selected no rows, " +
        "and revert deletes files. To revert one or more paths, name them; to revert " +
        "the WHOLE change set, omit `paths` from the body entirely.",
    );
  }

  const set = await sessionChanges(db, sessionId);
  if (!set.available || !set.base || !set.workspace) {
    throw new BadRequestError(`nothing to revert: ${set.reason ?? "no change set"}`);
  }

  const changed = new Set(set.files.map((f: FileDiff) => f.path));
  const targets: string[] = [];
  const skipped: string[] = [];
  // NOTE: the enforcement the old tree only claimed. The selection is intersected
  // with the change set the rail is showing — never a wildcard git resolves, so a
  // path that scrolled off the rail is unreachable.
  for (const requested of paths ?? []) {
    const match = matchPath(requested, changed);
    if (match) targets.push(match);
    else skipped.push(requested);
  }
  const selection = paths ? targets : [...changed];

  const { reverted, failed } = await revertPaths(set.workspace, set.base, selection);
  return { reverted, skipped, failed };
}

// ---- handlers ----------------------------------------------------------------

/**
 * `GET /sessions/:id/changes` — the rail's payload.
 *
 * Always 200, even with no change set: "not a repository" and "you changed
 * nothing" are both ordinary answers about a healthy session, and the difference
 * between them is `available` + `reason` rather than a status code (spec §13).
 */
export const getChangesH: Handler = async (_req, ctx, params) => {
  requireSession(ctx, params.id);
  return json(await sessionChanges(ctx.db, params.id));
};

/**
 * `POST /sessions/:id/changes/revert` — restore tracked paths from the base sha and
 * delete the ones the session created. No body (or a body without `paths`) reverts
 * everything the rail is showing; an explicit `{paths: []}` is refused rather than
 * treated as a wildcard (see `revertChanges`).
 *
 * REST exists so a revert does not cost a turn: this is the human's verb, and
 * asking the agent to undo its own work would be an LLM round-trip to run
 * `git checkout`.
 */
export const revertChangesH: Handler = async (req, ctx, params) => {
  requireSession(ctx, params.id);
  const body = await parseBody(req, RevertChangesBody, {});
  return json(await revertChanges(ctx.db, params.id, body.paths));
};
