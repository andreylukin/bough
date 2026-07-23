/**
 * Per-session workspace preparation, run once at the start of each turn. Two jobs:
 *
 *   1. Resolve the read-write root. Precedence: the session's persisted `workspace`
 *      column, then $BOUGH_WORKSPACE, then the process cwd. Only an *explicit*
 *      workspace (column or env) turns on the sandbox — a bare cwd fallback runs
 *      unsandboxed, which keeps the server- and turn-level tests from touching
 *      snapshot state or booting a sandbox VM.
 *
 *   2. When the root is a repo dir, lazily set up the session's shadow worktree
 *      (docs/shadow-snapshots.md) and resolve the dir the turn actually runs in —
 *      see prepareShadow. The base is captured on the first turn and persisted so
 *      later turns are deterministic. Snapshot failures are non-fatal: the turn
 *      still runs sandboxed, just without snapshot tracking.
 *
 * The clonefile snapshot dir (BOUGH_SNAPSHOT_BASE override, else ~/.bough/…) is
 * always created and handed back so bash can be granted write access to it.
 */
import type { Db } from "../db/db.ts";
import { join } from "node:path";
import { sessionDir as snapshotSessionDir, snapshotBase } from "../vcs/clonefile.ts";
import * as shadow from "../vcs/shadow.ts";
import { guestTrack } from "../vcs/guestgit.ts";
import { refreshMirror } from "../vcs/mirror.ts";
import { attachVm, sandboxVm } from "../sandbox/vmsession.ts";
import { pathExists } from "../fsutil.ts";

export interface PreparedWorkspace {
  /** The resolved read-write root (bash cwd + file-tool root). */
  cwd: string;
  /**
   * Host-side read root for compose-time consumers (@ refs, AGENTS.md, LSP/MCP
   * cwd): the read-only mirror checkout in guest-owned mode (fresh as of the
   * last guest push), else `cwd`.
   */
  hostView: string;
  /**
   * Guest-owned (VM) session: the working copy is the guest clone at
   * /workspace/repo; `cwd` stays the ORIGIN path and file tools route through
   * the VM (ToolRunCtx.guestFs).
   */
  guestOwned?: boolean;
  /** The clonefile snapshot dir for this session (bash gets write access to it). */
  sessionDir: string;
  /**
   * Per-session scratchpad dir — a writable dir OUTSIDE the workspace and the snapshot
   * dir, for temp files/scripts/outputs. It lives under the OS temp root so the OS
   * reaps it, and being outside the repo means scratch files never get snapshotted,
   * built by the live server, or turn up in `git diff main HEAD`. "" when not sandboxed.
   */
  scratchDir: string;
  /** Whether the turn should run sandboxed (an explicit workspace was configured). */
  sandboxed: boolean;
  /**
   * Set when workspace isolation was expected but could not be provided (shadow
   * prep failed on a first turn): the turn runs directly in the user's checkout.
   * The caller surfaces it in the thread — a silent fallback let sessions pollute
   * the real repo with nothing visible outside the server log (user-testing bug).
   */
  warning?: string;
}

/**
 * Root holding per-session scratchpads. BOUGH_SCRATCH_BASE overrides (tests); else a
 * `bough-scratch` dir under the OS temp root ($TMPDIR, else /tmp), auto-reaped by
 * the OS.
 */
function scratchBase(): string {
  const override = Deno.env.get("BOUGH_SCRATCH_BASE");
  if (override) return override;
  return join(Deno.env.get("TMPDIR") ?? "/tmp", "bough-scratch");
}

/**
 * Normalize a user-supplied workspace path: expand a leading `~`, make it
 * absolute. Users type `~/repos/x` into the new-session form; nothing in the
 * OS expands that for us, and an unexpanded `~` produces a cwd that doesn't
 * exist — workspace prep then fails and every tool in the session dies.
 */
export function normalizeWorkspace(raw: string): string {
  let p = raw.trim();
  const home = Deno.env.get("HOME");
  if (home && (p === "~" || p.startsWith("~/"))) p = home + p.slice(1);
  if (!p.startsWith("/")) p = `${Deno.cwd()}/${p}`;
  return p;
}

/**
 * Host-side read root for a session's files: the mirror checkout under
 * `workspacesRoot()` when one exists — the guest-owned mirror, or a legacy
 * host worktree at the same path — else the given workspace. Sync so the
 * compose-time callers (@ image attachments, the file picker) stay sync.
 */
export function hostReadRoot(sessionId: string, workspace: string): string {
  const mirror = shadow.workspaceDirFor(sessionId);
  try {
    if (Deno.statSync(mirror).isDirectory) return mirror;
  } catch {
    // no mirror — non-repo or unsandboxed session; read the workspace itself
  }
  return workspace;
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
  const cwd = override ?? explicit ?? Deno.cwd();
  // BOUGH_NO_SANDBOX=1 is a debugging escape hatch: run the turn against the real
  // workspace with no snapshot isolation and no sandbox.
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
    const isJj = await pathExists(`${cwd}/.jj`); // legacy jj-era workspace dirs still count as repos
    if (isGit || isJj) {
      // VM mode: refs + mirror only (the working copy is the guest clone).
      // Host-worktree mode: may relocate the turn into the session's own
      // worktree (first turn).
      const prepped = await prepareShadow(db, sessionId, cwd, runtime.base === null, sandboxVm());
      const guestOwned = prepped.guestOwned === true;
      return {
        cwd: prepped.dir,
        hostView: guestOwned ? shadow.workspaceDirFor(sessionId) : prepped.dir,
        ...(guestOwned ? { guestOwned } : {}),
        sessionDir: dir,
        scratchDir,
        sandboxed,
        warning: prepped.warning,
      };
    }
  }

  return { cwd, hostView: cwd, sessionDir: dir, scratchDir, sandboxed };
}

/**
 * Root/refs-only session creation with the broken-store quarantine retry: a
 * store that predates this attempt and now errors with a corruption signature
 * is broken derived state — quarantine (never delete) and retry once fresh.
 */
async function createWithQuarantine(
  repo: string,
  sessionId: string,
  opts: { worktree?: boolean },
): Promise<string> {
  const hadStore = await pathExists(await shadow.storeDirFor(repo));
  try {
    return await shadow.createSessionWorkspace(repo, sessionId, opts);
  } catch (e) {
    if (!hadStore || !shadow.looksLikeBrokenStore(e as Error)) throw e;
    const moved = await shadow.quarantineStore(repo);
    if (!moved) throw e;
    console.error(
      `shadow store for ${repo} quarantined to ${moved} (${
        (e as Error).message.split("\n")[0]
      }); retrying fresh`,
    );
    return await shadow.createSessionWorkspace(repo, sessionId, opts);
  }
}

/**
 * Shadow-backend session prep — the single external-style path (there is no
 * colocated mode: every repo session gets an isolated working copy, even on
 * repos that still carry a legacy `.jj`).
 *
 * Guest-owned (VM) mode: the working copy is the guest clone at /workspace/repo
 * (vmsession bootstrapClone) — the first turn only sets the session's store
 * refs (no host worktree, no hydration) and seeds the read-only mirror; the
 * workspace column permanently keeps the ORIGIN path, and `cwd` stays the
 * origin (bash cwd is resolved guest-side).
 *
 * Host-worktree mode (no VM backend): first turn branches a worktree — off the
 * parent session's tip for forks, else off a captured snapshot of the repo's
 * working tree — and repoints the session's workspace column at it. Resumes
 * run where the column already points.
 *
 * Failures degrade gracefully either way: sandboxed turn in the user's
 * checkout, with a loud warning.
 */
async function prepareShadow(
  db: Db,
  sessionId: string,
  repo: string,
  firstTurn: boolean,
  vmMode: boolean,
): Promise<{ dir: string; warning?: string; guestOwned?: boolean }> {
  const session = db.getSession(sessionId);
  try {
    if (!firstTurn) return { dir: repo, ...(vmMode ? { guestOwned: true } : {}) };
    if (vmMode) {
      if (session?.kind === "fork" && session.originId) {
        // Branch off the forked-from session's pushed tip (guestTrack flushes
        // the parent VM first — attachVm so a machine surviving a server
        // restart is flushed too; best-effort: a failed flush still leaves the
        // last-pushed tip); fall back to a fresh root capture only if the
        // parent's refs vanished.
        try {
          if (await attachVm(session.originId, { origin: repo, gitOrigin: true })) {
            await guestTrack(session.originId);
          }
        } catch (e) {
          console.error(
            `fork ${sessionId}: parent flush failed, branching off the last push: ${
              (e as Error).message
            }`,
          );
        }
        try {
          await shadow.addWorkspace(
            await shadow.storeDirFor(repo),
            sessionId,
            shadow.workspaceDirFor(sessionId),
            session.originId,
            { worktree: false },
          );
        } catch {
          await createWithQuarantine(repo, sessionId, { worktree: false });
        }
      } else {
        await createWithQuarantine(repo, sessionId, { worktree: false });
      }
      // Initial mirror = the session base tree; the gateway refreshes it on
      // every received push. Non-fatal: the mirror only feeds read-side
      // consumers (@ refs, AGENTS.md, LSP) — the session itself is intact.
      try {
        await refreshMirror(sessionId);
      } catch (e) {
        console.error(`mirror seed failed for ${sessionId}: ${(e as Error).message}`);
      }
      // The workspace column keeps the ORIGIN path — only the base sentinel moves.
      db.setSessionBase(sessionId, (await gitHead(repo)) ?? "shadow");
      return { dir: repo, guestOwned: true };
    }
    let dir: string;
    if (
      session?.kind === "fork" && session.originId &&
      (await shadow.originRepo(repo)) !== null
    ) {
      // `repo` is the forked-from session's worktree (forks inherit the
      // workspace column, and originId names that session — forks are SIBLINGS,
      // so parentId is null when forking a root session); branch off its tip,
      // falling back to the worktree's HEAD if its refs vanished.
      dir = shadow.workspaceDirFor(sessionId);
      try {
        await shadow.addWorkspace(repo, sessionId, dir, session.originId);
      } catch {
        await shadow.addWorkspace(repo, sessionId, dir, null);
      }
    } else {
      // Root session (also forks whose parent never took a turn): isolated
      // worktree off a captured snapshot.
      dir = await createWithQuarantine(repo, sessionId, {});
    }
    db.setSessionWorkspace(sessionId, dir);
    // Metadata + the "not first turn" sentinel; "shadow" when the origin has no
    // resolvable git HEAD (non-git dirs, forks from a parent's worktree).
    db.setSessionBase(sessionId, (await gitHead(repo)) ?? "shadow");
    return { dir };
  } catch (e) {
    console.error(`shadow workspace prep skipped for ${sessionId}: ${(e as Error).message}`);
    const warning = firstTurn
      ? `⚠ workspace isolation failed — this session edits ${repo} DIRECTLY, and the ` +
        `changes review (^d) can't track it. git said: ${(e as Error).message.split("\n")[0]}`
      : undefined;
    return { dir: repo, warning };
  }
}
