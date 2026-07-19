/**
 * Shadow-git backend tests — the full session lifecycle against real git repos
 * in temp dirs: capture, track, diff, fork/subagent worktrees, adopt,
 * materialize (incl. 3-way), per-path and whole-change revert, accept, and the
 * origin-untouched invariant that is the whole point.
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import * as shadow from "./shadow.ts";

async function sh(cwd: string, bin: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command(bin, { args, cwd, stdout: "piped", stderr: "piped" }).output();
  if (r.code !== 0) {
    throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(r.stderr)}`);
  }
  return new TextDecoder().decode(r.stdout);
}

/** A scratch git repo with one commit, an uncommitted edit, and an untracked file. */
async function makeRepo(): Promise<string> {
  const repo = await Deno.makeTempDir({ prefix: "bough-shadow-origin-" });
  await sh(repo, "git", "init", "-q", "-b", "main");
  await sh(repo, "git", "config", "user.name", "t");
  await sh(repo, "git", "config", "user.email", "t@t");
  await Deno.writeTextFile(`${repo}/committed.txt`, "committed\n");
  await Deno.writeTextFile(`${repo}/.gitignore`, "ignored.txt\n");
  await sh(repo, "git", "add", "-A");
  await sh(repo, "git", "commit", "-q", "-m", "init");
  await Deno.writeTextFile(`${repo}/committed.txt`, "committed+dirty\n"); // uncommitted edit
  await Deno.writeTextFile(`${repo}/untracked.txt`, "untracked\n");
  await Deno.writeTextFile(`${repo}/ignored.txt`, "ignored\n");
  return repo;
}

/** Point the store + workspace roots into temp dirs for the duration of `fn`. */
async function withRoots(fn: () => Promise<void>): Promise<void> {
  const shadowBase = await Deno.makeTempDir({ prefix: "bough-shadow-store-" });
  const wsBase = await Deno.makeTempDir({ prefix: "bough-shadow-ws-" });
  Deno.env.set("BOUGH_SHADOW_BASE", shadowBase);
  Deno.env.set("BOUGH_SUBAGENT_BASE", wsBase);
  try {
    await fn();
  } finally {
    Deno.env.delete("BOUGH_SHADOW_BASE");
    Deno.env.delete("BOUGH_SUBAGENT_BASE");
    await Deno.remove(shadowBase, { recursive: true }).catch(() => {});
    await Deno.remove(wsBase, { recursive: true }).catch(() => {});
  }
}

async function originState(repo: string): Promise<string> {
  const head = await sh(repo, "git", "rev-parse", "HEAD");
  const status = await sh(repo, "git", "status", "--porcelain");
  return head + status;
}

Deno.test("createSessionWorkspace: inherits dirty tree, never touches origin", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const before = await originState(repo);
    const dir = await shadow.createSessionWorkspace(repo, "s1");
    assertEquals(dir, shadow.workspaceDirFor("s1"));
    // The worktree carries committed, uncommitted, and untracked state...
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "committed+dirty\n");
    assertEquals(await Deno.readTextFile(`${dir}/untracked.txt`), "untracked\n");
    // ...but not ignored files, and no bough refs/branches in the origin.
    assertEquals(await Deno.stat(`${dir}/ignored.txt`).catch(() => null), null);
    assertEquals(await originState(repo), before);
    assertEquals((await sh(repo, "git", "branch", "--list", "bough/*")).trim(), "");
    // Idempotent.
    assertEquals(await shadow.createSessionWorkspace(repo, "s1"), dir);
  });
});

Deno.test("track + diff: change-vs-base, empty when untouched, ignores ignored", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s2");
    assertEquals((await shadow.diff(dir, "s2")).files, []);
    await Deno.writeTextFile(`${dir}/new.txt`, "hello\n");
    await Deno.writeTextFile(`${dir}/committed.txt`, "committed+edited\n");
    await Deno.writeTextFile(`${dir}/ignored.txt`, "still ignored\n");
    const d = await shadow.diff(dir, "s2");
    assertEquals(d.source, "shadow");
    assertEquals(d.files.map((f) => [f.path, f.status]).sort(), [
      ["committed.txt", "modified"],
      ["new.txt", "added"],
    ]);
    // Same tree → same tip (no empty snapshot commits).
    const t1 = await shadow.track(dir, "s2");
    const t2 = await shadow.track(dir, "s2");
    assertEquals(t1, t2);
  });
});

Deno.test("session worktree sees the origin's real git history", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s-hist");
    // The base grafts onto the origin's HEAD (alternates), so log/blame reach
    // the repo's actual commits from inside the session worktree.
    const log = await sh(dir, "git", "log", "--format=%s");
    assertStringIncludes(log, "bough: session base");
    assertStringIncludes(log, "init");
    const blame = await sh(dir, "git", "blame", "--line-porcelain", "HEAD", "--", ".gitignore");
    assertStringIncludes(blame, "summary init");
    // History under the base must not leak into the changes rail.
    assertEquals((await shadow.diff(dir, "s-hist")).files, []);
  });
});

Deno.test("non-git origin dir works identically", async () => {
  await withRoots(async () => {
    const origin = await Deno.makeTempDir({ prefix: "bough-shadow-plain-" });
    await Deno.writeTextFile(`${origin}/notes.md`, "hi\n");
    const dir = await shadow.createSessionWorkspace(origin, "s3");
    assertEquals(await Deno.readTextFile(`${dir}/notes.md`), "hi\n");
    await Deno.writeTextFile(`${dir}/notes.md`, "hi\nedited\n");
    const d = await shadow.diff(dir, "s3");
    assertEquals(d.files.map((f) => f.path), ["notes.md"]);
    assertEquals(await shadow.originRepo(dir), await Deno.realPath(origin));
  });
});

Deno.test("addWorkspace: child branches off parent tip and diverges", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const parent = await shadow.createSessionWorkspace(repo, "p1");
    await Deno.writeTextFile(`${parent}/parent-work.txt`, "from parent\n");
    const child = await shadow.addWorkspace(parent, "c1", shadow.workspaceDirFor("c1"), "p1");
    // Inherits the parent's ON-DISK work (tracked at spawn), then diverges.
    assertEquals(await Deno.readTextFile(`${child}/parent-work.txt`), "from parent\n");
    await Deno.writeTextFile(`${child}/child-work.txt`, "from child\n");
    const cd = await shadow.diff(child, "c1");
    assertEquals(cd.files.map((f) => f.path), ["child-work.txt"]); // parent work is base, not diff
    const pd = await shadow.diff(parent, "p1");
    assertEquals(pd.files.map((f) => f.path), ["parent-work.txt"]); // parent unaffected by child
  });
});

Deno.test("adoptChanges: subagent work lands in parent worktree, sub stays alive", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const parent = await shadow.createSessionWorkspace(repo, "p2");
    const sub = await shadow.addWorkspace(parent, "sub2", shadow.workspaceDirFor("sub2"), "p2");
    await Deno.writeTextFile(`${sub}/sub-file.txt`, "sub result\n");
    await Deno.writeTextFile(`${sub}/committed.txt`, "committed+sub\n");
    await shadow.adoptChanges(parent, sub, "sub2", "p2");
    assertEquals(await Deno.readTextFile(`${parent}/sub-file.txt`), "sub result\n");
    assertEquals(await Deno.readTextFile(`${parent}/committed.txt`), "committed+sub\n");
    // The parent's diff now includes the adopted work; the sub's rail clears
    // (its base advanced) but its files stay on disk — branch continuable.
    const pd = await shadow.diff(parent, "p2");
    assert(pd.files.some((f) => f.path === "sub-file.txt"));
    assertEquals((await shadow.diff(sub, "sub2")).files, []);
    assertEquals(await Deno.readTextFile(`${sub}/sub-file.txt`), "sub result\n");
  });
});

Deno.test("materialize: delivers to origin worktree only; re-press no-ops; 3-way merges", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s4");
    await Deno.writeTextFile(`${dir}/new.txt`, "delivered\n");
    await Deno.writeTextFile(`${dir}/committed.txt`, "committed+dirty\nsession line\n");
    // User edits the same file in origin AFTER the session branched (3-way case).
    await Deno.writeTextFile(`${repo}/committed.txt`, "user line\ncommitted+dirty\n");
    const headBefore = await sh(repo, "git", "rev-parse", "HEAD");
    await shadow.materialize(dir, "s4", repo, []);
    assertEquals(await Deno.readTextFile(`${repo}/new.txt`), "delivered\n");
    const merged = await Deno.readTextFile(`${repo}/committed.txt`);
    assertStringIncludes(merged, "user line");
    assertStringIncludes(merged, "session line");
    assertEquals(await sh(repo, "git", "rev-parse", "HEAD"), headBefore); // HEAD untouched
    await shadow.materialize(dir, "s4", repo, []); // idempotent re-press
  });
});

Deno.test("revertPaths: only the named paths return to base", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s5");
    await Deno.writeTextFile(`${dir}/a.txt`, "a\n");
    await Deno.writeTextFile(`${dir}/b.txt`, "b\n");
    await shadow.revertPaths(dir, "s5", ["a.txt"]);
    assertEquals(await Deno.stat(`${dir}/a.txt`).catch(() => null), null); // added → gone
    assertEquals(await Deno.readTextFile(`${dir}/b.txt`), "b\n"); // untouched
    assertEquals((await shadow.diff(dir, "s5")).files.map((f) => f.path), ["b.txt"]);
  });
});

Deno.test("undoAll: whole change back to base, diff clears, modified files restore", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s6");
    await Deno.writeTextFile(`${dir}/committed.txt`, "rewritten\n");
    await Deno.writeTextFile(`${dir}/junk.txt`, "junk\n");
    const reverted = await shadow.undoAll(dir, "s6");
    assertEquals(reverted.sort(), ["committed.txt", "junk.txt"]);
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "committed+dirty\n");
    assertEquals(await Deno.stat(`${dir}/junk.txt`).catch(() => null), null);
    assertEquals((await shadow.diff(dir, "s6")).files, []);
  });
});

Deno.test("accept: seals — diff clears, work stays on disk", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s7");
    await Deno.writeTextFile(`${dir}/done.txt`, "done\n");
    await shadow.accept(dir, "s7", "bough: my feature");
    assertEquals((await shadow.diff(dir, "s7")).files, []);
    assertEquals(await Deno.readTextFile(`${dir}/done.txt`), "done\n");
    // New work after the seal diffs against the new base.
    await Deno.writeTextFile(`${dir}/later.txt`, "later\n");
    assertEquals((await shadow.diff(dir, "s7")).files.map((f) => f.path), ["later.txt"]);
  });
});

Deno.test("originRepo: resolves for worktrees, null for plain repos", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    const dir = await shadow.createSessionWorkspace(repo, "s8");
    assertEquals(await shadow.originRepo(dir), await Deno.realPath(repo));
    assertEquals(await shadow.originRepo(repo), null);
  });
});

Deno.test("looksLikeBrokenStore: corruption signatures only", () => {
  assert(shadow.looksLikeBrokenStore(new Error("fatal: not a git repository")));
  assert(shadow.looksLikeBrokenStore(new Error("error: object file is corrupt")));
  assert(!shadow.looksLikeBrokenStore(new Error("permission denied")));
});

Deno.test("hydrate: gitignored runtime artifacts clone into the worktree", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    // Ignored deps at root, one level deep, and an ignored env file.
    await Deno.writeTextFile(`${repo}/.gitignore`, "ignored.txt\nnode_modules/\n.env\n");
    await Deno.mkdir(`${repo}/node_modules/pkg`, { recursive: true });
    await Deno.writeTextFile(`${repo}/node_modules/pkg/index.js`, "dep\n");
    await Deno.mkdir(`${repo}/web/node_modules/wpkg`, { recursive: true });
    await Deno.writeTextFile(`${repo}/web/node_modules/wpkg/index.js`, "webdep\n");
    await Deno.writeTextFile(`${repo}/web/app.ts`, "app\n"); // tracked sibling
    await Deno.writeTextFile(`${repo}/.env`, "SECRET=1\n");
    const dir = await shadow.createSessionWorkspace(repo, "h1");
    assertEquals(await Deno.readTextFile(`${dir}/node_modules/pkg/index.js`), "dep\n");
    assertEquals(await Deno.readTextFile(`${dir}/web/node_modules/wpkg/index.js`), "webdep\n");
    assertEquals(await Deno.readTextFile(`${dir}/.env`), "SECRET=1\n");
    // Hydrated artifacts are ignored in the worktree too — they never hit the diff.
    assertEquals((await shadow.diff(dir, "h1")).files, []);
    // Isolation: mutating the clone leaves the origin's copy alone.
    await Deno.writeTextFile(`${dir}/node_modules/pkg/index.js`, "mutated\n");
    assertEquals(await Deno.readTextFile(`${repo}/node_modules/pkg/index.js`), "dep\n");
  });
});

Deno.test("shipToOrigin: commits on the origin branch without touching its index; pushes", async () => {
  await withRoots(async () => {
    const repo = await makeRepo();
    // A bare "remote" so push has somewhere real to go.
    const remote = await Deno.makeTempDir({ prefix: "bough-shadow-remote-" });
    await sh(remote, "git", "init", "-q", "--bare", ".");
    await sh(repo, "git", "remote", "add", "origin", remote);
    // The user has something STAGED that must survive shipping untouched.
    await Deno.writeTextFile(`${repo}/staged.txt`, "user staged\n");
    await sh(repo, "git", "add", "staged.txt");

    const dir = await shadow.createSessionWorkspace(repo, "ship1");
    await Deno.writeTextFile(`${dir}/feature.txt`, "shipped\n");
    const res = await shadow.shipToOrigin(dir, "ship1", repo, {
      message: "bough: ship feature",
      push: true,
    });
    assert(res.commit, "expected a commit");
    assertEquals(res.branch, "main");
    assertEquals(res.paths, ["feature.txt"]);
    assertEquals(res.pushed, true);

    // The commit is on main, contains ONLY the shipped file, and reached the remote.
    assertEquals((await sh(repo, "git", "log", "-1", "--format=%s")).trim(), "bough: ship feature");
    const shown = await sh(repo, "git", "show", "--stat", "--format=", "HEAD");
    assertStringIncludes(shown, "feature.txt");
    assertEquals(shown.includes("staged.txt"), false);
    assertEquals(
      (await sh(remote, "git", "log", "-1", "--format=%s", "main")).trim(),
      "bough: ship feature",
    );
    // The user's staging area is exactly as they left it.
    assertEquals((await sh(repo, "git", "diff", "--cached", "--name-only")).trim(), "staged.txt");
    // The session sealed: its rail is empty.
    assertEquals((await shadow.diff(dir, "ship1")).files, []);
    // Re-ship with nothing new is a clean no-op.
    const again = await shadow.shipToOrigin(dir, "ship1", repo, { message: "again", push: true });
    assertEquals(again.commit, null);
    await Deno.remove(remote, { recursive: true }).catch(() => {});
  });
});
