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
 *   - `refs/bough/originbase/<id>` — the origin's tree at branch time, never
 *     moved (forks/subagents inherit it). materialize/ship diff and merge
 *     against THIS ref; the base ref is the rail cursor and moves under it.
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
import { type Diff, parseGitDiff } from "../schema/changes.ts";
import { pathExists } from "../fsutil.ts";
import { homeStrict } from "../paths.ts";

const USER = "bough";
const EMAIL = "bough@localhost";

/** Session tip ref inside the shadow repo. */
export function refFor(sessionId: string): string {
  return `refs/bough/sessions/${sessionId}`;
}

/** Session base ref (branch point) inside the shadow repo. */
function baseRefFor(sessionId: string): string {
  return `refs/bough/base/${sessionId}`;
}

/**
 * Session origin-base ref: the origin's tree at the moment the session's chain
 * branched off it. NEVER advanced — unlike the base ref, which doubles as the
 * Changes-rail cursor and is moved by accept/adopt. Delivery to the origin
 * (materialize/ship) must diff and 3-way against THIS ref: using the rail
 * cursor as a merge base makes half-shipped work look like origin deletions
 * (phantom delete/modify conflicts), and a fork created after the work exists
 * (base == tip) sees nothing to ship at all.
 */
function originBaseRefFor(sessionId: string): string {
  return `refs/bough/originbase/${sessionId}`;
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

/** Root for shadow stores: `$BOUGH_SHADOW_BASE` or `~/.bough/shadow`. */
export function storeBase(): string {
  const env = Deno.env.get("BOUGH_SHADOW_BASE");
  if (env) return env;
  return `${homeStrict("shadow")}/.bough/shadow`;
}

/**
 * Root for per-session worktrees (isolated working copies), shared by root
 * sessions and subagents: `$BOUGH_SUBAGENT_BASE` or `~/.bough/workspaces`.
 * Same location the jj backend used — session dirs are interchangeable ideas.
 */
export function workspacesRoot(): string {
  const env = Deno.env.get("BOUGH_SUBAGENT_BASE");
  if (env) return env;
  return `${homeStrict("shadow")}/.bough/workspaces`;
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
 * The shadow-store dirs a sandboxed shell running in worktree `ws` needs write
 * access to for ordinary git use (`add`/`commit`/`fetch`/`stash`): the
 * worktree's own git dir (index, HEAD, locks) and the store's object database.
 * Deliberately NOT the store root — shared state (`refs/`, `config`,
 * `packed-refs`) stays read-only, so a session cannot move another session's
 * refs or repoint the store (in-sandbox `git branch` fails; commits ride the
 * detached worktree HEAD, which is bough's model anyway).
 *
 * `packed-refs.lock` is the one extra file: `git commit`'s ref transaction
 * probes it and spams "Unable to create packed-refs.lock" on every in-sandbox
 * commit otherwise. Allowing the lockfile is harmless — replacing packed-refs
 * itself requires a rename into the store root, which stays denied.
 *
 * Empty unless `ws/.git` is a worktree gitfile pointing under `storeBase()` —
 * a session that fell back to running directly in the user's checkout must not
 * gain write access to the real repo's `.git`.
 */
export async function sandboxGitWriteDirs(ws: string): Promise<string[]> {
  let gitdir: string;
  try {
    const gitfile = await Deno.readTextFile(join(ws, ".git"));
    const m = gitfile.match(/^gitdir:\s*(.+)\s*$/m);
    if (!m) return [];
    gitdir = isAbsolute(m[1]) ? m[1] : resolve(ws, m[1]);
  } catch {
    return []; // no .git, or .git is a directory (not a linked worktree)
  }
  const store = await Deno.realPath(dirname(dirname(gitdir))).catch(() => null);
  const base = await Deno.realPath(storeBase()).catch(() => null);
  if (!store || !base || store !== base && !store.startsWith(base + "/")) return [];
  return [gitdir, join(store, "objects"), join(store, "packed-refs.lock")];
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

/**
 * Ensure the shadow repo for `origin` exists: a bare git dir under `storeBase()`
 * plus a `bough-origin` pointer file back to the origin. Identity, EOL, and gc
 * are pinned locally so snapshots are deterministic regardless of user config.
 * Losing an init race to a concurrently starting session is fine.
 */
async function ensureStore(origin: string): Promise<string> {
  const store = await storeDirFor(origin);
  if (!(await pathExists(`${store}/HEAD`))) {
    await Deno.mkdir(store, { recursive: true });
    try {
      await git(store, ["init", "--bare", "-b", "bough", "."]);
    } catch (e) {
      if (!(await pathExists(`${store}/HEAD`))) throw e; // a concurrent session won
    }
    // The user's global excludesFile is config-isolated away; keep the one entry
    // everyone expects from it. Project .gitignore files apply as usual.
    await Deno.writeTextFile(`${store}/info/exclude`, ".DS_Store\n");
    await Deno.writeTextFile(`${store}/bough-origin`, await Deno.realPath(origin));
  }
  // Pinned on every call (not just init) so pre-existing stores pick up additions.
  // maintenance.auto: in-sandbox `git commit` otherwise spawns `maintenance run
  // --auto`, whose pack-refs task hits the write-protected store refs and spams
  // "Unable to create packed-refs.lock" (gc.auto=0 does not cover it).
  for (
    const [k, v] of [
      ["user.name", USER],
      ["user.email", EMAIL],
      ["core.autocrlf", "false"],
      ["core.quotepath", "false"],
      ["commit.gpgsign", "false"],
      ["gc.auto", "0"],
      ["maintenance.auto", "false"],
    ]
  ) {
    await git(store, ["config", k, v]);
  }
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
    await git(store, ["update-ref", originBaseRefFor(sessionId), base]);
    await git(store, ["update-ref", refFor(sessionId), base]);
    await Deno.mkdir(workspacesRoot(), { recursive: true });
    await git(store, ["worktree", "add", "--detach", dir, base]);
    startHydration(origin, dir);
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
  // The child inherits the parent's origin branch point, so delivery still
  // diffs against what the origin actually had — a fork's rail base (= parent
  // tip) would make inherited work unshippable. Falls back to the tip for
  // parents that pre-date originbase refs and for branch-off-HEAD spawns.
  const originBase = (fromSessionId !== null
    ? await refSha(fromDir, originBaseRefFor(fromSessionId))
    : null) ?? tip;
  return await withLock(store, async () => {
    await git(store, ["update-ref", baseRefFor(sessionId), tip]);
    await git(store, ["update-ref", originBaseRefFor(sessionId), originBase]);
    await git(store, ["update-ref", refFor(sessionId), tip]);
    await Deno.mkdir(workspacesRoot(), { recursive: true });
    await git(store, ["worktree", "add", "--detach", dir, tip]);
    startHydration(fromDir, dir); // the parent's runtime artifacts, already hydrated once
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
/**
 * Runtime-artifact hydration runs in the BACKGROUND: the worktree itself is
 * ready in ~0.1s (git checkout), but cloning node_modules et al. takes ~1-2s
 * (many small inodes, even with clonefile). Starting it detached lets the turn's
 * first LLM round overlap the copy; `awaitHydration()` gates the turn's first
 * tool on completion so code never runs against a half-populated tree. Keyed by
 * worktree dir. hydrate() swallows its own errors, so this promise never rejects.
 */
const hydrations = new Map<string, Promise<void>>();

function startHydration(source: string, dir: string): void {
  hydrations.set(dir, hydrate(source, dir));
}

/** Await (and forget) a worktree's background hydration, if one is in flight. */
export async function awaitHydration(dir: string): Promise<void> {
  const p = hydrations.get(dir);
  if (!p) return;
  try {
    await p;
  } finally {
    hydrations.delete(dir);
  }
}

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
  return withLock(
    `adopt:${parentDir}`,
    () => adoptInner(parentDir, subDir, fromSessionId, intoSessionId),
  );
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

/** One path's planned delivery, computed BEFORE anything is written. */
type PlannedFile =
  | { kind: "skip" } // origin already has the session's content — nothing to write, but a ship commit still includes it
  | { kind: "noop" } // origin diverged but already contains the session's change — deliver and commit nothing
  | { kind: "write"; content: Uint8Array }
  | { kind: "delete" };

/**
 * Plan one file's base→tip delivery into the origin's current copy: exact-copy
 * fast path when the origin still matches the base, else a content-level 3-way
 * via `git merge-file` (never the index — `git apply --3way` refuses on
 * unstaged origin edits, which is the normal state of a user's checkout).
 * Conflicts throw with the reason; nothing is written here — materialize only
 * writes once EVERY selected path has planned cleanly.
 */
async function planFile(
  dir: string,
  base: string,
  tip: string,
  origin: string,
  path: string,
): Promise<PlannedFile> {
  const baseBlob = await blobAt(dir, base, path);
  const tipBlob = await blobAt(dir, tip, path);
  const cur = await Deno.readFile(join(origin, path)).catch(() => null);
  if (!tipBlob) {
    // Deleted in the session.
    if (cur === null) return { kind: "skip" };
    if (baseBlob && bytesEqual(cur, baseBlob)) return { kind: "delete" };
    throw new Error("deleted in session but modified in origin");
  }
  if (cur !== null && bytesEqual(cur, tipBlob)) return { kind: "skip" };
  if (cur === null || (baseBlob && bytesEqual(cur, baseBlob))) {
    return { kind: "write", content: tipBlob };
  }
  if (!baseBlob) {
    // Added in the session AND independently present in the origin.
    throw new Error("added in session but a different file exists in origin");
  }
  const tmp = await Deno.makeTempDir({ prefix: "bough-merge-" });
  try {
    await Deno.writeFile(`${tmp}/base`, baseBlob);
    await Deno.writeFile(`${tmp}/ours`, cur);
    await Deno.writeFile(`${tmp}/theirs`, tipBlob);
    // -p prints the merge to stdout; nothing on disk changes on conflict.
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
    const merged = new TextEncoder().encode(r.stdout);
    // The merge added nothing (the origin already carries the session's change
    // plus its own edits) — leave the user's copy alone, keep it out of commits.
    if (bytesEqual(merged, cur)) return { kind: "noop" };
    return { kind: "write", content: merged };
  } finally {
    await Deno.remove(tmp, { recursive: true }).catch(() => {});
  }
}

/**
 * The baseline delivery diffs and merges against: the immovable origin branch
 * point (originbase), falling back to the rail base for sessions created
 * before originbase refs existed.
 */
async function deliveryBase(dir: string, sessionId: string): Promise<string | null> {
  return (await refSha(dir, originBaseRefFor(sessionId))) ??
    (await refSha(dir, baseRefFor(sessionId)));
}

/**
 * Deliver a session's reviewed edits into the origin's working tree: the
 * originbase..tip diff, limited to `paths`, planned per file first (exact copy
 * or content-level 3-way — see planFile) and written ONLY when every selected
 * path plans cleanly: a conflict throws with the files named and the origin
 * untouched, never half-delivered. The origin's HEAD, index, and branch stay
 * put, so unstaged origin edits merge fine. Already-delivered files (working
 * copy == tip blob) skip the write, so a re-press is a clean no-op. Returns
 * the paths a ship commit should include: everything now carrying the
 * session's content (writes, deletions, and already-delivered skips) but NOT
 * paths whose merge added nothing to a diverged origin copy — committing those
 * would sweep the user's own edits into the session's commit.
 */
export async function materialize(
  dir: string,
  sessionId: string,
  origin: string,
  paths: string[],
): Promise<string[]> {
  await track(dir, sessionId);
  const base = await deliveryBase(dir, sessionId);
  const tip = await refSha(dir, refFor(sessionId));
  if (!base || !tip) return [];
  const all = (await git(dir, ["diff", "--name-only", base, tip]))
    .split("\n").map((s) => s.trim()).filter(Boolean);
  const targets = paths.length > 0 ? all.filter((p) => paths.includes(p)) : all;
  const plan: Array<[string, PlannedFile]> = [];
  const failed: string[] = [];
  for (const p of targets) {
    try {
      plan.push([p, await planFile(dir, base, tip, origin, p)]);
    } catch (e) {
      failed.push(`${p}: ${(e as Error).message.trim().split("\n").at(-1)}`);
    }
  }
  if (failed.length > 0) {
    throw new Error(
      `could not apply (origin untouched; merge base ${base.slice(0, 7)}) ${failed.join("; ")}`,
    );
  }
  const delivered: string[] = [];
  for (const [p, action] of plan) {
    if (action.kind === "noop") continue;
    const target = join(origin, p);
    if (action.kind === "delete") await Deno.remove(target).catch(() => {});
    else if (action.kind === "write") {
      await Deno.mkdir(dirname(target), { recursive: true });
      await Deno.writeFile(target, action.content);
    }
    delivered.push(p);
  }
  return delivered;
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
 * selected paths into the origin's working tree (content-level 3-way against
 * the immovable originbase, so adopted/forked/partially-shipped sessions still
 * deliver their complete change; conflicts throw with the origin untouched),
 * build the commit through a THROWAWAY index seeded from HEAD — the
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
  const base = await deliveryBase(dir, sessionId);
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
    // With an originbase ref, an empty diff really means the tree matches the
    // origin branch point. Legacy sessions diff against the rail base, which
    // accept/adopt/fork-creation move — an empty diff there can hide real work.
    const note = (await refSha(dir, originBaseRefFor(sessionId)))
      ? "nothing to ship: the session tree matches the origin branch point"
      : "nothing to ship: base == tip — this pre-originbase session's base ref may have been " +
        "advanced by a seal, adopt, or fork; inherited work would sit below the base, not be absent";
    return { commit: null, branch, paths: [], pushed: false, note };
  }
  const delivered = await materialize(dir, sessionId, origin, paths);
  if (delivered.length === 0) {
    return {
      commit: null,
      branch,
      paths: [],
      pushed: false,
      note: "nothing to commit: the origin already contains this work merged with its own edits",
    };
  }
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
    await originGit(origin, ["add", "--", ...delivered], env);
    const tree = (await originGit(origin, ["write-tree"], env)).trim();
    if (head && tree === (await originGit(origin, ["rev-parse", "HEAD^{tree}"])).trim()) {
      await accept(dir, sessionId, opts.message);
      return { commit: null, branch, paths: delivered, pushed: false, note: "already committed" };
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
    await originGit(origin, ["add", "--all", "--", ...delivered]);
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
  await accept(dir, sessionId, opts.message);
  if (!opts.push) return { commit, branch, paths: delivered, pushed: false };
  const remote = (await run("git", ["config", `branch.${branch}.remote`], origin)).stdout.trim() ||
    "origin";
  const hasRemote = (await run("git", ["remote"], origin)).stdout.split("\n").includes(remote);
  if (!hasRemote) {
    return { commit, branch, paths, pushed: false, note: `no remote "${remote}" to push to` };
  }
  await originGit(origin, ["push", remote, branch]);
  return { commit, branch, paths: delivered, pushed: true };
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
