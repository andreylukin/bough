/**
 * The Changes rail's git layer: what a session changed in the user's checkout.
 *
 * THE INVARIANT THIS HOLDS: **the working tree IS the tip, so the only thing worth
 * recording is where the session started.** There is no snapshot substrate under
 * this and nothing to materialize. The agent's programs edit the real checkout
 * (spec §2, §3), so a session's change set is exactly `git diff <base>` plus
 * whatever is untracked, and delivery is the reviewer's own `git commit`. Ported
 * from `src/vcs/repodiff.ts`, which learned this the expensive way: the substrates
 * it replaced (shadow worktrees, then a copy-on-write clone) existed to keep the
 * agent's edits out of the user's tree and carry them back later, which bought
 * isolation git already provides and paid for it by breaking git.
 *
 * Three consequences, each load-bearing:
 *
 *   - **A base is a real sha, always.** A repo with no commits records the git
 *     empty-tree object (`EMPTY_TREE`) rather than a sentinel, because `git diff`
 *     and `git cat-file` both accept it — so the diff path and the revert path have
 *     exactly one shape instead of a special case each. The old tree stored the
 *     string "empty" and then had to map it back to null, which made a commitless
 *     repo report no change set at all.
 *   - **Not-a-repo is an answer, not an error.** A workspace outside git degrades to
 *     an unavailable `ChangeSet` carrying the reason (spec §13: the rail says so
 *     plainly rather than showing an empty diff). Nothing here throws for it, and
 *     the agent keeps working there — it simply produces no reviewable change set.
 *   - **Revert is the only mutation.** It restores tracked paths from the base sha
 *     and deletes the ones the session created, per path. This module does not
 *     decide WHICH paths are the session's — `server/changes.ts` intersects the
 *     request with the live change set before calling — because "never touches
 *     anything the session did not change" (spec §13) must be enforced, not merely
 *     true by construction of the caller.
 *
 * Pure-ish and server-free: nothing here imports from `server/`, so the whole
 * module is exercised against a real temp repo with no socket and no ctx.
 */
import { dirname, join } from "node:path";
import type { Db } from "../types.ts";

/**
 * The git empty-tree object id — the base recorded for a repository with no
 * commits yet. git resolves it without it existing in the object database, so
 * `git diff <EMPTY_TREE>` reports the whole index as additions and
 * `git cat-file -e <EMPTY_TREE>:<path>` correctly says "not in the base".
 */
export const EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

// ---- the structured diff ----------------------------------------------------
//
// Declared here rather than in `schema/` because `schema/` is frozen (plan §4) and
// carries no changes contract. These are wire shapes: `server/changes.ts` serializes
// them verbatim.

/** Coarse git status. A rename surfaces as delete + add — git's default without `-M`. */
export type FileStatus = "added" | "modified" | "deleted";

/**
 * One `@@ … @@` block: the header verbatim plus the body lines with their leading
 * ` `/`+`/`-` markers intact, so a client colours them without re-parsing.
 */
export interface Hunk {
  header: string;
  lines: string[];
}

/** One changed file. Binary or unreadable content yields no hunks, not a failure. */
export interface FileDiff {
  /** Repo-relative, forward slashes — the same string `revert` takes back. */
  path: string;
  status: FileStatus;
  hunks: Hunk[];
}

/**
 * A session's review payload.
 *
 * `available: false` is a first-class answer with a stated `reason`, not an error
 * and not an empty diff (spec §13). The two are different facts — "this workspace
 * is not a repository" versus "you changed nothing" — and a rail that rendered
 * both as an empty list would be lying about one of them.
 */
export interface ChangeSet {
  available: boolean;
  /** Present exactly when `available` is false. One plain sentence for the human. */
  reason?: string;
  /** The sha the diff is measured from, or null when there is none. */
  base: string | null;
  files: FileDiff[];
}

/** An unavailable change set — the shape every "no diff here" path returns. */
function unavailable(reason: string, base: string | null = null): ChangeSet {
  return { available: false, reason, base, files: [] };
}

// ---- git --------------------------------------------------------------------

export interface GitResult {
  ok: boolean;
  out: string;
  err: string;
}

/**
 * Run git in `dir`. A missing git binary comes back as `ok: false` rather than
 * throwing: every caller here already has a "git could not answer" path, and the
 * Changes rail must degrade to "no change set" instead of 500-ing a session that
 * is otherwise working fine.
 */
export async function git(dir: string, args: string[]): Promise<GitResult> {
  try {
    const { code, stdout, stderr } = await new Deno.Command("git", {
      args: ["-C", dir, ...args],
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
    }).output();
    const dec = new TextDecoder();
    return { ok: code === 0, out: dec.decode(stdout), err: dec.decode(stderr) };
  } catch (e) {
    return { ok: false, out: "", err: e instanceof Error ? e.message : String(e) };
  }
}

/** Whether `dir` is inside a git work tree. */
export async function isRepo(dir: string): Promise<boolean> {
  const r = await git(dir, ["rev-parse", "--is-inside-work-tree"]);
  return r.ok && r.out.trim() === "true";
}

/** The checkout's current HEAD sha, or null (no commits yet, or not a repo). */
export async function headSha(dir: string): Promise<string | null> {
  const r = await git(dir, ["rev-parse", "--verify", "-q", "HEAD"]);
  const sha = r.out.trim();
  return r.ok && sha ? sha : null;
}

/**
 * The sha to record as a session's `base` for `dir`, or null when `dir` is not a
 * repository (spec §13: no repo ⇒ no base ⇒ no change set, and that is fine).
 *
 * A repository with no commits answers `EMPTY_TREE`, not null: the session started
 * from nothing, which is a real starting point and diffs correctly.
 */
export async function baseFor(dir: string): Promise<string | null> {
  if (!(await isRepo(dir))) return null;
  return (await headSha(dir)) ?? EMPTY_TREE;
}

/**
 * Record the sha a session starts from, best-effort.
 *
 * Best-effort is the whole point: a broken git install, a workspace that vanished
 * between validation and here, or a repository someone is mid-`rebase` in must cost
 * the user their Changes rail, never their session. A session with no base is a
 * session the rail reports as unreviewable, which is exactly what spec §13 asks for.
 *
 * Returns what was stored, or null if nothing was.
 */
export async function recordBase(
  db: Pick<Db, "setSessionBase">,
  sessionId: string,
  dir: string,
): Promise<string | null> {
  try {
    const base = await baseFor(dir);
    if (base) db.setSessionBase(sessionId, base);
    return base;
  } catch {
    return null;
  }
}

// ---- parsing ----------------------------------------------------------------

/**
 * Parse a `git diff` into `FileDiff[]`. Pure and dependency-free — the heaviest
 * unit-tested surface in this module (plan §7).
 *
 * Ported from `src/schema/changes.ts`'s `parseGitDiff`, minus its `stripPrefix`
 * hook: that existed for `git diff --no-index` between two absolute snapshot roots,
 * and there is no second root any more. Names come from the b-side, which is
 * already repo-relative.
 */
export function parseGitDiff(text: string): FileDiff[] {
  const files: FileDiff[] = [];
  let cur: FileDiff | null = null;
  let hunk: Hunk | null = null;

  const flushHunk = () => {
    if (cur && hunk) cur.hunks.push(hunk);
    hunk = null;
  };
  const flushFile = () => {
    flushHunk();
    if (cur) files.push(cur);
    cur = null;
  };

  for (const line of text.split("\n")) {
    if (line.startsWith("diff --git ")) {
      flushFile();
      // "diff --git a/foo b/foo" — the b-side is the canonical path.
      const parts = line.split(" ");
      cur = {
        path: parts[parts.length - 1].replace(/^b\//, ""),
        status: "modified",
        hunks: [],
      };
      continue;
    }
    if (!cur) continue;

    if (line.startsWith("new file mode")) cur.status = "added";
    else if (line.startsWith("deleted file mode")) cur.status = "deleted";
    else if (line.startsWith("rename from") || line.startsWith("rename to")) {
      // A pure rename carries no content change and no hunks; leave it modified.
    } else if (line.startsWith("--- ") || line.startsWith("+++ ")) {
      // File headers — the b-side name is already captured.
    } else if (line.startsWith("@@")) {
      flushHunk();
      hunk = { header: line, lines: [] };
    } else if (hunk && (line.startsWith(" ") || line.startsWith("+") || line.startsWith("-"))) {
      hunk.lines.push(line);
    } else if (hunk && line === "\\ No newline at end of file") {
      hunk.lines.push(line);
    }
  }
  flushFile();
  return files;
}

// ---- the change set ---------------------------------------------------------

/**
 * The largest untracked file whose body is worth inlining as an added hunk.
 *
 * A change set is a REVIEW, and nobody reviews a 4 MiB blob by scrolling it. The
 * entry still appears — you must be able to see that the file is new — it just
 * carries no body.
 */
const MAX_ADDED_BYTES = 512 * 1024;

/**
 * Paths git neither tracks nor ignores, relative to the repo root.
 *
 * `--directory` is what makes this survive a real checkout. Without it, one
 * untracked directory — build output, a venv, a data dir, anything not in
 * `.gitignore` — enumerates every file beneath it: this repo's own `bench/` turned
 * the Changes rail into "50908 files changed" over a six-entry screen, and the
 * post-turn refresh then read all 50,899 bodies off disk. `git status` collapses
 * such a directory to a single `bench/` line, and the review surface should say
 * exactly what git says.
 */
async function untracked(dir: string): Promise<string[]> {
  const r = await git(dir, [
    "ls-files",
    "--others",
    "--exclude-standard",
    "--directory",
    "--no-empty-directory",
  ]);
  if (!r.ok) return [];
  return r.out.split("\n").map((l) => l.trim()).filter(Boolean);
}

/** An untracked file as an all-added FileDiff. Binary, huge or unreadable ⇒ no hunks. */
async function addedFile(dir: string, path: string): Promise<FileDiff> {
  // `--directory` yields collapsed directories with a trailing slash. There is no
  // body to read and no point stat-ing it: it is one entry meaning "all of this is
  // new", which is the same thing `git status` shows.
  if (path.endsWith("/")) return { path, status: "added", hunks: [] };

  let lines: string[] = [];
  try {
    const info = await Deno.stat(join(dir, path));
    if (info.size <= MAX_ADDED_BYTES) {
      const body = (await Deno.readTextFile(join(dir, path))).split("\n");
      // A trailing "" from a final newline is not a line; a file without a final
      // newline keeps its last one.
      if (body.at(-1) === "") body.pop();
      lines = body.map((l) => `+${l}`);
    }
  } catch {
    lines = []; // binary, unreadable, or deleted mid-review
  }
  return {
    path,
    status: "added",
    hunks: lines.length ? [{ header: `@@ -0,0 +1,${lines.length} @@`, lines }] : [],
  };
}

/**
 * What changed in `dir` since `base`: tracked edits from `git diff` plus untracked
 * files as additions.
 *
 * Untracked files are appended rather than obtained with `--no-index` passes
 * because `git diff <base>` already covers everything git knows about, staged or
 * not — so the two lists are disjoint by construction and nothing is counted twice.
 */
export async function changeSet(dir: string, base: string | null): Promise<ChangeSet> {
  if (!(await isRepo(dir))) {
    return unavailable(
      `${dir} is not a git repository — bough reviews changes with git, so this ` +
        `session has no change set and revert is unavailable. The agent still works ` +
        `here; its edits are simply not reviewable in the Changes rail (spec §13).`,
    );
  }
  if (!base) {
    return unavailable(
      `no starting commit was recorded for this session in ${dir}, so there is ` +
        `nothing to diff against. Sessions record one when they are created; a ` +
        `session that predates that — or whose workspace was not a repository then ` +
        `— has no change set.`,
    );
  }

  const r = await git(dir, ["diff", "--no-color", "--no-ext-diff", base]);
  if (!r.ok) {
    return unavailable(
      `git diff ${base} failed in ${dir}: ${r.err.trim() || "git reported no reason"}. ` +
        `The commit the session started from may have been dropped by a rebase or ` +
        `a prune, which leaves nothing to measure this session's work against.`,
      base,
    );
  }

  const files = parseGitDiff(r.out);
  for (const path of await untracked(dir)) files.push(await addedFile(dir, path));
  return { available: true, base, files };
}

// ---- revert -----------------------------------------------------------------

/** What a revert actually did. `failed` carries git's own reason, per path. */
export interface RevertResult {
  reverted: string[];
  failed: { path: string; error: string }[];
}

/**
 * Undo the session's work on `paths`: tracked files are restored to their content
 * at `base`, files the session created are deleted.
 *
 * PER PATH, and per path in both directions — one path that cannot be restored
 * fails alone and the rest still revert. A reviewer un-picking one file out of
 * twelve must not lose the other eleven to a single permission error.
 *
 * The caller is responsible for passing only paths the session actually changed
 * (`server/changes.ts` intersects with the live change set first). `git checkout
 * <base> -- <path>` would happily rewrite a file this session never touched.
 */
export async function revertPaths(
  dir: string,
  base: string,
  paths: string[],
): Promise<RevertResult> {
  const result: RevertResult = { reverted: [], failed: [] };
  for (const path of paths) {
    // Present in the base commit ⇒ restore that content. `git checkout <sha> --
    // <path>` also stages it, which is what a reviewer means by "put it back".
    const known = await git(dir, ["cat-file", "-e", `${base}:${path}`]);
    if (known.ok) {
      const r = await git(dir, ["checkout", base, "--", path]);
      if (r.ok) result.reverted.push(path);
      else result.failed.push({ path, error: r.err.trim() || "git checkout failed" });
      continue;
    }
    // Absent from the base commit ⇒ the session created it. Delete it, then prune
    // the directories that existed only to hold it.
    try {
      await Deno.remove(join(dir, path));
      result.reverted.push(path);
      pruneEmptyParents(dir, path);
    } catch (e) {
      // Already gone is a success: the reviewer asked for it not to be there.
      if (e instanceof Deno.errors.NotFound) {
        result.reverted.push(path);
        continue;
      }
      result.failed.push({ path, error: e instanceof Error ? e.message : String(e) });
    }
  }
  return result;
}

/**
 * Remove the now-empty directories a deleted file left behind, stopping at the
 * first one that still holds something. Synchronous and swallowing: this is
 * tidiness, and a failure to tidy must never turn a successful revert into a
 * reported failure.
 */
function pruneEmptyParents(dir: string, path: string): void {
  let parent = dirname(path);
  while (parent && parent !== "." && parent !== "/") {
    try {
      Deno.removeSync(join(dir, parent)); // throws once non-empty — that is the stop
    } catch {
      return;
    }
    parent = dirname(parent);
  }
}
