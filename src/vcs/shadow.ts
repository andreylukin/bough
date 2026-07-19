/**
 * Shadow-git snapshots — the jj replacement (docs/shadow-snapshots.md). One bare
 * "shadow" git repository per origin directory, kept entirely OUTSIDE it under
 * `~/.bough/shadow/<name>-<hash>`; the origin's own `.git` (if any) is never
 * written to — it is read only to link its objects in via alternates (session
 * bases graft onto the origin's HEAD, so worktrees see the repo's real history
 * in `git log`/`blame`) and for `git hash-object`/`git apply` against its
 * working tree at materialize time. Sessions are chains of snapshot commits on
 * shadow refs, and every session runs in its own linked worktree of the shadow
 * repo under `~/.bough/workspaces/<sessionId>`.
 *
 * Model:
 *   - `refs/bough/sessions/<id>` — the session's tip: a commit chain, one commit
 *     per `track()` (working-tree snapshot). Worktree HEAD rides along detached.
 *   - `refs/bough/base/<id>` — where the session branched. `diff()` is always
 *     base..tip; `accept()` seals by advancing base onto tip.
 *   - Linked worktrees give every session an isolated working copy with its own
 *     index for free (no shared-index cross-session bleed — the opencode #7774
 *     failure class is structurally impossible here).
 *
 * Discipline (scar tissue inherited from opencode/dura):
 *   - `git add -A` failures are FATAL to the snapshot — a swallowed add turns
 *     the next revert into data loss (opencode #12719).
 *   - Restores only ever touch an explicit path list, never the whole index.
 *   - Shadow-side git runs with global/system config isolated
 *     (GIT_CONFIG_GLOBAL=/dev/null): the user's autocrlf/hooksPath/gpgsign must
 *     not leak into snapshot plumbing. Origin-side calls keep the user's config.
 *   - `--3way` applies in the origin get the shadow's blobs via
 *     GIT_ALTERNATE_OBJECT_DIRECTORIES — env-scoped, nothing written to the
 *     origin's config.
 *
 * Works identically for non-git origin dirs: the shadow repo doesn't care what
 * the work-tree is.
 */
import { dirname, isAbsolute, join, resolve } from "node:path";
import { parseGitDiff } from "../schema/changes.ts";
import type { Diff } from "../schema/changes.ts";

const USER = "bough";
const EMAIL = "bough@localhost";

/** Session tip ref inside the shadow repo. */
export function refFor(sessionId: string): string {
  return `refs/bough/sessions/${sessionId}`;
}

/** Session base ref (branch point) inside the shadow repo. */
export function baseRefFor(sessionId: string): string {
  return `refs/bough/base/${sessionId}`;
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

/** Shadow-side env: user/system git config must not leak into snapshot plumbing. */
const ISOLATED = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };

/** Run git in `cwd` with config isolation; throws on non-zero exit. */
async function git(
  cwd: string,
  args: string[],
  env?: Record<string, string>,
  stdin?: string,
): Promise<string> {
  const r = await run("git", args, cwd, { ...ISOLATED, ...env }, stdin);
  if (!r.ok) {
    throw new Error(`git ${args.join(" ")} failed (${r.code}): ${r.stderr.trim()}`);
  }
  return r.stdout;
}

/** Run git in the origin with the USER's config intact (materialize-side calls). */
async function originGit(
  cwd: string,
  args: string[],
  env?: Record<string, string>,
  stdin?: string,
): Promise<string> {
  const r = await run("git", args, cwd, env, stdin);
  if (!r.ok) {
    throw new Error(`git ${args.join(" ")} failed (${r.code}): ${r.stderr.trim()}`);
  }
  return r.stdout;
}

/** `git --version` (or throws). Callers use this to gate on install. */
export async function version(): Promise<string> {
  const r = await run("git", ["--version"], Deno.cwd());
  if (!r.ok) throw new Error("git not installed");
  return r.stdout.trim();
}

/** Root for shadow stores: `$BOUGH_SHADOW_BASE` or `~/.bough/shadow`. */
export function storeBase(): string {
  const env = Deno.env.get("BOUGH_SHADOW_BASE");
  if (env) return env;
  const home = Deno.env.get("HOME");
  if (!home) throw new Error("shadow: no $HOME");
  return `${home}/.bough/shadow`;
}

/**
 * Root for per-session worktrees (isolated working copies), shared by root
 * sessions and subagents: `$BOUGH_SUBAGENT_BASE` or `~/.bough/workspaces`.
 * Same location the jj backend used — session dirs are interchangeable ideas.
 */
export function workspacesRoot(): string {
  const env = Deno.env.get("BOUGH_SUBAGENT_BASE");
  if (env) return env;
  const home = Deno.env.get("HOME");
  if (!home) throw new Error("shadow: no $HOME");
  return `${home}/.bough/workspaces`;
}

/** A session's own working-copy dir under `workspacesRoot()`. */
export function workspaceDirFor(sessionId: string): string {
  return `${workspacesRoot()}/${sessionId}`;
}

/** Shadow store dir for an origin: `<storeBase>/<name>-<hash>`, stable per canonical path. */
export async function storeDirFor(origin: string): Promise<string> {
  const real = await Deno.realPath(origin);
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(real));
  const hex = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0"))
    .join("")
    .slice(0, 12);
  const name = real.split("/").filter(Boolean).pop() ?? "dir";
  return `${storeBase()}/${name}-${hex}`;
}

/**
 * True when an error reads like shadow-store corruption, as opposed to a
 * transient or environmental failure. Gates quarantine-and-retry: a healthy
 * store must never be moved aside — live worktrees of the origin point into it.
 */
export function looksLikeBrokenStore(e: Error): boolean {
  return /not a git repository|corrupt|unable to read|bad object|does not exist|invalid gitfile/i
    .test(e.message);
}

/**
 * Move an origin's shadow store aside — NEVER delete — so the next
 * createSessionWorkspace re-initializes it fresh. Snapshots are derived state;
 * anything unrecoverable stays in the `.broken-<ts>` copy for manual salvage.
 */
export async function quarantineStore(origin: string): Promise<string | null> {
  const dir = await storeDirFor(origin);
  const dst = `${dir}.broken-${Date.now()}`;
  try {
    await Deno.rename(dir, dst);
    return dst;
  } catch {
    return null;
  }
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
 * Ensure the shadow repo for `origin` exists: a bare git dir under `storeBase()`
 * plus a `bough-origin` pointer file back to the origin. Identity, EOL, and gc
 * are pinned locally so snapshots are deterministic regardless of user config.
 * Losing an init race to a concurrently starting session is fine.
 */
export async function ensureStore(origin: string): Promise<string> {
  const store = await storeDirFor(origin);
  if (await pathExists(`${store}/HEAD`)) {
    await linkOriginObjects(store, origin);
    return store;
  }
  await Deno.mkdir(store, { recursive: true });
  try {
    await git(store, ["init", "--bare", "-b", "bough", "."]);
  } catch (e) {
    if (!(await pathExists(`${store}/HEAD`))) throw e; // a concurrent session won
  }
  for (
    const [k, v] of [
      ["user.name", USER],
      ["user.email", EMAIL],
      ["core.autocrlf", "false"],
      ["core.quotepath", "false"],
      ["commit.gpgsign", "false"],
      ["gc.auto", "0"],
    ]
  ) {
    await git(store, ["config", k, v]);
  }
  // The user's global excludesFile is config-isolated away; keep the one entry
  // everyone expects from it. Project .gitignore files apply as usual.
  await Deno.writeTextFile(`${store}/info/exclude`, ".DS_Store\n");
  await Deno.writeTextFile(`${store}/bough-origin`, await Deno.realPath(origin));
  await linkOriginObjects(store, origin);
  return store;
}

/**
 * Point the shadow store's `objects/info/alternates` at the origin repo's
 * object dir (read-only from the shadow side), so session bases can graft onto
 * the origin's HEAD and worktrees see the repo's real history. No-op when the
 * origin isn't itself a git toplevel (plain dirs, nested non-repo subdirs).
 * Idempotent — re-run on every ensureStore so pre-existing stores pick it up.
 */
async function linkOriginObjects(store: string, origin: string): Promise<void> {
  const top = await run("git", ["rev-parse", "--show-toplevel"], origin, ISOLATED);
  if (!top.ok) return;
  const topReal = await Deno.realPath(top.stdout.trim()).catch(() => null);
  const originReal = await Deno.realPath(origin).catch(() => null);
  if (!topReal || topReal !== originReal) return;
  const common = await gitCommonDir(origin);
  if (!common) return;
  const objects = join(common, "objects");
  if (!(await pathExists(objects))) return;
  await Deno.mkdir(`${store}/objects/info`, { recursive: true });
  await Deno.writeTextFile(`${store}/objects/info/alternates`, objects + "\n");
}

/**
 * Capture the origin's working tree (tracked + untracked, .gitignore respected)
 * as a parentless commit in the shadow repo, via a throwaway index. The origin's
 * own index/HEAD/tree are never touched — for a non-git origin there is nothing
 * to touch. Returns the commit sha.
 */
async function captureBase(store: string, origin: string): Promise<string> {
  const idx = await Deno.makeTempFile({ prefix: "bough-shadow-index-" });
  try {
    const env = { GIT_DIR: store, GIT_WORK_TREE: origin, GIT_INDEX_FILE: idx };
    await git(origin, ["read-tree", "--empty"], env);
    await git(origin, ["add", "-A"], env);
    const tree = (await git(origin, ["write-tree"], env)).trim();
    const args = ["commit-tree", tree, "-m", "bough: session base (working-tree snapshot)"];
    // Graft the origin's real history under the base: HEAD becomes the parent
    // when the store can actually read it (alternates linked by ensureStore) —
    // otherwise (non-git origin, unborn branch) the base stays parentless.
    const head = await run(
      "git",
      ["rev-parse", "--verify", "-q", "HEAD^{commit}"],
      origin,
      ISOLATED,
    );
    if (head.ok) {
      const sha = head.stdout.trim();
      const visible = await run("git", ["cat-file", "-e", sha], origin, {
        ...ISOLATED,
        GIT_DIR: store,
      });
      if (visible.ok) args.splice(2, 0, "-p", sha);
    }
    return (await git(origin, args, env)).trim();
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
}

/** Per-store serialization for ref-moving ops (parallel subagents adopt into one parent). */
const locks = new Map<string, Promise<unknown>>();
function withLock<T>(key: string, fn: () => Promise<T>): Promise<T> {
  const prev = locks.get(key) ?? Promise.resolve();
  const next = prev.then(fn, fn);
  locks.set(key, next.catch(() => {}));
  return next;
}

/**
 * Create a root session's isolated working copy: capture the origin's tree as
 * the session base, point `refs/bough/{base,sessions}/<id>` at it, and check
 * out a detached linked worktree under `workspacesRoot()`. The origin is never
 * modified. Idempotent: an existing worktree dir is reused as-is.
 */
export async function createSessionWorkspace(origin: string, sessionId: string): Promise<string> {
  const dir = workspaceDirFor(sessionId);
  if (await pathExists(`${dir}/.git`)) return dir;
  const store = await ensureStore(origin);
  return await withLock(store, async () => {
    const base = await captureBase(store, origin);
    await git(store, ["update-ref", baseRefFor(sessionId), base]);
    await git(store, ["update-ref", refFor(sessionId), base]);
    await Deno.mkdir(workspacesRoot(), { recursive: true });
    await git(store, ["worktree", "add", "--detach", dir, base]);
    await hydrate(origin, dir);
    return dir;
  });
}

/**
 * Give a session its own worktree branched off ANOTHER session's tip — the
 * multi-working-copy fork used by subagent spawns and fork sessions. `fromDir`
 * (the spawner/parent's worktree) is snapshotted first so the child inherits
 * on-disk work, unless `fromSessionId` is null, in which case the child branches
 * off `fromDir`'s HEAD as-is. Idempotent: an existing worktree dir is reused.
 */
export async function addWorkspace(
  fromDir: string,
  sessionId: string,
  dir: string,
  fromSessionId: string | null,
): Promise<string> {
  if (await pathExists(`${dir}/.git`)) return dir;
  const store = await gitCommonDir(fromDir);
  // The bough-origin pointer marks a bough shadow store. Never branch inside an
  // arbitrary git dir — a spawn from an un-tracked repo must fail, not write
  // refs/worktrees into the user's own .git (or a legacy jj workspace).
  if (!store || !(await pathExists(`${store}/bough-origin`))) {
    throw new Error(`not a shadow worktree: ${fromDir}`);
  }
  const tip = fromSessionId !== null
    ? await track(fromDir, fromSessionId)
    : (await git(fromDir, ["rev-parse", "HEAD"])).trim();
  return await withLock(store, async () => {
    await git(store, ["update-ref", baseRefFor(sessionId), tip]);
    await git(store, ["update-ref", refFor(sessionId), tip]);
    await Deno.mkdir(workspacesRoot(), { recursive: true });
    await git(store, ["worktree", "add", "--detach", dir, tip]);
    await hydrate(fromDir, dir); // the parent's runtime artifacts, already hydrated once
    return dir;
  });
}

/** Resolve a worktree's shadow store root (its git common dir), or null. */
async function gitCommonDir(dir: string): Promise<string | null> {
  const r = await run("git", ["rev-parse", "--git-common-dir"], dir, ISOLATED);
  if (!r.ok) return null;
  const p = r.stdout.trim();
  return isAbsolute(p) ? p : resolve(dir, p);
}

/**
 * Gitignored runtime artifacts a fresh worktree needs to actually RUN the code —
 * a checkout carries tracked+untracked files only, so deps/venvs/env files never
 * arrive on their own. Root-level candidates plus one-level-deep node_modules
 * (monorepo web/ dirs). Cloned with APFS clonefile (`cp -c`): instant, CoW, and
 * fully isolated from the source's copies.
 */
const HYDRATE_CANDIDATES = [
  "node_modules",
  ".venv",
  "venv",
  "target",
  "vendor",
  ".env",
  ".env.local",
];

/**
 * Copy runtime artifacts from `source` (the origin, or a parent worktree) into a
 * fresh worktree. Best-effort by design: a failed or unsupported clone (non-APFS,
 * cross-volume) skips that artifact — the session still starts; the agent can
 * reinstall deps itself.
 */
async function hydrate(source: string, dir: string): Promise<void> {
  const targets: string[] = [...HYDRATE_CANDIDATES];
  // One level deep: <subdir>/node_modules (e.g. web/node_modules).
  try {
    for await (const e of Deno.readDir(source)) {
      if (!e.isDirectory || e.name.startsWith(".") || e.name === "node_modules") continue;
      targets.push(`${e.name}/node_modules`);
    }
  } catch { /* unreadable source — nothing to hydrate */ }
  for (const rel of targets) {
    const from = join(source, rel);
    const to = join(dir, rel);
    if (!(await pathExists(from)) || (await pathExists(to))) continue;
    const r = await run("cp", ["-Rc", from, to], dir);
    if (!r.ok) {
      await Deno.remove(to, { recursive: true }).catch(() => {}); // no half-copies
      console.error(`shadow: hydrate skipped ${rel}: ${r.stderr.trim().split("\n")[0]}`);
    }
  }
}

/**
 * Snapshot a session worktree: stage everything (fatal on failure), commit onto
 * the session's tip ref, and move the worktree's detached HEAD along so
 * `git status` stays clean. No-ops (returning the existing tip) when the tree
 * is unchanged. Returns the tip sha. Serialized per worktree — parallel
 * subagent spawns each snapshot the spawner, and concurrent `git add` calls in
 * one worktree collide on its index.lock.
 */
export function track(dir: string, sessionId: string, message?: string): Promise<string> {
  return withLock(dir, () => trackInner(dir, sessionId, message));
}

async function trackInner(dir: string, sessionId: string, message?: string): Promise<string> {
  await git(dir, ["add", "-A"]);
  const tree = (await git(dir, ["write-tree"])).trim();
  const tipR = await run("git", ["rev-parse", "--verify", "-q", refFor(sessionId)], dir, ISOLATED);
  if (!tipR.ok) throw new Error(`shadow: unknown session ref ${refFor(sessionId)}`);
  const tip = tipR.stdout.trim();
  const tipTree = (await git(dir, ["rev-parse", `${tip}^{tree}`])).trim();
  if (tree === tipTree) return tip;
  const next = (await git(dir, [
    "commit-tree",
    tree,
    "-p",
    tip,
    "-m",
    message ?? "bough: snapshot",
  ])).trim();
  await git(dir, ["update-ref", refFor(sessionId), next, tip]);
  await git(dir, ["update-ref", "HEAD", next]);
  return next;
}

/** A ref's sha in the worktree's shadow repo, or null. */
async function refSha(dir: string, ref: string): Promise<string | null> {
  const r = await run("git", ["rev-parse", "--verify", "-q", ref], dir, ISOLATED);
  return r.ok ? r.stdout.trim() : null;
}

/**
 * The structured diff of a session's change vs. where it branched (base..tip).
 * Snapshots first so on-disk edits are included. Missing refs (a session that
 * never got a workspace, or pruned state) degrade to an empty diff.
 */
export async function diff(dir: string, sessionId: string): Promise<Diff> {
  try {
    await track(dir, sessionId);
  } catch {
    return { source: "shadow", files: [] };
  }
  const base = await refSha(dir, baseRefFor(sessionId));
  const tip = await refSha(dir, refFor(sessionId));
  if (!base || !tip) return { source: "shadow", files: [] };
  const out = await git(dir, ["diff", "--no-color", "--no-ext-diff", base, tip]);
  return { source: "shadow", files: parseGitDiff(out) };
}

/**
 * Adopt a subagent session's work into its spawner: both worktrees are
 * snapshotted, the subagent's base..tip patch is 3-way-applied into the
 * spawner's worktree (shared object store, so blob lookups always succeed), and
 * the spawner is snapshotted again. The subagent's refs and worktree stay
 * alive, so its branch remains continuable — same contract as jj's
 * `squash --keep-emptied`.
 */
export function adoptChanges(
  parentDir: string,
  subDir: string,
  fromSessionId: string,
  intoSessionId: string,
): Promise<void> {
  // Serialized per parent (distinct key from track's per-dir lock — the inner
  // tracks still take that one): two parallel adopts patching one worktree
  // otherwise interleave apply and snapshot.
  return withLock(`adopt:${parentDir}`, () =>
    adoptInner(parentDir, subDir, fromSessionId, intoSessionId));
}

async function adoptInner(
  parentDir: string,
  subDir: string,
  fromSessionId: string,
  intoSessionId: string,
): Promise<void> {
  await track(subDir, fromSessionId);
  await track(parentDir, intoSessionId);
  const base = await refSha(subDir, baseRefFor(fromSessionId));
  const tip = await refSha(subDir, refFor(fromSessionId));
  if (!base || !tip || base === tip) return; // nothing to adopt
  const patch = await git(subDir, ["diff", "--binary", base, tip]);
  if (!patch.trim()) return;
  await git(parentDir, ["apply", "--3way", "--whitespace=nowarn"], undefined, patch);
  await track(parentDir, intoSessionId, `bough: adopt ${fromSessionId}`);
  // The adopted work now belongs to the spawner; advance the subagent's base so
  // its own rail clears (disk untouched, branch continuable) — the shadow
  // analogue of `jj squash --keep-emptied` emptying the source change.
  await git(subDir, ["update-ref", baseRefFor(fromSessionId), tip]);
}

/**
 * Accept a session's reviewed change: snapshot (the commit message becomes the
 * seal description) and advance base onto tip. The work stays on disk; the
 * base..tip diff — and so the Changes rail — clears.
 */
export async function accept(dir: string, sessionId: string, message?: string): Promise<void> {
  const tip = await track(dir, sessionId, message);
  await git(dir, ["update-ref", baseRefFor(sessionId), tip]);
}

/**
 * The origin directory a shadow worktree ultimately snapshots, from the
 * `bough-origin` pointer in its store. Null when `dir` isn't a shadow worktree
 * (plain repos, clonefile sessions, legacy jj dirs).
 */
export async function originRepo(dir: string): Promise<string | null> {
  try {
    const store = await gitCommonDir(dir);
    if (!store) return null;
    const origin = (await Deno.readTextFile(`${store}/bough-origin`)).trim();
    return origin || null;
  } catch {
    return null;
  }
}

/** A path's blob bytes at `rev` in the worktree's shadow repo, or null if absent. */
async function blobAt(dir: string, rev: string, path: string): Promise<Uint8Array | null> {
  const cmd = new Deno.Command("git", {
    args: ["show", `${rev}:${path}`],
    cwd: dir,
    env: ISOLATED,
    stdout: "piped",
    stderr: "null",
  });
  const r = await cmd.output();
  return r.code === 0 ? r.stdout : null;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * Merge one file's base→tip change into the origin's current copy, touching
 * ONLY the working tree (no index — `git apply --3way` refuses on unstaged
 * origin edits, which is the normal state of a user's checkout). Modify/modify
 * merges via `git merge-file`; conflicts throw with the reason.
 */
async function threeWayFile(
  dir: string,
  base: string,
  tip: string,
  origin: string,
  path: string,
): Promise<void> {
  const baseBlob = await blobAt(dir, base, path);
  const tipBlob = await blobAt(dir, tip, path);
  const target = join(origin, path);
  const cur = await Deno.readFile(target).catch(() => null);
  if (!tipBlob) {
    // Deleted in the session.
    if (cur === null) return;
    if (baseBlob && bytesEqual(cur, baseBlob)) {
      await Deno.remove(target);
      return;
    }
    throw new Error("deleted in session but modified in origin");
  }
  if (cur === null || (baseBlob && bytesEqual(cur, baseBlob))) {
    await Deno.mkdir(dirname(target), { recursive: true });
    await Deno.writeFile(target, tipBlob);
    return;
  }
  if (!baseBlob) {
    // Added in the session AND independently present in the origin.
    if (bytesEqual(cur, tipBlob)) return;
    throw new Error("added in session but a different file exists in origin");
  }
  const tmp = await Deno.makeTempDir({ prefix: "bough-merge-" });
  try {
    await Deno.writeFile(`${tmp}/base`, baseBlob);
    await Deno.writeFile(`${tmp}/ours`, cur);
    await Deno.writeFile(`${tmp}/theirs`, tipBlob);
    // -p prints the merge to stdout; the origin file is only written on success.
    const r = await run("git", [
      "merge-file",
      "-p",
      "-L",
      "origin",
      "-L",
      "base",
      "-L",
      "session",
      `${tmp}/ours`,
      `${tmp}/base`,
      `${tmp}/theirs`,
    ], origin);
    if (r.code !== 0) throw new Error("conflicts with origin edits");
    await Deno.writeTextFile(target, r.stdout);
  } finally {
    await Deno.remove(tmp, { recursive: true }).catch(() => {});
  }
}

/**
 * Deliver a session's reviewed edits into the origin's working tree: the
 * base..tip diff, limited to `paths`, applied per file — plain `git apply`
 * first (clean case), a content-level 3-way merge as the fallback for files
 * the user changed since the session branched (conflicts surface as an error
 * naming the file). The origin's HEAD, index, and branch stay put — the
 * fallback never touches the index, so unstaged origin edits merge fine.
 * Already-delivered files (working copy hash == tip blob) are skipped, so a
 * re-press is a clean no-op.
 */
export async function materialize(
  dir: string,
  sessionId: string,
  origin: string,
  paths: string[],
): Promise<void> {
  await track(dir, sessionId);
  const base = await refSha(dir, baseRefFor(sessionId));
  const tip = await refSha(dir, refFor(sessionId));
  if (!base || !tip) return;
  const all = (await git(dir, ["diff", "--name-only", base, tip]))
    .split("\n").map((s) => s.trim()).filter(Boolean);
  const targets = paths.length > 0 ? all.filter((p) => paths.includes(p)) : all;
  const failed: string[] = [];
  for (const p of targets) {
    const tipBlob = await run("git", ["rev-parse", `${tip}:${p}`], dir, ISOLATED);
    const cur = await run("git", ["hash-object", "--", p], origin);
    if (tipBlob.ok ? cur.ok && cur.stdout.trim() === tipBlob.stdout.trim() : !cur.ok) continue;
    const patch = await git(dir, ["diff", "--binary", base, tip, "--", p]);
    if (!patch.trim()) continue;
    try {
      await originGit(origin, ["apply", "--whitespace=nowarn"], undefined, patch);
    } catch {
      try {
        await threeWayFile(dir, base, tip, origin, p);
      } catch (e) {
        failed.push(`${p}: ${(e as Error).message.trim().split("\n").at(-1)}`);
      }
    }
  }
  if (failed.length > 0) throw new Error(`could not apply ${failed.join("; ")}`);
}

export interface ShipResult {
  /** The new commit sha in the origin, or null when there was nothing to commit. */
  commit: string | null;
  /** The origin branch the commit landed on. */
  branch: string;
  /** Paths included in the commit. */
  paths: string[];
  /** True when the branch was pushed to its remote. */
  pushed: boolean;
  /** Human-readable caveat (nothing to ship, no remote to push to, …). */
  note?: string;
}

/**
 * Ship a session's work into the origin as a real commit: materialize the
 * selected paths into the origin's working tree (content-level 3-way; conflicts
 * throw), build the commit through a THROWAWAY index seeded from HEAD — the
 * user's own index/staging is never read or written — advance the origin's
 * current branch, seal the session (base → tip), and optionally `git push`.
 * The commit is authored with the origin's own git identity/config, and the
 * push uses the user's normal credentials (keychain), exactly as if they had
 * typed it. Refuses a detached-HEAD origin: there is no branch to advance.
 */
export async function shipToOrigin(
  dir: string,
  sessionId: string,
  origin: string,
  opts: { message: string; paths?: string[]; push?: boolean },
): Promise<ShipResult> {
  await track(dir, sessionId);
  const base = await refSha(dir, baseRefFor(sessionId));
  const tip = await refSha(dir, refFor(sessionId));
  if (!base || !tip) throw new Error(`ship: unknown session refs for ${sessionId}`);
  const branchR = await run("git", ["symbolic-ref", "--short", "-q", "HEAD"], origin);
  if (!branchR.ok) {
    throw new Error("ship: the origin checkout is on a detached HEAD — check out a branch first");
  }
  const branch = branchR.stdout.trim();
  const all = (await git(dir, ["diff", "--name-only", base, tip]))
    .split("\n").map((s) => s.trim()).filter(Boolean);
  const paths = opts.paths && opts.paths.length > 0
    ? all.filter((p) => opts.paths!.includes(p))
    : all;
  if (paths.length === 0) {
    return { commit: null, branch, paths: [], pushed: false, note: "nothing to ship" };
  }
  await materialize(dir, sessionId, origin, paths);
  // Commit exactly HEAD + the shipped paths via a temp index. `git add` there
  // reads the just-materialized working files; the user's index stays untouched,
  // so anything they had staged remains staged.
  const idx = await Deno.makeTempFile({ prefix: "bough-ship-index-" });
  let commit: string | null = null;
  try {
    const env = { GIT_INDEX_FILE: idx };
    const headR = await run("git", ["rev-parse", "--verify", "-q", "HEAD"], origin, env);
    const head = headR.ok ? headR.stdout.trim() : null;
    await originGit(origin, head ? ["read-tree", "HEAD"] : ["read-tree", "--empty"], env);
    await originGit(origin, ["add", "--", ...paths], env);
    const tree = (await originGit(origin, ["write-tree"], env)).trim();
    if (head && tree === (await originGit(origin, ["rev-parse", "HEAD^{tree}"])).trim()) {
      await accept(dir, sessionId, opts.message);
      return { commit: null, branch, paths, pushed: false, note: "already committed" };
    }
    const args = ["commit-tree", tree, "-m", opts.message];
    if (head) args.push("-p", head);
    commit = (await originGit(origin, args, env)).trim();
    await originGit(
      origin,
      head
        ? ["update-ref", `refs/heads/${branch}`, commit, head]
        : ["update-ref", `refs/heads/${branch}`, commit],
    );
    // Sync ONLY the shipped paths into the real index (adds and deletions):
    // without this, the advanced HEAD reads the stale index as phantom staged
    // deletions in `git status`. Everything else the user staged stays staged —
    // the end state is exactly `git add <paths> && git commit`.
    await originGit(origin, ["add", "--all", "--", ...paths]);
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
  await accept(dir, sessionId, opts.message);
  if (!opts.push) return { commit, branch, paths, pushed: false };
  const remote =
    (await run("git", ["config", `branch.${branch}.remote`], origin)).stdout.trim() || "origin";
  const hasRemote = (await run("git", ["remote"], origin)).stdout.split("\n").includes(remote);
  if (!hasRemote) {
    return { commit, branch, paths, pushed: false, note: `no remote "${remote}" to push to` };
  }
  await originGit(origin, ["push", remote, branch]);
  return { commit, branch, paths, pushed: true };
}

/** Restore `path` in the worktree to its state at `rev` (delete if absent there). */
async function restorePath(dir: string, rev: string, path: string): Promise<void> {
  const inRev = await run("git", ["cat-file", "-e", `${rev}:${path}`], dir, ISOLATED);
  if (inRev.ok) {
    await git(dir, ["checkout", rev, "--", path]);
  } else {
    await Deno.remove(join(dir, path)).catch(() => {});
    await run("git", ["rm", "--cached", "-q", "--force", "--", path], dir, ISOLATED);
  }
}

/**
 * Per-path revert: restore ONLY `paths` back to the session's base, leaving
 * every other edited path intact, then snapshot so the tip reflects the revert.
 * No-op when `paths` is empty.
 */
export async function revertPaths(dir: string, sessionId: string, paths: string[]): Promise<void> {
  if (paths.length === 0) return;
  await track(dir, sessionId);
  const base = await refSha(dir, baseRefFor(sessionId));
  if (!base) throw new Error(`shadow: unknown session ref ${baseRefFor(sessionId)}`);
  for (const p of paths) await restorePath(dir, base, p);
  await track(dir, sessionId, "bough: revert paths");
}

/**
 * Whole-change revert: restore every base..tip path back to base (explicit path
 * list — never an index-wide checkout), snapshot, and return the reverted
 * paths. The session's history stays in the ref chain for salvage.
 */
export async function undoAll(dir: string, sessionId: string): Promise<string[]> {
  await track(dir, sessionId);
  const base = await refSha(dir, baseRefFor(sessionId));
  const tip = await refSha(dir, refFor(sessionId));
  if (!base || !tip || base === tip) return [];
  const paths = (await git(dir, ["diff", "--name-only", base, tip]))
    .split("\n").map((s) => s.trim()).filter(Boolean);
  for (const p of paths) await restorePath(dir, base, p);
  await track(dir, sessionId, "bough: revert all");
  return paths;
}
