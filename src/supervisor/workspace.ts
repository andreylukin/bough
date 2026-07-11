/**
 * Per-session workspace preparation, run once at the start of each turn. Two jobs:
 *
 *   1. Resolve the read-write root. Precedence: the session's persisted `workspace`
 *      column, then $BOUGH_WORKSPACE, then the process cwd. Only an *explicit*
 *      workspace (column or env) turns on the sandbox — a bare cwd fallback runs
 *      unsandboxed, which keeps the server- and turn-level tests from touching jj
 *      or spawning sandbox-exec.
 *
 *   2. When the root is a git repo (or a session's jj workspace dir), lazily set up
 *      the session's jj state and resolve the dir the turn actually runs in — see
 *      prepareRepo for the two modes (external store vs. legacy colocated). The
 *      base is captured on the first turn and persisted so later turns are
 *      deterministic. jj failures (not installed, not a repo mid-op) are
 *      non-fatal: the turn still runs sandboxed, just without snapshot tracking.
 *
 * The clonefile snapshot dir (BOUGH_SNAPSHOT_BASE override, else ~/.bough/…) is
 * always created and handed back so bash can be granted write access to it.
 */
import type { Db } from "../db/db.ts";
import { join } from "node:path";
import { sessionDir as snapshotSessionDir, snapshotBase } from "../vcs/clonefile.ts";
import {
  addWorkspace,
  bookmarkFor,
  createSessionWorkspace,
  ensureWorkspace,
  forkSession,
  updateStale,
  workspaceDirFor,
} from "../vcs/jj.ts";

export interface PreparedWorkspace {
  /** The resolved read-write root (bash cwd + file-tool root). */
  cwd: string;
  /** The clonefile snapshot dir for this session (added to the Seatbelt allowWrite). */
  sessionDir: string;
  /**
   * Per-session scratchpad dir — a writable dir OUTSIDE the workspace and the snapshot
   * dir, for temp files/scripts/outputs. It lives under the OS temp root so the OS
   * reaps it, and being outside the repo means scratch files never get jj-snapshotted,
   * built by the live server, or turn up in `git diff main HEAD`. "" when not sandboxed.
   */
  scratchDir: string;
  /** Whether the turn should run sandboxed (an explicit workspace was configured). */
  sandboxed: boolean;
}

/**
 * Root holding per-session scratchpads. BOUGH_SCRATCH_BASE overrides (tests); else a
 * `bough-scratch` dir under the OS temp root ($TMPDIR, else /tmp) — already inside the
 * Seatbelt write allowlist and auto-reaped by the OS.
 */
function scratchBase(): string {
  const override = Deno.env.get("BOUGH_SCRATCH_BASE");
  if (override) return override;
  return join(Deno.env.get("TMPDIR") ?? "/tmp", "bough-scratch");
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}

/**
 * Normalize a user-supplied workspace path: expand a leading `~`, make it
 * absolute. Users type `~/repos/x` into the new-session form; nothing in the
 * OS expands that for us, and an unexpanded `~` produces a cwd that doesn't
 * exist — sandbox-exec then fails to spawn and every tool in the session dies.
 */
export function normalizeWorkspace(raw: string): string {
  let p = raw.trim();
  const home = Deno.env.get("HOME");
  if (home && (p === "~" || p.startsWith("~/"))) p = home + p.slice(1);
  if (!p.startsWith("/")) p = `${Deno.cwd()}/${p}`;
  return p;
}

/** Human-readable reason a workspace path is unusable, or null if it's fine. */
export async function workspaceProblem(p: string): Promise<string | null> {
  try {
    const st = await Deno.stat(p);
    return st.isDirectory ? null : `workspace is not a directory: ${p}`;
  } catch {
    return `workspace directory does not exist: ${p}`;
  }
}

async function gitHead(repo: string): Promise<string | null> {
  const r = await new Deno.Command("git", {
    args: ["rev-parse", "HEAD"],
    cwd: repo,
    stdout: "piped",
    stderr: "null",
  }).output();
  return r.code === 0 ? new TextDecoder().decode(r.stdout).trim() : null;
}

/** Resolve + set up the workspace for `sessionId`'s turn. `override` wins for tests. */
export async function prepareWorkspace(
  db: Db,
  sessionId: string,
  override?: string,
): Promise<PreparedWorkspace> {
  const runtime = db.getSessionRuntime(sessionId);
  const rawExplicit = runtime.workspace ?? Deno.env.get("BOUGH_WORKSPACE") ?? undefined;
  // Normalize even though createSession validates: legacy rows and env values
  // predate validation and may still carry a literal `~`.
  const explicit = rawExplicit === undefined ? undefined : normalizeWorkspace(rawExplicit);
  let cwd = override ?? explicit ?? Deno.cwd();
  // BOUGH_NO_SANDBOX=1 is a debugging escape hatch: run the turn against the real
  // workspace with no jj tracking and no Seatbelt wrap.
  const noSandbox = Deno.env.get("BOUGH_NO_SANDBOX") === "1";
  const sandboxed = override === undefined && explicit !== undefined && !noSandbox;

  if (sandboxed) {
    // Fail the turn once with one readable message rather than letting every
    // tool die separately on a cwd that doesn't exist.
    const problem = await workspaceProblem(cwd);
    if (problem) throw new Error(problem);
  }

  const base = Deno.env.get("BOUGH_SNAPSHOT_BASE") ?? snapshotBase();
  const dir = snapshotSessionDir(sessionId, base);

  let scratchDir = "";
  if (sandboxed) {
    await Deno.mkdir(dir, { recursive: true });
    scratchDir = join(scratchBase(), sessionId);
    await Deno.mkdir(scratchDir, { recursive: true });
    const isGit = await pathExists(`${cwd}/.git`);
    const isJj = await pathExists(`${cwd}/.jj`);
    if (isGit || isJj) {
      // May relocate the turn into the session's own working copy (external mode).
      cwd = await prepareRepo(db, sessionId, cwd, runtime.base, isGit && isJj);
    }
  }

  return { cwd, sessionDir: dir, scratchDir, sandboxed };
}

/**
 * Set up the session's jj state for a repo workspace and return the dir the turn
 * should run in. Two modes, decided by what `repo` already carries:
 *
 *   - Colocated (`.git` + `.jj` — a checkout the user deliberately runs jj in,
 *     e.g. bough's own dev repo): legacy in-place behavior. The session's change
 *     lives in the primary checkout and the turn runs there.
 *   - External (plain `.git`, or a session's `.jj`-only workspace dir): jj stays
 *     out of the repo. The first turn branches an isolated working copy under
 *     ~/.bough/workspaces/<id> — off the parent session's tip for forks, else off
 *     a captured snapshot of the repo's working tree — and repoints the session's
 *     workspace column at it (the same repoint subagent spawns do). The user's
 *     checkout is never modified.
 *
 * jj failures are non-fatal: the turn still runs sandboxed on the original cwd,
 * just without snapshot tracking.
 */
async function prepareRepo(
  db: Db,
  sessionId: string,
  repo: string,
  persistedBase: string | null,
  colocated: boolean,
): Promise<string> {
  const session = db.getSession(sessionId);
  const firstTurn = persistedBase === null;
  try {
    if (colocated) {
      if (firstTurn && session?.kind === "fork" && session.parentId) {
        // Fork sessions branch off the parent session's tip, once. Falls back to a
        // plain workspace if the parent never took a turn (no bookmark to fork from).
        try {
          await forkSession(repo, session.parentId, sessionId);
        } catch {
          await ensureWorkspace(repo, sessionId, bookmarkFor(session.parentId));
        }
      } else {
        // Never pass a stored git-HEAD as the jj base: `jj new <HEAD>` would reset
        // the working copy to the committed tree and wipe uncommitted work. Let
        // ensureWorkspace default to the current working-copy snapshot (`@`); the
        // bookmark-exists check already handles deterministic resume.
        await ensureWorkspace(repo, sessionId);
      }
      if (firstTurn) {
        // Record the git point the session started from (metadata + the "not first
        // turn" sentinel). It is deliberately NOT fed back to `jj new`.
        const head = await gitHead(repo);
        if (head) db.setSessionBase(sessionId, head);
      }
      return repo;
    }

    if (!firstTurn) {
      // External-mode resume: the workspace column already points at the session's
      // own working copy. Repair staleness (a sibling workspace's op can rewrite
      // our working-copy commit) and run where we are.
      await updateStale(repo);
      return repo;
    }

    let dir: string;
    if (session?.kind === "fork" && session.parentId && await pathExists(`${repo}/.jj`)) {
      // `repo` is the parent session's workspace dir (forks inherit the workspace
      // column); branch the fork's own working copy off the parent's tip. If the
      // parent bookmark vanished, fall back to the dir's current working copy.
      dir = workspaceDirFor(sessionId);
      try {
        await addWorkspace(repo, sessionId, dir, bookmarkFor(session.parentId));
      } catch {
        await addWorkspace(repo, sessionId, dir, "@");
      }
    } else {
      // Root session on a plain git repo (also forks whose parent never took a
      // turn — nothing to inherit): isolated working copy off a captured snapshot.
      dir = await createSessionWorkspace(repo, sessionId);
    }
    db.setSessionWorkspace(sessionId, dir);
    // Metadata + the "not first turn" sentinel; "jj" when the origin has no
    // resolvable git HEAD (e.g. a fork branched from a parent's workspace dir).
    db.setSessionBase(sessionId, (await gitHead(repo)) ?? "jj");
    return dir;
  } catch (e) {
    console.error(`jj workspace prep skipped for ${sessionId}: ${(e as Error).message}`);
    return repo;
  }
}
