/**
 * jj (Jujutsu) integration — repo snapshots and per-session branching. This is
 * both the snapshot/review backend for repo work and the FS half of the tree
 * "branching" pillar: every session is a jj bookmark on its own change, forking a
 * session forks the bookmark, and every jj command auto-snapshots the working
 * copy so nothing is ever lost (the op log is the undo history).
 *
 * Model:
 *   - We run jj colocated with git (`jj git init --colocate`), so the git repo and
 *     its history are untouched and any tool that reads git keeps working.
 *   - A session's edits land on one jj change bookmarked `bough/<sessionId>`. jj
 *     amends that change on every snapshot, and the bookmark follows it — so the
 *     bookmark always points at the session's current tip.
 *   - A new session branches off the repo's git HEAD (or a caller-supplied base),
 *     giving it an isolated change. `forkSession` branches a new change off the
 *     source session's tip, so the fork inherits the source's work then diverges.
 *   - `diff(session)` is the change-vs-parent diff (`jj diff --git -r <bookmark>`),
 *     i.e. exactly what that session changed since it branched.
 *
 * Shelling out: every call uses `--no-pager` and `--color=never` for stable,
 * parseable output, and pins `user.name`/`user.email` via `--config` so jj works
 * with no global config. macOS-first but nothing here is macOS-specific.
 */
import { parseGitDiff } from "../schema/changes.ts";
import type { Diff } from "../schema/changes.ts";

/** Identity stamped on jj changes when the repo/user has none configured. */
const JJ_USER = "bough";
const JJ_EMAIL = "bough@localhost";

const BOOKMARK_PREFIX = "bough/";

/** Bookmark name for a session's change. */
export function bookmarkFor(sessionId: string): string {
  return BOOKMARK_PREFIX + sessionId;
}

interface RunResult {
  ok: boolean;
  code: number;
  stdout: string;
  stderr: string;
}

async function run(bin: string, args: string[], cwd: string): Promise<RunResult> {
  const cmd = new Deno.Command(bin, {
    args,
    cwd,
    stdout: "piped",
    stderr: "piped",
  });
  const { code, stdout, stderr } = await cmd.output();
  return {
    ok: code === 0,
    code,
    stdout: new TextDecoder().decode(stdout),
    stderr: new TextDecoder().decode(stderr),
  };
}

/** Run a jj subcommand in `repo`; throws on non-zero exit with stderr attached. */
async function jj(repo: string, args: string[]): Promise<string> {
  const base = [
    "--no-pager",
    "--color=never",
    "--config",
    `user.name=${JJ_USER}`,
    "--config",
    `user.email=${JJ_EMAIL}`,
  ];
  const r = await run("jj", [...base, ...args], repo);
  if (!r.ok) {
    throw new Error(`jj ${args.join(" ")} failed (${r.code}): ${r.stderr.trim()}`);
  }
  return r.stdout;
}

/** `jj --version` (or throws if jj isn't installed). Callers use this to gate on install. */
export async function version(): Promise<string> {
  const r = await run("jj", ["--version"], Deno.cwd());
  if (!r.ok) throw new Error("jj not installed (run `brew install jj`)");
  return r.stdout.trim();
}

async function isColocated(repo: string): Promise<boolean> {
  try {
    const info = await Deno.stat(`${repo}/.jj`);
    return info.isDirectory;
  } catch {
    return false;
  }
}

/** Initialise jj colocated with the existing git repo, once. No-op if already done. */
export async function ensureRepo(repo: string): Promise<void> {
  if (await isColocated(repo)) return;
  await jj(repo, ["git", "init", "--colocate"]);
}

/** True if a bookmark with this exact name exists. */
async function bookmarkExists(repo: string, name: string): Promise<boolean> {
  const out = await jj(repo, ["bookmark", "list", "-T", 'name ++ "\\n"']);
  return out.split("\n").some((n) => n.trim() === name);
}

/**
 * True if `rev` resolves to a commit. A bookmark's NAME can linger in some listings
 * while no longer resolving as a revision — e.g. its change was abandoned (jj prunes
 * empty leaf changes and drops the bookmark) or a colocated git import left it stale.
 * We check resolvability directly instead of trusting the name list, so a diff of a
 * vanished session degrades to "no changes" rather than throwing "revision doesn't exist".
 */
async function revResolves(repo: string, rev: string): Promise<boolean> {
  const r = await run("jj", [
    "--no-pager",
    "--color=never",
    "log",
    "--no-graph",
    "-r",
    rev,
    "-T",
    "commit_id.short()",
  ], repo);
  return r.ok && r.stdout.trim().length > 0;
}

/**
 * Ensure a session's jj workspace exists and the working copy is editing it.
 * Idempotent: resuming an existing session switches to its change; a new session
 * branches a fresh change off `base`. Returns the bookmark name.
 *
 * `base` DEFAULTS TO `@` — the current working-copy snapshot — NOT git HEAD. This
 * is load-bearing: `jj new @` keeps every uncommitted and untracked file on disk
 * (they live in the parent snapshot) and starts the session's change empty, so its
 * diff shows only what the agent does. Branching off git HEAD instead would reset
 * the working copy to the committed tree and silently delete the user's in-progress
 * work — the one thing a coding agent must never do to a repo it's pointed at.
 */
export async function ensureWorkspace(
  repo: string,
  sessionId: string,
  base?: string,
): Promise<string> {
  await ensureRepo(repo);
  const name = bookmarkFor(sessionId);
  if (await bookmarkExists(repo, name)) {
    await jj(repo, ["edit", name]);
    return name;
  }
  // ensureRepo snapshotted the working copy into `@`; branch off that, not HEAD.
  const from = base ?? "@";
  await jj(repo, ["new", from]);
  await jj(repo, ["bookmark", "create", name, "-r", "@"]);
  return name;
}

/**
 * Fork `fromSessionId` into `toSessionId`: a new change branched off the source
 * session's tip, bookmarked and checked out so subsequent edits diverge. The
 * fork inherits everything the source has so far. Returns the new bookmark name.
 */
export async function forkSession(
  repo: string,
  fromSessionId: string,
  toSessionId: string,
): Promise<string> {
  await ensureRepo(repo);
  const from = bookmarkFor(fromSessionId);
  const to = bookmarkFor(toSessionId);
  await jj(repo, ["new", from]);
  await jj(repo, ["bookmark", "create", to, "-r", "@"]);
  return to;
}

/** jj workspace name for a session's dedicated working copy (subagents). */
export function workspaceNameFor(sessionId: string): string {
  return "bough-" + sessionId;
}

/**
 * Give a session its own jj workspace — a second working copy of the same repo at
 * `dir`, branched off `baseBookmark`'s tip. This is the multi-working-copy analogue
 * of forkSession: the repo's default working copy stays untouched, so sessions run
 * in parallel without fighting over one checkout. The new workspace's working-copy
 * change starts as a child of the base (inheriting its work) and gets the session's
 * bookmark. Idempotent: an existing workspace dir is reused as-is.
 */
export async function addWorkspace(
  repo: string,
  sessionId: string,
  dir: string,
  baseBookmark: string,
): Promise<string> {
  await ensureRepo(repo);
  if (await isColocated(dir)) return dir; // already added (dir has .jj)
  await jj(repo, ["workspace", "add", "--name", workspaceNameFor(sessionId), "-r", baseBookmark, dir]);
  await jj(dir, ["bookmark", "create", bookmarkFor(sessionId), "-r", "@"]);
  return dir;
}

/**
 * Refresh a workspace whose working copy went stale (its change was rebased or
 * amended from another workspace — e.g. adoptChanges rewrote the spawner's change
 * under a subagent). Safe to call when not stale; failures are swallowed because
 * staleness is the only condition this repairs.
 */
export async function updateStale(dir: string): Promise<void> {
  try {
    await jj(dir, ["workspace", "update-stale"]);
  } catch {
    // not a workspace / nothing stale — nothing to repair
  }
}

/**
 * Adopt a subagent session's work into its spawner: move the subagent change's diff
 * into the spawner's change (`jj squash --from --into`). Both working copies are
 * snapshotted first so on-disk edits are included; the squash runs in the spawner's
 * workspace (`repo`) so its checkout is updated in place rather than marked stale.
 * `--keep-emptied` keeps the subagent's emptied change and bookmark alive, so its
 * branch stays on the map and remains continuable.
 */
export async function adoptChanges(
  repo: string,
  subagentDir: string,
  fromSessionId: string,
  intoSessionId: string,
): Promise<void> {
  await snapshot(subagentDir);
  await snapshot(repo);
  await jj(repo, [
    "squash",
    "--from",
    bookmarkFor(fromSessionId),
    "--into",
    bookmarkFor(intoSessionId),
    "--keep-emptied",
    "--use-destination-message",
  ]);
  await updateStale(subagentDir);
}

/**
 * Force a working-copy snapshot. jj snapshots on any command, so a bare `jj st`
 * is the explicit "capture what's on disk now" touch (e.g. before reading a diff
 * that must reflect the latest edits). Returns the status text.
 */
export async function snapshot(repo: string): Promise<string> {
  return await jj(repo, ["st"]);
}

/**
 * The structured diff of a session's change vs. where it branched
 * (`jj diff --git -r <bookmark>`). Snapshots first so on-disk edits are included.
 * If the session's bookmark no longer resolves to a revision (abandoned/pruned change),
 * returns an empty diff instead of throwing.
 */
export async function diff(repo: string, sessionId: string): Promise<Diff> {
  await snapshot(repo);
  const bookmark = bookmarkFor(sessionId);
  // The session's change may have been abandoned/pruned (bookmark gone stale). Treat
  // an unresolvable revision as an empty diff — there is genuinely nothing to review —
  // rather than surfacing jj's "revision doesn't exist" as a failure.
  if (!(await revResolves(repo, bookmark))) {
    return { source: "jj", files: [] };
  }
  const out = await jj(repo, ["diff", "--git", "-r", bookmark]);
  return { source: "jj", files: parseGitDiff(out) };
}

/**
 * Accept a session's reviewed change: seal it as a finished commit and advance the
 * session bookmark onto a new empty child (`jj new <bookmark>` + `bookmark move`).
 * The accepted work stays on disk — it's the new working copy's parent — and the
 * session's change-vs-parent diff resets to empty, so the Changes rail clears.
 * Whole-change in v1; snapshots first so on-disk edits are folded in before sealing.
 */
export async function accept(repo: string, sessionId: string): Promise<void> {
  await snapshot(repo);
  const name = bookmarkFor(sessionId);
  await jj(repo, ["new", name]);
  await jj(repo, ["bookmark", "move", name, "--to", "@"]);
}

/** One entry in the operation log — the unit of undo/restore. */
export interface Operation {
  id: string;
  description: string;
}

/** The operation log (most recent first), for building an undo/restore UI. */
export async function operations(repo: string, limit = 20): Promise<Operation[]> {
  const out = await jj(repo, [
    "op",
    "log",
    "--no-graph",
    "--limit",
    String(limit),
    "-T",
    'id.short() ++ "\\t" ++ description ++ "\\n"',
  ]);
  const ops: Operation[] = [];
  for (const line of out.split("\n")) {
    if (!line.trim()) continue;
    const tab = line.indexOf("\t");
    if (tab < 0) continue;
    ops.push({ id: line.slice(0, tab), description: line.slice(tab + 1) });
  }
  return ops;
}

/** Undo the most recent operation (like `jj undo`). */
export async function undo(repo: string): Promise<void> {
  await jj(repo, ["undo"]);
}

/** Restore the repo to the state at a given operation id (`jj op restore`). */
export async function restore(repo: string, opId: string): Promise<void> {
  await jj(repo, ["op", "restore", opId]);
}
