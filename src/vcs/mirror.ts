/**
 * Read-only mirror of a guest-owned session's tree: a plain checkout of the
 * session tip under `workspaceDirFor(sid)` — the SAME path the host worktree
 * used to live at, deliberately, so the whole read-side consumer family
 * (@ picker, @file expansion, AGENTS.md, LSP, stdio-MCP cwd) keeps working
 * with zero per-consumer change, at "fresh as of last push" semantics.
 *
 * Refreshed by the store gateway on every received push and by prepareShadow
 * after captureBase (initial mirror = base tree). The host never writes it
 * otherwise, and it is NOT a git worktree — no `.git`, no index, so nothing can
 * mistake it for a writable workspace.
 *
 * v1 strategy: full checkout into a fresh sibling dir, then swap into place
 * (rename old aside, rename new in, remove old). Handles deletions at tip for
 * free and keeps the visible window of a half-populated dir to a single rename.
 */
import { basename, dirname, join } from "node:path";
import { refFor, storeForSession, track, withLock, workspaceDirFor } from "./shadow.ts";
import { pathExists } from "../fsutil.ts";

/** Mirror-side env: user/system git config must not leak (mirrors shadow.ts). */
const ISOLATED = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };

async function git(store: string, args: string[], env?: Record<string, string>): Promise<string> {
  const cmd = new Deno.Command("git", {
    args: [`--git-dir=${store}`, ...args],
    env: { ...ISOLATED, ...env },
    stdout: "piped",
    stderr: "piped",
  });
  const { code, stdout, stderr } = await cmd.output();
  if (code !== 0) {
    throw new Error(
      `git ${args.join(" ")} failed (${code}): ${new TextDecoder().decode(stderr).trim()}`,
    );
  }
  return new TextDecoder().decode(stdout);
}

/**
 * Whether `dir` is a legacy shadow worktree of THIS store: its `.git` is a
 * pointer FILE whose gitdir resolves under `<store>/worktrees/`. Only such a
 * dir is ever retired — anything else with a `.git` stays refused, because
 * the mirror swap deletes recursively.
 */
async function isLegacyWorktreeOf(store: string, dir: string): Promise<boolean> {
  try {
    if (!(await Deno.lstat(join(dir, ".git"))).isFile) return false;
  } catch {
    return false;
  }
  const text = await Deno.readTextFile(join(dir, ".git")).catch(() => "");
  const gitdir = text.match(/^gitdir:\s*(.+?)\s*$/m)?.[1];
  if (!gitdir) return false;
  if (gitdir.startsWith(join(store, "worktrees") + "/")) return true;
  const [rg, rs] = await Promise.all([
    Deno.realPath(gitdir).catch(() => null),
    Deno.realPath(store).catch(() => null),
  ]);
  return rg !== null && rs !== null && rg.startsWith(join(rs, "worktrees") + "/");
}

/** Drop a verified legacy worktree dir + its store-side metadata. Caller holds
 *  the mirror lock and has verified provenance via {@link isLegacyWorktreeOf}. */
async function dropWorktree(store: string, dir: string): Promise<void> {
  await Deno.remove(dir, { recursive: true });
  await git(store, ["worktree", "prune"]).catch(() => {});
}

/**
 * Retire a legacy host worktree squatting on the mirror path — the one-shot
 * cleanup for sessions the startup migration flipped from host-worktree to
 * guest-owned mode (the migration rewrites the workspace COLUMN only; the
 * worktree itself would otherwise block every mirror refresh forever). Edits
 * still sitting in the worktree are flushed to the session ref first (host
 * track()) so nothing is lost, then the dir is removed, worktree metadata
 * pruned, and the mirror seeded in its place. A flush failure aborts (never
 * delete an unflushed tree); a non-worktree `.git` is left alone. Idempotent.
 */
export async function retireLegacyWorktree(sessionId: string): Promise<void> {
  const store = await storeForSession(sessionId);
  const dir = workspaceDirFor(sessionId);
  await withLock(`mirror:${dir}`, async () => {
    if (!(await isLegacyWorktreeOf(store, dir))) return;
    await track(dir, sessionId, "bough: pre-migration flush");
    await dropWorktree(store, dir);
  });
  await refreshMirror(sessionId);
}

/**
 * Rebuild the session's mirror as a checkout of its current tip. No-op when the
 * session ref is gone (pruned store). A leftover legacy worktree of this store
 * at the mirror path (retirement crashed/skipped at migration time) is retired
 * in place — WITHOUT a flush, since by the time a refresh runs the guest tip is
 * the authority; any other `.git` is refused (non-VM mode owns it). Serialized
 * per mirror dir — gateway receives and prepareShadow may overlap.
 */
export async function refreshMirror(sessionId: string): Promise<void> {
  const store = await storeForSession(sessionId);
  const dir = workspaceDirFor(sessionId);
  await withLock(`mirror:${dir}`, async () => {
    if (await pathExists(`${dir}/.git`)) {
      if (!(await isLegacyWorktreeOf(store, dir))) {
        throw new Error(`mirror: ${dir} is a git worktree, not a mirror — refusing to overwrite`);
      }
      console.warn(
        `mirror: retiring leftover legacy worktree at ${dir} (guest tip is authoritative)`,
      );
      await dropWorktree(store, dir);
    }
    const tipR = await new Deno.Command("git", {
      args: [`--git-dir=${store}`, "rev-parse", "--verify", "-q", `${refFor(sessionId)}^{commit}`],
      env: ISOLATED,
      stdout: "piped",
      stderr: "null",
    }).output();
    if (tipR.code !== 0) return; // no session ref — nothing to mirror
    const tip = new TextDecoder().decode(tipR.stdout).trim();
    // Checkout into a fresh sibling via a throwaway index (the store's own index
    // is shared across sessions and must never be touched), then swap.
    const fresh = join(dirname(dir), `.${basename(dir)}.mirror-${crypto.randomUUID().slice(0, 8)}`);
    const idx = await Deno.makeTempFile({ prefix: "bough-mirror-index-" });
    try {
      await Deno.mkdir(fresh, { recursive: true });
      const env = { GIT_WORK_TREE: fresh, GIT_INDEX_FILE: idx };
      await git(store, ["read-tree", tip], env);
      await git(store, ["checkout-index", "-a", "-f"], env);
      const old = `${fresh}.old`;
      const had = await pathExists(dir);
      if (had) await Deno.rename(dir, old);
      await Deno.rename(fresh, dir);
      if (had) await Deno.remove(old, { recursive: true }).catch(() => {});
    } finally {
      await Deno.remove(idx).catch(() => {});
      await Deno.remove(fresh, { recursive: true }).catch(() => {});
    }
  });
}
