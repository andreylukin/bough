/**
 * jj (Jujutsu) integration — repo snapshots and per-session branching. This is
 * both the snapshot/review backend for repo work and the FS half of the tree
 * "branching" pillar: every session is a jj bookmark on its own change, forking a
 * session forks the bookmark, and every jj command auto-snapshots the working
 * copy so nothing is ever lost (the op log is the undo history).
 *
 * Model (shared by both modes below):
 *   - A session's edits land on one jj change bookmarked `bough/<sessionId>`. jj
 *     amends that change on every snapshot, and the bookmark follows it — so the
 *     bookmark always points at the session's current tip.
 *   - `diff(session)` is the change-vs-parent diff (`jj diff --git -r <bookmark>`),
 *     i.e. exactly what that session changed since it branched.
 *
 * Two placements of jj state, decided per repo by the workspace supervisor:
 *   - External (the default for plain git repos): jj is kept OUT of the repo
 *     entirely. The jj store lives under `~/.bough/jj/<repo>-<hash>` backed by the
 *     repo's .git (`jj git init --git-repo`), and every session gets its own jj
 *     workspace (a second working copy) under `~/.bough/workspaces/<sessionId>`,
 *     branched off a captured snapshot of the repo's working tree. The user's
 *     checkout — HEAD, branch, index, tree, `git status` — is never modified; the
 *     only visible trace is a `bough/<sessionId>` git branch (kept fresh via
 *     `jj git export`) so session work stays reachable from plain git.
 *   - Colocated (legacy; only for repos that already have `.jj` alongside `.git`,
 *     e.g. a checkout the user deliberately runs jj in): sessions share the
 *     primary checkout, `jj new` moves it onto the session's change, and git HEAD
 *     rides along detached. Never initiated on new repos anymore.
 *
 * Shelling out: every call uses `--no-pager` and `--color=never` for stable,
 * parseable output, and pins `user.name`/`user.email` via `--config` so jj works
 * with no global config. macOS-first but nothing here is macOS-specific.
 */
import { dirname, resolve } from "node:path";
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

async function run(
  bin: string,
  args: string[],
  cwd: string,
  env?: Record<string, string>,
  stdin?: string,
): Promise<RunResult> {
  const cmd = new Deno.Command(bin, {
    args,
    cwd,
    env,
    stdin: stdin === undefined ? "null" : "piped",
    stdout: "piped",
    stderr: "piped",
  });
  const child = cmd.spawn();
  if (stdin !== undefined) {
    const w = child.stdin.getWriter();
    await w.write(new TextEncoder().encode(stdin));
    await w.close();
  }
  const { code, stdout, stderr } = await child.output();
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

/** Run a git subcommand in `repo`; throws on non-zero exit with stderr attached. */
async function git(
  repo: string,
  args: string[],
  env?: Record<string, string>,
  stdin?: string,
): Promise<string> {
  const r = await run("git", args, repo, env, stdin);
  if (!r.ok) {
    throw new Error(`git ${args.join(" ")} failed (${r.code}): ${r.stderr.trim()}`);
  }
  return r.stdout;
}

/** `jj --version` (or throws if jj isn't installed). Callers use this to gate on install. */
export async function version(): Promise<string> {
  const r = await run("jj", ["--version"], Deno.cwd());
  if (!r.ok) throw new Error("jj not installed (run `brew install jj`)");
  return r.stdout.trim();
}

/** True if `dir` is a jj repo or workspace (has a `.jj` dir). */
async function hasJjDir(dir: string): Promise<boolean> {
  try {
    const info = await Deno.stat(`${dir}/.jj`);
    return info.isDirectory;
  } catch {
    return false;
  }
}

/**
 * Initialise jj colocated with the existing git repo, once. No-op if already done.
 * Legacy: only `ensureWorkspace` (in-place sessions on already-colocated repos and
 * tests) still calls this — new repos get an external store via `ensureStore`.
 */
export async function ensureRepo(repo: string): Promise<void> {
  if (await hasJjDir(repo)) return;
  await jj(repo, ["git", "init", "--colocate"]);
}

/** Root for external jj stores: `$BOUGH_JJ_BASE` or `~/.bough/jj`. */
export function storeBase(): string {
  const env = Deno.env.get("BOUGH_JJ_BASE");
  if (env) return env;
  const home = Deno.env.get("HOME");
  if (!home) throw new Error("jj: no $HOME");
  return `${home}/.bough/jj`;
}

/**
 * Root for per-session jj workspaces (isolated working copies), shared by root
 * sessions and subagents: `$BOUGH_SUBAGENT_BASE` or `~/.bough/workspaces`.
 */
export function workspacesRoot(): string {
  const env = Deno.env.get("BOUGH_SUBAGENT_BASE");
  if (env) return env;
  const home = Deno.env.get("HOME");
  if (!home) throw new Error("jj: no $HOME");
  return `${home}/.bough/workspaces`;
}

/** A session's own working-copy dir under `workspacesRoot()`. */
export function workspaceDirFor(sessionId: string): string {
  return `${workspacesRoot()}/${sessionId}`;
}

/**
 * True when an error reads like STORE corruption (broken/partial state under
 * `storeBase()`), as opposed to a transient or environmental failure. Gates the
 * quarantine-and-retry below: a healthy store must never be moved aside — other
 * live workspaces of the repo point into it.
 */
export function looksLikeBrokenStore(e: Error): boolean {
  return /Internal error|repository appears broken|could not be found|not found|does not appear to be a git repository/i
    .test(e.message);
}

/**
 * Move a repo's external store aside — NEVER delete — so the next
 * createSessionWorkspace re-initializes it fresh. The store is derived state
 * (snapshots re-import from the repo's git); anything unrecoverable stays in the
 * `.broken-<ts>` copy for manual salvage. Returns the quarantine path, or null
 * when there was no store to move.
 */
export async function quarantineStore(repo: string): Promise<string | null> {
  const dir = await storeDirFor(repo);
  const dst = `${dir}.broken-${Date.now()}`;
  try {
    await Deno.rename(dir, dst);
    return dst;
  } catch {
    return null;
  }
}

/** External store dir for a repo: `<storeBase>/<name>-<hash>`, stable per canonical path. */
export async function storeDirFor(repo: string): Promise<string> {
  const real = await Deno.realPath(repo);
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(real));
  const hex = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0"))
    .join("")
    .slice(0, 12);
  const name = real.split("/").filter(Boolean).pop() ?? "repo";
  return `${storeBase()}/${name}-${hex}`;
}

/**
 * Ensure the external jj store for `repo` exists: a jj repo under `storeBase()`
 * backed by the repo's .git (`jj git init --git-repo`). The repo itself gains no
 * `.jj` dir and its HEAD/tree are never touched; jj state lives entirely outside.
 * Losing an init race to a concurrently starting session is fine.
 */
export async function ensureStore(repo: string): Promise<string> {
  const store = await storeDirFor(repo);
  if (await hasJjDir(store)) return store;
  await Deno.mkdir(store, { recursive: true });
  try {
    await jj(store, ["git", "init", `--git-repo=${repo}`]);
  } catch (e) {
    if (!(await hasJjDir(store))) throw e; // a concurrent session won the init
  }
  return store;
}

/**
 * Capture the repo's working tree (tracked edits + untracked files, .gitignore
 * respected) as a git commit without touching the repo's index, HEAD, or tree: a
 * throwaway GIT_INDEX_FILE stages everything, write-tree/commit-tree seal it. A
 * clean tree returns HEAD itself. Works on an unborn HEAD (parentless commit).
 */
async function captureBase(repo: string): Promise<string> {
  const headR = await run("git", ["rev-parse", "--verify", "-q", "HEAD"], repo);
  const head = headR.ok ? headR.stdout.trim() : null;
  const idx = await Deno.makeTempFile({ prefix: "bough-git-index-" });
  try {
    const env = { GIT_INDEX_FILE: idx };
    await git(repo, head ? ["read-tree", "HEAD"] : ["read-tree", "--empty"], env);
    await git(repo, ["add", "-A"], env);
    const tree = (await git(repo, ["write-tree"], env)).trim();
    if (head && tree === (await git(repo, ["rev-parse", "HEAD^{tree}"])).trim()) {
      return head;
    }
    const args = [
      "-c",
      `user.name=${JJ_USER}`,
      "-c",
      `user.email=${JJ_EMAIL}`,
      "commit-tree",
      "-m",
      "bough: session base (working-tree snapshot)",
    ];
    if (head) args.push("-p", "HEAD");
    args.push(tree);
    return (await git(repo, args, env)).trim();
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
}

/**
 * Create a root session's isolated working copy for a plain git repo, keeping jj
 * out of the repo entirely. The working tree (including uncommitted and untracked
 * files) is captured as a base commit, published as the git branch
 * `bough/<sessionId>` so jj can import it, and the session's jj workspace is
 * branched off it under `workspacesRoot()`. The repo's checkout is never
 * modified; the branch is the only visible trace, and `exportRefs` keeps it on
 * the session's tip so `git diff main bough/<id>` works from the user's checkout.
 * Idempotent: an existing workspace dir is reused as-is.
 */
export async function createSessionWorkspace(repo: string, sessionId: string): Promise<string> {
  const dir = workspaceDirFor(sessionId);
  if (await hasJjDir(dir)) return dir;
  const store = await ensureStore(repo);
  const base = await captureBase(repo);
  const name = bookmarkFor(sessionId);
  await git(repo, ["update-ref", `refs/heads/${name}`, base]);
  await jj(store, ["git", "import"]);
  await updateStale(store);
  await Deno.mkdir(workspacesRoot(), { recursive: true });
  await jj(store, ["workspace", "add", "--name", workspaceNameFor(sessionId), "-r", name, dir]);
  // The imported bookmark sits on the base commit; move it onto the workspace's
  // fresh working-copy change so diff/accept see change-vs-parent as usual.
  await jj(dir, ["bookmark", "move", name, "--to", "@"]);
  await exportRefs(dir);
  return dir;
}

/**
 * Push jj bookmarks out to git branches (`jj git export`) so session tips stay
 * reachable as `bough/<id>` refs from the user's checkout. Best-effort: export is
 * visibility, not correctness, and must never fail the operation that ran it.
 */
export async function exportRefs(dir: string): Promise<void> {
  try {
    await jj(dir, ["git", "export"]);
  } catch {
    // visibility only
  }
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
 *
 * `repo` must already carry jj state (a colocated checkout, another session's
 * workspace dir, or an external store) — this never initialises jj into a plain
 * git repo; a spawn from an un-tracked repo fails instead of colocating it.
 */
export async function addWorkspace(
  repo: string,
  sessionId: string,
  dir: string,
  baseBookmark: string,
): Promise<string> {
  if (await hasJjDir(dir)) return dir; // already added (dir has .jj)
  // An op from a sibling workspace (a concurrent spawn's snapshot, an adopt) can
  // rewrite this workspace's working-copy commit, leaving it stale — jj then
  // refuses to snapshot and `workspace add` fails, killing the spawn. Repair
  // first; a no-op when the working copy is fresh.
  await updateStale(repo);
  await jj(repo, [
    "workspace",
    "add",
    "--name",
    workspaceNameFor(sessionId),
    "-r",
    baseBookmark,
    dir,
  ]);
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
  // Keep the session's git ref on its tip whenever someone looks at the diff, so
  // external-store sessions stay reachable from plain git (no-op when unchanged).
  await exportRefs(repo);
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
 * `message`, when given, becomes the sealed commit's description (otherwise the
 * change keeps whatever it had — usually empty, which reads badly from plain git).
 */
export async function accept(repo: string, sessionId: string, message?: string): Promise<void> {
  await snapshot(repo);
  const name = bookmarkFor(sessionId);
  if (message) await jj(repo, ["describe", "-r", name, "-m", message]);
  await jj(repo, ["new", name]);
  await jj(repo, ["bookmark", "move", name, "--to", "@"]);
  await exportRefs(repo);
}

/**
 * The origin git repo behind a session's jj dir, from jj's own plumbing:
 * `<dir>/.jj/repo` (a pointer file for workspaces, the store itself otherwise) →
 * `<store>/store/git_target` → the backing `.git`. For an external-mode session
 * workspace this is the user's checkout; for a colocated repo it's the repo
 * itself (callers compare against `dir` to tell the modes apart). Null when the
 * plumbing can't be read (no jj, exotic store).
 */
export async function originRepo(dir: string): Promise<string | null> {
  try {
    const ptrPath = `${dir}/.jj/repo`;
    let repoDir = ptrPath;
    if ((await Deno.stat(ptrPath)).isFile) {
      const ptr = (await Deno.readTextFile(ptrPath)).trim();
      repoDir = ptr.startsWith("/") ? ptr : resolve(`${dir}/.jj`, ptr);
    }
    const target = (await Deno.readTextFile(`${repoDir}/store/git_target`)).trim();
    const gitDir = target.startsWith("/") ? target : resolve(`${repoDir}/store`, target);
    const real = await Deno.realPath(gitDir);
    return real.endsWith("/.git") ? dirname(real) : real;
  } catch {
    return null;
  }
}

/**
 * Deliver a session's reviewed edits into the origin checkout's working tree:
 * the change-vs-parent diff, limited to `paths`, applied with `git apply --3way`
 * (3-way so a file the user touched since the session branched merges instead of
 * being clobbered; conflicts surface as an error naming the file). Only the
 * working tree changes — HEAD, index, and branch stay put. No-op when the scoped
 * diff is empty.
 */
export async function materialize(
  workspace: string,
  sessionId: string,
  origin: string,
  paths: string[],
): Promise<void> {
  await snapshot(workspace);
  await exportRefs(workspace); // refresh bough/<id> in the origin's git first
  const name = bookmarkFor(sessionId);
  const all = (await git(origin, ["diff", "--name-only", `${name}^..${name}`]))
    .split("\n").map((s) => s.trim()).filter(Boolean);
  const targets = paths.length > 0 ? all.filter((p) => paths.includes(p)) : all;
  const failed: string[] = [];
  for (const p of targets) {
    // Already delivered? Compare the working file against the session tip's blob
    // (hash compare — exact and binary-safe). A re-press becomes a clean no-op.
    const tip = await run("git", ["rev-parse", `${name}:${p}`], origin);
    const cur = await run("git", ["hash-object", "--", p], origin);
    if (tip.ok ? cur.ok && cur.stdout.trim() === tip.stdout.trim() : !cur.ok) continue;
    const patch = await git(origin, ["diff", "--binary", `${name}^..${name}`, "--", p]);
    if (!patch.trim()) continue;
    // Plain apply first: it touches ONLY the working tree. --3way (which stages
    // what it merges) is the fallback for files the user changed since branching.
    try {
      await git(origin, ["apply", "--whitespace=nowarn"], undefined, patch);
    } catch {
      try {
        await git(origin, ["apply", "--3way", "--whitespace=nowarn"], undefined, patch);
      } catch (e) {
        failed.push(`${p}: ${(e as Error).message.trim().split("\n").at(-1)}`);
      }
    }
  }
  if (failed.length > 0) throw new Error(`could not apply ${failed.join("; ")}`);
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
