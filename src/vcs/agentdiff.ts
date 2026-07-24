/**
 * agentfs-diff snapshots — the phase-5 replacement for the shadow-git store as the
 * diff/snapshot substrate. Every sandboxed session's edits live in an
 * `agentfs run --session <id>` copy-on-write delta, NOT in the on-disk worktree
 * (the file tools write through the overlay; see sandbox/agentfs.ts). So the
 * session's change set is exactly `agentfs diff <delta.db>` — the delta measured
 * against the session's base dir (the host worktree the overlay copies-on-write).
 *
 * Model (mirrors what the shadow store used to provide, minus git):
 *   - BASE   = the session's worktree files on disk. agentfs never writes them, so
 *              they stay pristine at the branch point — the merge base for delivery.
 *   - TIP    = base + delta, read back per file with `agentfs fs <db> cat <path>`
 *              (a direct db read; the whole file is copied-up on first write, so the
 *              cat is the full tip content, binary-safe on stdout).
 *   - CHANGE = `agentfs diff` A/M/D lines. macOS NFS leaves AppleDouble `._*` files
 *              and directory (`d`) rows in the raw output — both are filtered here.
 *
 * Delivery into the user's checkout (materialize/ship) is a content-level 3-way
 * (base/ours=origin/theirs=tip) identical in spirit to the shadow path, so a user's
 * concurrent edits merge rather than clobber. Sealing a change (accept, or the tail
 * of a ship) FOLDS the delta into the base worktree and resets the delta, so the
 * base now carries the work, the next turn overlays it, and `agentfs diff` clears.
 *
 * Locking: `agentfs run` holds an EXCLUSIVE lock on the delta db for the life of the
 * run, and `agentfs diff`/`agentfs fs` are locked out while it does. Everything here
 * runs on the HOST between tool invocations (rail reads, apply/revert, the ship host
 * fn) — never concurrently with a live run in the same session.
 */
import { dirname, isAbsolute, join, relative } from "node:path";
import { type Diff, type FileDiff, parseGitDiff } from "../schema/changes.ts";
import { pathExists } from "../fsutil.ts";
import * as agentfs from "../sandbox/agentfs.ts";

interface RunResult {
  ok: boolean;
  code: number;
  stdout: Uint8Array;
  stderr: string;
}

async function run(bin: string, args: string[], stdin?: Uint8Array): Promise<RunResult> {
  const cmd = new Deno.Command(bin, {
    args,
    stdin: stdin === undefined ? "null" : "piped",
    stdout: "piped",
    stderr: "piped",
  });
  const child = cmd.spawn();
  if (stdin !== undefined) {
    const w = child.stdin.getWriter();
    await w.write(stdin);
    await w.close();
  }
  const { code, stdout, stderr } = await child.output();
  return { ok: code === 0, code, stdout, stderr: new TextDecoder().decode(stderr) };
}

/** The session's delta db path — the same `~/.agentfs/run/<id>/delta.db` the runs
 *  create (agentfs keys the run dir off HOME, which the runs inherit). */
export function deltaDbPath(sessionId: string): string {
  const home = Deno.env.get("HOME");
  if (!home) throw new Error("agentdiff: HOME is unset");
  return `${home}/.agentfs/run/${sessionId}/delta.db`;
}

/** The run dir holding a session's delta (removed to reset the overlay). */
function runDir(sessionId: string): string {
  return dirname(deltaDbPath(sessionId));
}

/** Whether the session has an on-disk delta yet (a first turn that ran no tool has none). */
export async function hasDelta(sessionId: string): Promise<boolean> {
  return await pathExists(deltaDbPath(sessionId));
}

type Status = "added" | "modified" | "deleted";
interface Entry {
  status: Status;
  path: string;
}

/** AppleDouble sidecar (`._foo`) left by the macOS NFS mount — never real content. */
function isAppleDouble(path: string): boolean {
  return path.split("/").some((seg) => seg.startsWith("._"));
}

/**
 * Parse `agentfs diff` stdout into per-FILE change entries. Each line is
 * `<A|M|D> <f|d> /<path>`; directory rows and AppleDouble sidecars are dropped, and
 * the leading slash is stripped so paths are base-relative (matching the tool seam).
 */
export function parseAgentfsDiff(stdout: string): Entry[] {
  const out: Entry[] = [];
  for (const line of stdout.split("\n")) {
    const m = /^([AMD]) ([fd]) \/(.*)$/.exec(line.trimEnd());
    if (!m) continue;
    const [, sc, kind, path] = m;
    if (kind !== "f" || isAppleDouble(path) || path === "") continue;
    out.push({ status: sc === "A" ? "added" : sc === "M" ? "modified" : "deleted", path });
  }
  return out;
}

/** The raw, filtered change set of a session's delta (base..tip), file rows only. */
export async function changedEntries(sessionId: string): Promise<Entry[]> {
  if (!(await hasDelta(sessionId))) return [];
  const r = await run(agentfs.bin(), ["diff", deltaDbPath(sessionId)]);
  if (!r.ok) {
    throw new Error(`agentfs diff failed (${r.code}): ${r.stderr.trim().split("\n").at(-1)}`);
  }
  return parseAgentfsDiff(new TextDecoder().decode(r.stdout));
}

/** A path's TIP bytes (base + delta) read from the delta db, or null if absent there
 *  (a base-only file the session never touched, or one it deleted). */
async function tipBytes(sessionId: string, path: string): Promise<Uint8Array | null> {
  const r = await run(agentfs.bin(), ["fs", deltaDbPath(sessionId), "cat", path]);
  return r.ok ? r.stdout : null;
}

/** A path's BASE bytes — the pristine worktree file the overlay copies-on-write. */
async function baseBytes(baseDir: string, path: string): Promise<Uint8Array | null> {
  try {
    return await Deno.readFile(join(baseDir, path));
  } catch {
    return null;
  }
}

/**
 * The structured diff of a session's change (base..tip). Reads the change set from
 * `agentfs diff`, then reconstructs hunks by laying the base and tip content of each
 * changed file into two throwaway trees and running one `git diff --no-index` over
 * them (the same rendering clonefile uses). Missing delta → empty diff. Tagged
 * `source: "shadow"` so the Changes rail's apply/revert routing is unchanged.
 */
export async function diff(baseDir: string, sessionId: string): Promise<Diff> {
  const entries = await changedEntries(sessionId).catch(() => [] as Entry[]);
  if (entries.length === 0) return { source: "shadow", files: [] };

  const tmp = await Deno.makeTempDir({ prefix: "bough-agentdiff-" });
  try {
    const baseTree = join(tmp, "base");
    const tipTree = join(tmp, "tip");
    // Both roots must exist even when a side is empty (all-added → no base files,
    // all-deleted → no tip files): `git diff --no-index` errors on a missing path.
    await Deno.mkdir(baseTree, { recursive: true });
    await Deno.mkdir(tipTree, { recursive: true });
    for (const e of entries) {
      if (e.status !== "added") {
        const b = await baseBytes(baseDir, e.path);
        if (b) await writeUnder(baseTree, e.path, b);
      }
      if (e.status !== "deleted") {
        const t = await tipBytes(sessionId, e.path);
        if (t) await writeUnder(tipTree, e.path, t);
      }
    }
    const r = await run("git", ["diff", "--no-index", "--no-color", baseTree, tipTree]);
    if (r.code > 1) {
      throw new Error(`git diff --no-index failed (${r.code}): ${r.stderr.trim()}`);
    }
    // git reports the b-side path; strip whichever tree prefix it carries (tip for
    // add/modify, base for delete) back to the base-relative path.
    const baseNo = baseTree.replace(/^\//, "") + "/";
    const tipNo = tipTree.replace(/^\//, "") + "/";
    const strip = (p: string): string =>
      p.startsWith(tipNo)
        ? p.slice(tipNo.length)
        : p.startsWith(baseNo)
        ? p.slice(baseNo.length)
        : p;
    const files: FileDiff[] = parseGitDiff(
      r.stdout.length ? new TextDecoder().decode(r.stdout) : "",
      strip,
    );
    return { source: "shadow", files };
  } finally {
    await Deno.remove(tmp, { recursive: true }).catch(() => {});
  }
}

async function writeUnder(root: string, rel: string, bytes: Uint8Array): Promise<void> {
  const target = join(root, rel);
  await Deno.mkdir(dirname(target), { recursive: true });
  await Deno.writeFile(target, bytes);
}

// ---- delivery into the user's checkout (materialize / ship) -----------------

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

type PlannedFile =
  | { kind: "skip" } // origin already has the tip content — nothing to write
  | { kind: "noop" } // origin diverged but the merge added nothing — leave it, don't commit
  | { kind: "write"; content: Uint8Array }
  | { kind: "delete" };

/**
 * Plan one file's base→tip delivery into the origin's current copy: exact-copy fast
 * path when the origin still matches the base, else a content-level 3-way via
 * `git merge-file` (base / ours=origin / theirs=tip). Conflicts throw; nothing is
 * written here — materialize writes only once every selected path plans cleanly.
 */
async function planFile(
  base: Uint8Array | null,
  tip: Uint8Array | null,
  origin: string,
  path: string,
): Promise<PlannedFile> {
  const cur = await Deno.readFile(join(origin, path)).catch(() => null);
  if (!tip) {
    // Deleted in the session.
    if (cur === null) return { kind: "skip" };
    if (base && bytesEqual(cur, base)) return { kind: "delete" };
    throw new Error("deleted in session but modified in origin");
  }
  if (cur !== null && bytesEqual(cur, tip)) return { kind: "skip" };
  if (cur === null || (base && bytesEqual(cur, base))) return { kind: "write", content: tip };
  if (!base) throw new Error("added in session but a different file exists in origin");
  const tmp = await Deno.makeTempDir({ prefix: "bough-merge-" });
  try {
    await Deno.writeFile(`${tmp}/base`, base);
    await Deno.writeFile(`${tmp}/ours`, cur);
    await Deno.writeFile(`${tmp}/theirs`, tip);
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
    ]);
    if (r.code !== 0) throw new Error("conflicts with origin edits");
    const merged = r.stdout;
    if (bytesEqual(merged, cur)) return { kind: "noop" };
    return { kind: "write", content: merged };
  } finally {
    await Deno.remove(tmp, { recursive: true }).catch(() => {});
  }
}

/**
 * Deliver a session's reviewed edits into the origin's working tree: plan every
 * selected path first (exact copy or content-level 3-way), and WRITE only when all
 * plan cleanly — a conflict throws with the origin untouched, never half-delivered.
 * Returns the paths a commit should include (writes + deletes + already-present
 * skips, but NOT merges that added nothing to a diverged origin copy).
 */
export async function materialize(
  baseDir: string,
  sessionId: string,
  origin: string,
  paths: string[],
): Promise<string[]> {
  const entries = await changedEntries(sessionId);
  const targets = paths.length > 0 ? entries.filter((e) => paths.includes(e.path)) : entries;
  const plan: Array<[string, PlannedFile]> = [];
  const failed: string[] = [];
  for (const e of targets) {
    try {
      const base = e.status === "added" ? null : await baseBytes(baseDir, e.path);
      const tip = e.status === "deleted" ? null : await tipBytes(sessionId, e.path);
      plan.push([e.path, await planFile(base, tip, origin, e.path)]);
    } catch (err) {
      failed.push(`${e.path}: ${(err as Error).message.trim().split("\n").at(-1)}`);
    }
  }
  if (failed.length > 0) {
    throw new Error(`could not apply (origin untouched) ${failed.join("; ")}`);
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
  commit: string | null;
  branch: string;
  paths: string[];
  pushed: boolean;
  note?: string;
}

/** Run git in the origin with the user's config intact; throws on non-zero exit. */
async function originGit(
  cwd: string,
  args: string[],
  env?: Record<string, string>,
): Promise<string> {
  const { code, stdout, stderr } = await new Deno.Command("git", {
    args,
    cwd,
    env,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (code !== 0) {
    throw new Error(
      `git ${args.join(" ")} failed (${code}): ${new TextDecoder().decode(stderr).trim()}`,
    );
  }
  return new TextDecoder().decode(stdout);
}

async function tryGit(
  cwd: string,
  args: string[],
  env?: Record<string, string>,
): Promise<RunResult & { text: string }> {
  const { code, stdout, stderr } = await new Deno.Command("git", {
    args,
    cwd,
    env,
    stdout: "piped",
    stderr: "piped",
  }).output();
  return {
    ok: code === 0,
    code,
    stdout,
    stderr: new TextDecoder().decode(stderr),
    text: new TextDecoder().decode(stdout),
  };
}

/**
 * Ship a session's work into the origin as a real commit: materialize the selected
 * paths (content-level 3-way against the base worktree), build the commit through a
 * THROWAWAY index seeded from HEAD (the user's own index is never touched), advance
 * the origin's current branch, seal the session (fold the delta into the base), and
 * optionally `git push`. Refuses a detached-HEAD origin. Same contract/shape as the
 * shadow ship it replaces; the content just comes from the agentfs delta now.
 */
export async function shipToOrigin(
  baseDir: string,
  sessionId: string,
  origin: string,
  opts: { message: string; paths?: string[]; push?: boolean },
): Promise<ShipResult> {
  const branchR = await tryGit(origin, ["symbolic-ref", "--short", "-q", "HEAD"]);
  if (!branchR.ok) {
    throw new Error("ship: the origin checkout is on a detached HEAD — check out a branch first");
  }
  const branch = branchR.text.trim();
  const entries = await changedEntries(sessionId);
  const all = entries.map((e) => e.path);
  const paths = opts.paths && opts.paths.length > 0
    ? all.filter((p) => opts.paths!.includes(p))
    : all;
  if (paths.length === 0) {
    return {
      commit: null,
      branch,
      paths: [],
      pushed: false,
      note: "nothing to ship: the session made no tracked changes",
    };
  }
  const delivered = await materialize(baseDir, sessionId, origin, paths);
  if (delivered.length === 0) {
    return {
      commit: null,
      branch,
      paths: [],
      pushed: false,
      note: "nothing to commit: the origin already contains this work merged with its own edits",
    };
  }
  const idx = await Deno.makeTempFile({ prefix: "bough-ship-index-" });
  let commit: string | null = null;
  try {
    const env = { GIT_INDEX_FILE: idx };
    const headR = await tryGit(origin, ["rev-parse", "--verify", "-q", "HEAD"], env);
    const head = headR.ok ? headR.text.trim() : null;
    await originGit(origin, head ? ["read-tree", "HEAD"] : ["read-tree", "--empty"], env);
    await originGit(origin, ["add", "--", ...delivered], env);
    const tree = (await originGit(origin, ["write-tree"], env)).trim();
    if (head && tree === (await originGit(origin, ["rev-parse", "HEAD^{tree}"])).trim()) {
      await accept(baseDir, sessionId);
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
    // Sync only the shipped paths into the real index so the advanced HEAD doesn't
    // read them as phantom staged deletions in `git status`.
    await originGit(origin, ["add", "--all", "--", ...delivered]);
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
  await accept(baseDir, sessionId);
  if (!opts.push) return { commit, branch, paths: delivered, pushed: false };
  const remote = (await tryGit(origin, ["config", `branch.${branch}.remote`])).text.trim() ||
    "origin";
  const remotes = (await tryGit(origin, ["remote"])).text.split("\n");
  if (!remotes.includes(remote)) {
    return {
      commit,
      branch,
      paths: delivered,
      pushed: false,
      note: `no remote "${remote}" to push to`,
    };
  }
  await originGit(origin, ["push", remote, branch]);
  return { commit, branch, paths: delivered, pushed: true };
}

// ---- PR output: export the delta into a branch + open a GitHub PR -----------

export interface PrResult {
  /** The new branch the session's work was committed onto (empty when nothing to do). */
  branch: string;
  /** The base branch the PR targets. */
  base: string;
  /** The commit sha on the new branch, or null when there was nothing to commit. */
  commit: string | null;
  /** Paths included in the commit. */
  paths: string[];
  /** True when the branch was pushed to its remote. */
  pushed: boolean;
  /** The opened PR's URL (present only when `gh pr create` succeeded). */
  url?: string;
  /** Human-readable caveat (nothing to ship, no remote, gh failed, …). */
  note?: string;
}

/** Write bytes as a blob into the origin's object store; returns the blob sha. */
async function hashObject(origin: string, bytes: Uint8Array): Promise<string> {
  const child = new Deno.Command("git", {
    args: ["hash-object", "-w", "--stdin"],
    cwd: origin,
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const w = child.stdin.getWriter();
  await w.write(bytes);
  await w.close();
  const { code, stdout, stderr } = await child.output();
  if (code !== 0) {
    throw new Error(`git hash-object failed (${code}): ${new TextDecoder().decode(stderr).trim()}`);
  }
  return new TextDecoder().decode(stdout).trim();
}

/** A path's file mode in HEAD (`100755` for an executable), or `100644` if absent. */
async function headMode(origin: string, path: string): Promise<string> {
  const r = await tryGit(origin, ["ls-tree", "HEAD", "--", path]);
  const m = /^(\d{6}) /.exec(r.text);
  return m ? m[1] : "100644";
}

/** Slug a title into a git-safe branch name suffixed with the session's short id. */
function defaultBranchName(sessionId: string, title: string): string {
  const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "")
    .slice(0, 40) || "session";
  return `bough/${slug}-${sessionId.slice(0, 8)}`;
}

/**
 * Export a session's work into a real git branch and open a GitHub PR for it.
 *
 * Unlike shipToOrigin (which materializes into the user's working tree and advances
 * the current branch), this NEVER touches the origin checkout: the commit is built
 * purely in git object-land — a throwaway index seeded from HEAD, the session's tip
 * bytes hashed straight into the object store — so the user's working copy and index
 * stay exactly as they were. The new branch is committed on top of HEAD, pushed to
 * the base branch's remote (default `origin`), and a PR is opened against `base`
 * (default the origin's current branch) via `gh pr create` using the host gh auth.
 * The session is sealed (delta folded into the base) once the commit exists, so the
 * Changes rail clears just as it does after a ship. A missing remote or a `gh`
 * failure is reported in `note` with the branch/commit still returned — the work is
 * captured either way.
 */
export async function openPr(
  baseDir: string,
  sessionId: string,
  origin: string,
  opts: {
    title: string;
    body?: string;
    branch?: string;
    base?: string;
    paths?: string[];
    draft?: boolean;
  },
): Promise<PrResult> {
  const headBranchR = await tryGit(origin, ["symbolic-ref", "--short", "-q", "HEAD"]);
  const base = opts.base?.trim() || (headBranchR.ok ? headBranchR.text.trim() : "");
  if (!base) {
    throw new Error(
      "pr: the origin checkout is on a detached HEAD — pass base or check out a branch",
    );
  }
  const headR = await tryGit(origin, ["rev-parse", "--verify", "-q", "HEAD"]);
  if (!headR.ok) throw new Error("pr: the origin has no commits — cannot open a PR");
  const head = headR.text.trim();

  const entries = await changedEntries(sessionId);
  const all = entries.map((e) => e.path);
  const selected = opts.paths && opts.paths.length > 0
    ? all.filter((p) => opts.paths!.includes(p))
    : all;
  if (selected.length === 0) {
    return {
      branch: "",
      base,
      commit: null,
      paths: [],
      pushed: false,
      note: "nothing to open a PR for: the session made no tracked changes",
    };
  }
  const pick = new Set(selected);
  const targets = entries.filter((e) => pick.has(e.path));

  const branch = opts.branch?.trim() || defaultBranchName(sessionId, opts.title);
  if ((await tryGit(origin, ["rev-parse", "--verify", "-q", `refs/heads/${branch}`])).ok) {
    throw new Error(`pr: branch "${branch}" already exists — pass a different branch name`);
  }

  // Build the commit in a throwaway index seeded from HEAD; the working tree and the
  // user's real index are never read or written.
  const idx = await Deno.makeTempFile({ prefix: "bough-pr-index-" });
  let commit: string;
  try {
    const env = { GIT_INDEX_FILE: idx };
    await originGit(origin, ["read-tree", "HEAD"], env);
    for (const e of targets) {
      if (e.status === "deleted") {
        await originGit(origin, ["update-index", "--force-remove", "--", e.path], env);
        continue;
      }
      const tip = await tipBytes(sessionId, e.path);
      if (!tip) continue; // vanished from the delta — nothing to add
      const sha = await hashObject(origin, tip);
      const mode = e.status === "modified" ? await headMode(origin, e.path) : "100644";
      await originGit(
        origin,
        ["update-index", "--add", "--cacheinfo", `${mode},${sha},${e.path}`],
        env,
      );
    }
    const tree = (await originGit(origin, ["write-tree"], env)).trim();
    if (tree === (await originGit(origin, ["rev-parse", "HEAD^{tree}"])).trim()) {
      return {
        branch: "",
        base,
        commit: null,
        paths: [],
        pushed: false,
        note: "nothing to open a PR for: the change is already in HEAD",
      };
    }
    const msg = opts.body?.trim() ? `${opts.title}\n\n${opts.body.trim()}\n` : `${opts.title}\n`;
    commit = (await originGit(origin, ["commit-tree", tree, "-p", head, "-m", msg], env)).trim();
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
  await originGit(origin, ["update-ref", `refs/heads/${branch}`, commit]);
  await accept(baseDir, sessionId);

  const remote = (await tryGit(origin, ["config", `branch.${base}.remote`])).text.trim() ||
    "origin";
  const remotes = (await tryGit(origin, ["remote"])).text.split("\n").map((s) => s.trim());
  if (!remotes.includes(remote)) {
    return {
      branch,
      base,
      commit,
      paths: selected,
      pushed: false,
      note: `no remote "${remote}" to push to — branch "${branch}" created locally`,
    };
  }
  await originGit(origin, ["push", remote, `refs/heads/${branch}:refs/heads/${branch}`]);

  const ghArgs = [
    "pr",
    "create",
    "--head",
    branch,
    "--base",
    base,
    "--title",
    opts.title,
    "--body",
    opts.body ?? "",
  ];
  if (opts.draft) ghArgs.push("--draft");
  const gh = await new Deno.Command("gh", {
    args: ghArgs,
    cwd: origin,
    stdout: "piped",
    stderr: "piped",
  })
    .output();
  if (gh.code !== 0) {
    return {
      branch,
      base,
      commit,
      paths: selected,
      pushed: true,
      note: `branch pushed but \`gh pr create\` failed: ${
        new TextDecoder().decode(gh.stderr).trim().split("\n").at(-1)
      }`,
    };
  }
  const url = new TextDecoder().decode(gh.stdout).trim().split("\n").at(-1) ?? "";
  return { branch, base, commit, paths: selected, pushed: true, url: url || undefined };
}

// ---- sealing / revert (delta lifecycle) -------------------------------------

/** Remove a session's on-disk delta so the next `agentfs run` rebuilds it fresh from
 *  the (possibly updated) base worktree. */
async function resetDelta(sessionId: string): Promise<void> {
  await Deno.remove(runDir(sessionId), { recursive: true }).catch(() => {});
}

/**
 * Fold the delta's changes into the base worktree on disk (writes + deletes), then
 * reset the delta. After this the base carries the work, the next turn overlays it,
 * and `agentfs diff` shows a clean slate — the analogue of the shadow store advancing
 * base onto tip.
 */
export async function accept(baseDir: string, sessionId: string): Promise<void> {
  const entries = await changedEntries(sessionId).catch(() => [] as Entry[]);
  for (const e of entries) {
    const target = join(baseDir, e.path);
    if (e.status === "deleted") {
      await Deno.remove(target).catch(() => {});
    } else {
      const t = await tipBytes(sessionId, e.path);
      if (t) {
        await Deno.mkdir(dirname(target), { recursive: true });
        await Deno.writeFile(target, t);
      }
    }
  }
  await resetDelta(sessionId);
}

/**
 * Whole-change revert: discard the session's entire delta so every edit returns to
 * base. Returns the paths that were reverted. The base worktree is untouched.
 */
export async function undoAll(_baseDir: string, sessionId: string): Promise<string[]> {
  const entries = await changedEntries(sessionId).catch(() => [] as Entry[]);
  const paths = entries.map((e) => e.path);
  await resetDelta(sessionId);
  return paths;
}

/**
 * Per-path revert: restore ONLY `paths` back to base while keeping every other edit.
 * agentfs has no per-path delta erase, so this captures the tip of the KEPT paths,
 * resets the whole delta, then replays the kept paths through the overlay. No-op on
 * an empty list.
 */
export async function revertPaths(
  baseDir: string,
  sessionId: string,
  paths: string[],
): Promise<void> {
  if (paths.length === 0) return;
  const entries = await changedEntries(sessionId);
  const revert = new Set(paths);
  const keep = entries.filter((e) => !revert.has(e.path));
  // Capture kept tips BEFORE the reset wipes the delta.
  const replay: Array<{ path: string; content: Uint8Array | null; status: Status }> = [];
  for (const e of keep) {
    const content = e.status === "deleted" ? null : await tipBytes(sessionId, e.path);
    replay.push({ path: e.path, content, status: e.status });
  }
  await resetDelta(sessionId);
  if (replay.length === 0) return;
  agentfs.ensure(sessionId, { origin: baseDir });
  for (const r of replay) {
    if (r.status === "deleted") {
      await agentfs.execIn(sessionId, ["/bin/rm", "-f", overlayRel(baseDir, r.path)], {
        cwd: baseDir,
      });
    } else if (r.content) {
      await agentfs.writeFile(sessionId, join(baseDir, r.path), r.content);
    }
  }
}

/**
 * Adopt a subagent's changes into its spawner: fold the sub's delta (base..tip) into
 * the SPAWNER's delta by replaying each changed path through the spawner overlay
 * (writes + deletes), then seal the sub so its own rail clears (fold into the sub's
 * base, reset its delta) while its worktree stays continuable. The shadow analogue of
 * `jj squash --keep-emptied`. No-op when the sub made no changes.
 */
export async function adopt(
  spawnerBaseDir: string,
  spawnerSessionId: string,
  subBaseDir: string,
  subSessionId: string,
): Promise<void> {
  const entries = await changedEntries(subSessionId);
  if (entries.length === 0) return;
  agentfs.ensure(spawnerSessionId, { origin: spawnerBaseDir });
  for (const e of entries) {
    const dst = join(spawnerBaseDir, e.path);
    if (e.status === "deleted") {
      await agentfs.execIn(
        spawnerSessionId,
        ["/bin/rm", "-f", overlayRel(spawnerBaseDir, e.path)],
        {
          cwd: spawnerBaseDir,
        },
      );
    } else {
      const t = await tipBytes(subSessionId, e.path);
      if (t) await agentfs.writeFile(spawnerSessionId, dst, t);
    }
  }
  await accept(subBaseDir, subSessionId);
}

/** Base-relative path for a run command (paths land in the delta only when relative). */
function overlayRel(baseDir: string, path: string): string {
  if (!isAbsolute(path)) return path;
  const rel = relative(baseDir, path);
  return rel === "" ? "." : rel.startsWith("..") ? path : rel;
}
