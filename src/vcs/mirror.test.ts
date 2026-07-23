/**
 * Mirror tests — host-only, no VM: the read-only checkout under
 * workspaceDirFor(sid) tracks the session tip, including deletions, and never
 * becomes (or clobbers) a git worktree.
 */
import { assert, assertEquals, assertRejects } from "jsr:@std/assert@1";
import * as shadow from "./shadow.ts";
import { refreshMirror, retireLegacyWorktree } from "./mirror.ts";
import { pathExists } from "../fsutil.ts";

async function sh(cwd: string, bin: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command(bin, { args, cwd, stdout: "piped", stderr: "piped" }).output();
  if (r.code !== 0) {
    throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(r.stderr)}`);
  }
  return new TextDecoder().decode(r.stdout);
}

/** A scratch origin repo with one commit and one extra working-tree file. */
async function makeRepo(): Promise<string> {
  const repo = await Deno.makeTempDir({ prefix: "bough-mirror-origin-" });
  await sh(repo, "git", "init", "-q", "-b", "main");
  await sh(repo, "git", "config", "user.name", "t");
  await sh(repo, "git", "config", "user.email", "t@t");
  await Deno.writeTextFile(`${repo}/committed.txt`, "committed\n");
  await sh(repo, "git", "add", "-A");
  await sh(repo, "git", "commit", "-q", "-m", "init");
  await Deno.writeTextFile(`${repo}/untracked.txt`, "untracked\n");
  return repo;
}

/** Temp store/workspace roots + an origin-resolver for `sid`, for the duration of `fn`. */
async function withRoots(
  fn: (repo: string, sid: string) => Promise<void>,
): Promise<void> {
  const shadowBase = await Deno.makeTempDir({ prefix: "bough-mirror-store-" });
  const wsBase = await Deno.makeTempDir({ prefix: "bough-mirror-ws-" });
  Deno.env.set("BOUGH_SHADOW_BASE", shadowBase);
  Deno.env.set("BOUGH_SUBAGENT_BASE", wsBase);
  const repo = await makeRepo();
  const sid = "mirror-s1";
  shadow.setOriginResolver((id) => (id === sid ? repo : null));
  try {
    await fn(repo, sid);
  } finally {
    shadow.setOriginResolver(() => null);
    Deno.env.delete("BOUGH_SHADOW_BASE");
    Deno.env.delete("BOUGH_SUBAGENT_BASE");
    for (const d of [shadowBase, wsBase, repo]) {
      await Deno.remove(d, { recursive: true }).catch(() => {});
    }
  }
}

/** Advance the session ref store-side to the tree of `treeDir` (simulated guest push). */
async function pushTree(store: string, sid: string, treeDir: string): Promise<void> {
  const idx = await Deno.makeTempFile({ prefix: "bough-mirror-test-index-" });
  const env = {
    GIT_DIR: store,
    GIT_WORK_TREE: treeDir,
    GIT_INDEX_FILE: idx,
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_SYSTEM: "/dev/null",
  };
  const g = async (...args: string[]) => {
    const r = await new Deno.Command("git", {
      args,
      cwd: treeDir,
      env,
      stdout: "piped",
      stderr: "piped",
    })
      .output();
    if (r.code !== 0) throw new Error(new TextDecoder().decode(r.stderr));
    return new TextDecoder().decode(r.stdout).trim();
  };
  try {
    await g("read-tree", "--empty");
    await g("add", "-A");
    const tree = await g("write-tree");
    const tip = await g("rev-parse", `refs/bough/sessions/${sid}`);
    const next = await g("commit-tree", tree, "-p", tip, "-m", "test: push");
    await g("update-ref", `refs/bough/sessions/${sid}`, next, tip);
  } finally {
    await Deno.remove(idx).catch(() => {});
  }
}

Deno.test("refreshMirror: base tree, then tip updates incl. deletions; never a worktree", async () => {
  await withRoots(async (repo, sid) => {
    const store = await shadow.createSessionWorkspace(repo, sid, { worktree: false });
    assert(await pathExists(`${store}/bough-origin`), "worktree:false returns the store path");
    const dir = shadow.workspaceDirFor(sid);
    assert(!(await pathExists(dir)), "no worktree was created");

    // Initial mirror = the captured base tree (committed + untracked files).
    await refreshMirror(sid);
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "committed\n");
    assertEquals(await Deno.readTextFile(`${dir}/untracked.txt`), "untracked\n");
    assert(!(await pathExists(`${dir}/.git`)), "mirror is a plain dir, not a worktree");

    // Simulated guest push: edit one file, delete another, add a new one.
    const next = await Deno.makeTempDir({ prefix: "bough-mirror-next-" });
    await Deno.writeTextFile(`${next}/committed.txt`, "edited\n");
    await Deno.writeTextFile(`${next}/new.txt`, "new\n");
    await pushTree(store, sid, next);
    await refreshMirror(sid);
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "edited\n");
    assertEquals(await Deno.readTextFile(`${dir}/new.txt`), "new\n");
    assert(!(await pathExists(`${dir}/untracked.txt`)), "file deleted at tip left the mirror");
    await Deno.remove(next, { recursive: true }).catch(() => {});
  });
});

Deno.test("refreshMirror: refuses a foreign .git at the mirror path", async () => {
  await withRoots(async (repo, sid) => {
    await shadow.createSessionWorkspace(repo, sid, { worktree: false });
    const dir = shadow.workspaceDirFor(sid);
    await Deno.mkdir(dir, { recursive: true });
    // A worktree pointer into some OTHER repo — not this session's store.
    await Deno.writeTextFile(`${dir}/.git`, "gitdir: /nowhere\n");
    await assertRejects(() => refreshMirror(sid), Error, "worktree");
    // Same for a full .git DIRECTORY (a real repo squatting there).
    await Deno.remove(`${dir}/.git`);
    await Deno.mkdir(`${dir}/.git`);
    await assertRejects(() => refreshMirror(sid), Error, "worktree");
    // And retirement never touches either shape.
    await retireLegacyWorktree(sid).catch(() => {});
    assert(await pathExists(`${dir}/.git`), "foreign .git left alone");
  });
});

Deno.test("retireLegacyWorktree: flushes edits, removes the worktree, seeds the mirror", async () => {
  await withRoots(async (repo, sid) => {
    // Legacy (host-worktree mode) shape: a real checked-out worktree at the
    // mirror path — what the startup migration leaves squatting there.
    const dir = await shadow.createSessionWorkspace(repo, sid);
    assertEquals(dir, shadow.workspaceDirFor(sid));
    assert(await pathExists(`${dir}/.git`), "fixture is a real worktree");
    await Deno.writeTextFile(`${dir}/unflushed.txt`, "edit\n");

    await retireLegacyWorktree(sid);

    // Worktree gone; the SAME path is now a plain mirror of the session tip,
    // including the edit that was only on disk (flushed via host track()).
    assert(!(await pathExists(`${dir}/.git`)), "worktree retired");
    assertEquals(await Deno.readTextFile(`${dir}/unflushed.txt`), "edit\n");
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "committed\n");
    const store = await shadow.storeForSession(sid);
    const shown = await sh(
      store,
      "git",
      "show",
      `refs/bough/sessions/${sid}:unflushed.txt`,
    );
    assertEquals(shown, "edit\n");
    // Idempotent, and subsequent refreshes own the path.
    await retireLegacyWorktree(sid);
    await refreshMirror(sid);
    assert(!(await pathExists(`${dir}/.git`)));
  });
});

Deno.test("refreshMirror: retires a leftover legacy worktree of this store (no flush)", async () => {
  await withRoots(async (repo, sid) => {
    // Crash-window fallback: retirement never ran, a refresh arrives (gateway
    // receive). The worktree is verified as THIS store's and dropped as-is —
    // the pushed tip is authoritative by then.
    const dir = await shadow.createSessionWorkspace(repo, sid);
    await Deno.writeTextFile(`${dir}/stale.txt`, "stale\n"); // never flushed
    await refreshMirror(sid);
    assert(!(await pathExists(`${dir}/.git`)), "leftover worktree retired");
    assert(!(await pathExists(`${dir}/stale.txt`)), "unflushed edit dropped — tip wins");
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "committed\n");
  });
});

Deno.test("refreshMirror: no session ref is a no-op", async () => {
  await withRoots(async (repo, _sid) => {
    // Resolver knows this sid's origin but no session refs exist in the store.
    const other = "mirror-ghost";
    shadow.setOriginResolver(() => repo);
    await shadow.storeForSession(other); // ensure resolution works
    // Store may not even exist yet; create it via a real session, then ask for the ghost.
    await shadow.createSessionWorkspace(repo, "mirror-real", { worktree: false });
    await refreshMirror(other);
    assert(!(await pathExists(shadow.workspaceDirFor(other))), "nothing mirrored");
  });
});
