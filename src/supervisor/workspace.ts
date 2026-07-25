/**
 * Per-session workspace preparation, run once at the start of each turn. Two jobs:
 *
 *   1. Resolve the read-write root. Precedence: the session's persisted `workspace`
 *      column, then $BOUGH_WORKSPACE, then the process cwd.
 *
 *   2. When that root is a repo, record the session's starting HEAD once (the
 *      `base` column) so the Changes rail can report what this session changed —
 *      `git diff <base>`, see vcs/repodiff.ts.
 *
 * The turn runs IN the resolved root: the user's own checkout, on the user's own
 * branch. There is no per-session worktree and no copy-on-write overlay anymore —
 * both existed to keep the agent's edits out of the tree and then carry them back
 * in, which broke git for the agent (see shellInvocation in tools/bash.ts). The
 * isolation that remains is git's own: the agent commits, and the user reviews,
 * resets, or pushes like they would any other work.
 *
 * The clonefile snapshot dir (BOUGH_SNAPSHOT_BASE override, else ~/.bough/…) is
 * still created for the non-git config-edit path.
 */
import type { Db } from "../db/db.ts";
import { join } from "node:path";
import { sessionDir as snapshotSessionDir, snapshotBase } from "../vcs/clonefile.ts";
import { headSha, isRepo } from "../vcs/repodiff.ts";

export interface PreparedWorkspace {
  /** The resolved read-write root (bash cwd + file-tool root). */
  cwd: string;
  /**
   * Host-side read root for compose-time consumers (@ refs, AGENTS.md, LSP/MCP
   * cwd): the session's host checkout, same as `cwd`.
   */
  hostView: string;
  /** The clonefile snapshot dir for this session (bash gets write access to it). */
  sessionDir: string;
  /**
   * Per-session scratchpad dir — a writable dir OUTSIDE the workspace and the snapshot
   * dir, for temp files/scripts/outputs. It lives under the OS temp root so the OS
   * reaps it, and being outside the repo means scratch files never get snapshotted,
   * built by the live server, or turn up in `git diff main HEAD`. "" when not sandboxed.
   */
  scratchDir: string;
  /**
   * True when this is a real configured session (an explicit workspace, no test
   * override, no BOUGH_NO_SANDBOX escape hatch). The name is historical — nothing
   * is sandboxed any more — and it now gates the per-session setup that only a
   * configured session should get: the snapshot + scratch dirs, the base-sha
   * capture, and the compose-time reads rooted at the workspace.
   */
  sandboxed: boolean;
  /**
   * Set when the session's base sha could not be recorded on its first turn: the
   * turn still runs, but the Changes rail has nothing to diff against and will
   * show the work as if it predates the session. The caller surfaces it in the
   * thread — a silent degradation is invisible outside the server log.
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
 * Host-side read root for a session's files. The session works in its workspace
 * directly, so this is just the workspace — kept as a named seam because the
 * compose-time callers (@ image attachments, the file picker) all go through it.
 */
export function hostReadRoot(_sessionId: string, workspace: string): string {
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
  let warning: string | undefined;
  if (sandboxed) {
    await Deno.mkdir(dir, { recursive: true });
    scratchDir = join(scratchBase(), sessionId);
    await Deno.mkdir(scratchDir, { recursive: true });
    // First turn in a repo: pin the base sha the Changes rail diffs against. The
    // turn itself runs right here, in the user's checkout. Best-effort — a broken
    // git install must not cost the user their turn, only their diff.
    if (runtime.base === null) {
      try {
        if (await isRepo(cwd)) db.setSessionBase(sessionId, (await headSha(cwd)) ?? "empty");
      } catch (e) {
        warning = `Could not record this session's starting commit in ${cwd} ` +
          `(${(e as Error).message}) — the Changes rail may not show what changed.`;
      }
    }
  }

  return { cwd, hostView: cwd, sessionDir: dir, scratchDir, sandboxed, ...(warning ? { warning } : {}) };
}

