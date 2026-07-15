import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import * as jj from "./jj.ts";

async function sh(bin: string, args: string[], cwd: string): Promise<void> {
  const { code, stderr } = await new Deno.Command(bin, {
    args,
    cwd,
    stdout: "null",
    stderr: "piped",
  }).output();
  if (code !== 0) throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(stderr)}`);
}

/** A fresh git repo with one commit, in a temp dir. Returns its path. */
async function tempGitRepo(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "jjtest-" });
  await sh("git", ["init", "-q", "."], dir);
  await Deno.writeTextFile(`${dir}/README.md`, "base\n");
  await sh("git", ["add", "-A"], dir);
  await sh("git", [
    "-c",
    "user.email=t@t",
    "-c",
    "user.name=t",
    "-c",
    "commit.gpgsign=false",
    "commit",
    "-qm",
    "init",
  ], dir);
  return dir;
}

// These smokes shell out to jj + git, so they need `--allow-run`. Under the
// current `deno task test` flags they self-skip; run them for real with
// `deno test --allow-run` (see src/sandbox/INTEGRATION.md).
async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}

const jjAvailable = (await canRun("jj")) && (await canRun("git")) &&
  await (async () => {
    try {
      await jj.version();
      return true;
    } catch {
      return false;
    }
  })();

Deno.test({
  name: "jj: init → edit → diff → fork → undo round-trip",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      // version() reports jj.
      assertStringIncludes(await jj.version(), "jj");

      // ensureWorkspace creates the session change; base repo content is preserved.
      const name = await jj.ensureWorkspace(repo, "s1");
      assertEquals(name, "bough/s1");
      assertEquals((await Deno.readTextFile(`${repo}/README.md`)).trim(), "base");

      // Edit inside the workspace, then diff shows exactly that change.
      await Deno.writeTextFile(`${repo}/hello.txt`, "from-s1\n");
      const d1 = await jj.diff(repo, "s1");
      assertEquals(d1.source, "jj");
      const paths1 = d1.files.map((f) => f.path).sort();
      assertEquals(paths1, ["hello.txt"]);
      assertEquals(d1.files[0].status, "added");
      assertEquals(d1.files[0].hunks[0].lines, ["+from-s1"]);

      // Fork s1 → s2: the fork inherits s1's work, then diverges.
      const forked = await jj.forkSession(repo, "s1", "s2");
      assertEquals(forked, "bough/s2");
      await Deno.writeTextFile(`${repo}/only-s2.txt`, "s2-work\n");
      const d2 = await jj.diff(repo, "s2");
      assertEquals(d2.files.map((f) => f.path).sort(), ["only-s2.txt"]);

      // s1's diff is untouched by the fork's edits.
      const d1again = await jj.diff(repo, "s1");
      assertEquals(d1again.files.map((f) => f.path).sort(), ["hello.txt"]);

      // Idempotent resume: ensureWorkspace on an existing session switches to it.
      const resumed = await jj.ensureWorkspace(repo, "s1");
      assertEquals(resumed, "bough/s1");

      // Op log + undo: the last op (switching to s1) can be undone.
      const ops = await jj.operations(repo, 5);
      assert(ops.length > 0, "op log should be non-empty");
      await jj.undo(repo);

      // restore to a specific op id works (restore to the current tip is a no-op-safe call).
      await jj.restore(repo, ops[0].id);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  name: "jj: revertPaths reverts only the given paths; the rest of the change survives",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      await jj.ensureWorkspace(repo, "s1");
      // Two edits in one change: a new file and a modified tracked file.
      await Deno.writeTextFile(`${repo}/a.txt`, "a-work\n");
      await Deno.writeTextFile(`${repo}/README.md`, "base\nedit\n");

      const before = await jj.diff(repo, "s1");
      assertEquals(before.files.map((f) => f.path).sort(), ["README.md", "a.txt"]);

      // Revert only a.txt: it goes back to the parent (gone), README.md survives.
      await jj.revertPaths(repo, "s1", ["a.txt"]);
      assertEquals(await exists(`${repo}/a.txt`), false);
      assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\nedit\n");

      // The diff shrank to just the surviving edit.
      const after = await jj.diff(repo, "s1");
      assertEquals(after.files.map((f) => f.path), ["README.md"]);
      assertEquals(after.files[0].status, "modified");

      // Empty paths is a no-op (whole-change undo is revertChanges' job, not this).
      await jj.revertPaths(repo, "s1", []);
      assertEquals((await jj.diff(repo, "s1")).files.map((f) => f.path), ["README.md"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  name: "jj: ensureWorkspace is idempotent and modified files diff correctly",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      await jj.ensureWorkspace(repo, "s1");
      await Deno.writeTextFile(`${repo}/README.md`, "base\nadded line\n");
      const d = await jj.diff(repo, "s1");
      assertEquals(d.files.length, 1);
      assertEquals(d.files[0].path, "README.md");
      assertEquals(d.files[0].status, "modified");
      assertEquals(d.files[0].hunks[0].lines, [" base", "+added line"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  name: "jj: accept seals the change — edits stay on disk, session diff resets",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      await jj.ensureWorkspace(repo, "s1");
      await Deno.writeTextFile(`${repo}/hello.txt`, "from-s1\n");
      assertEquals((await jj.diff(repo, "s1")).files.length, 1);

      await jj.accept(repo, "s1");

      // The accepted edit is still on disk, but the session's change-vs-parent
      // diff is empty — the bookmark now sits on a fresh child of the sealed commit.
      assertEquals(await Deno.readTextFile(`${repo}/hello.txt`), "from-s1\n");
      assertEquals((await jj.diff(repo, "s1")).files.length, 0);

      // Post-accept work diffs alone, and resume still lands on the session change.
      await Deno.writeTextFile(`${repo}/next.txt`, "more\n");
      assertEquals((await jj.diff(repo, "s1")).files.map((f) => f.path), ["next.txt"]);
      assertEquals(await jj.ensureWorkspace(repo, "s1"), "bough/s1");
      assertEquals((await jj.diff(repo, "s1")).files.map((f) => f.path), ["next.txt"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  // Regression: diff() of a session whose bookmark no longer resolves to a revision
  // (abandoned/pruned change, or a stale colocated-git bookmark) must degrade to an
  // empty diff, not throw "Revision `bough/<id>` doesn't exist". This was surfacing
  // in the Changes rail as "changes: jj diff failed for <id>".
  name: "jj: diff of a vanished session bookmark returns empty, not an error",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      await jj.ensureWorkspace(repo, "gone");
      // A resolvable session diffs fine (empty change, no files).
      assertEquals((await jj.diff(repo, "gone")).files.length, 0);

      // Delete the bookmark so `bough/gone` no longer resolves as a revision —
      // the exact state that produced the reported error.
      await sh("jj", [
        "--no-pager",
        "--color=never",
        "bookmark",
        "delete",
        "bough/gone",
      ], repo);

      // diff() must not throw; it returns an empty jj diff.
      const d = await jj.diff(repo, "gone");
      assertEquals(d.source, "jj");
      assertEquals(d.files.length, 0);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  // Subagents: each gets its own jj workspace (a second working copy) branched off
  // the spawner's tip, edits in parallel without touching the spawner's checkout,
  // and adoptChanges squashes its diff back into the spawner's change.
  name: "jj: addWorkspace isolates a subagent; adoptChanges squashes back",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const subDir = await Deno.makeTempDir({ prefix: "jjtest-ws-" });
    try {
      // Spawner session with in-progress work.
      await jj.ensureWorkspace(repo, "spawner");
      await Deno.writeTextFile(`${repo}/spawner.txt`, "spawner-work\n");

      // Subagent workspace branches off the spawner's tip into its own dir.
      // (jj workspace add wants the destination to not exist yet.)
      await Deno.remove(subDir);
      await jj.addWorkspace(repo, "sub", subDir, jj.bookmarkFor("spawner"));

      // The subagent sees the spawner's work and adds its own.
      assertEquals(await Deno.readTextFile(`${subDir}/spawner.txt`), "spawner-work\n");
      await Deno.writeTextFile(`${subDir}/sub.txt`, "sub-work\n");

      // Its diff (vs the spawner tip it branched from) is just its own edit,
      // and the spawner's checkout is untouched by it.
      assertEquals((await jj.diff(subDir, "sub")).files.map((f) => f.path), ["sub.txt"]);
      let spawnerHasSub = true;
      try {
        await Deno.stat(`${repo}/sub.txt`);
      } catch {
        spawnerHasSub = false;
      }
      assertEquals(spawnerHasSub, false);

      // addWorkspace is idempotent on an existing dir.
      await jj.addWorkspace(repo, "sub", subDir, jj.bookmarkFor("spawner"));

      // Adopt: the subagent's edit lands in the spawner's change and on its disk;
      // the subagent's change empties but stays continuable (bookmark alive).
      await jj.adoptChanges(repo, subDir, "sub", "spawner");
      assertEquals(await Deno.readTextFile(`${repo}/sub.txt`), "sub-work\n");
      const spawnerDiff = await jj.diff(repo, "spawner");
      assertEquals(spawnerDiff.files.map((f) => f.path).sort(), ["spawner.txt", "sub.txt"]);
      assertEquals((await jj.diff(subDir, "sub")).files.length, 0);

      // The subagent workspace still works after adoption (not stale, can edit on).
      await Deno.writeTextFile(`${subDir}/more.txt`, "follow-up\n");
      assertEquals((await jj.diff(subDir, "sub")).files.map((f) => f.path), ["more.txt"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(subDir, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // Regression: an op from a sibling workspace (concurrent subagent spawn, adopt)
  // can rewrite the spawner workspace's working-copy commit, leaving it stale —
  // `jj workspace add` then refuses ("The working copy is stale") and the subagent
  // spawn fails. addWorkspace must repair the staleness before branching.
  name: "jj: addWorkspace repairs a stale spawner working copy",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const dir1 = await Deno.makeTempDir({ prefix: "jjtest-ws-" });
    const dir2 = await Deno.makeTempDir({ prefix: "jjtest-ws-" });
    try {
      await jj.ensureWorkspace(repo, "spawner");
      await Deno.remove(dir1);
      await jj.addWorkspace(repo, "sub1", dir1, jj.bookmarkFor("spawner"));

      // Rewrite the spawner's working-copy change (tree included) from the
      // sibling workspace — the spawner's checkout is now stale (the state the
      // incident hit). A message-only rewrite isn't enough: jj reconciles those
      // silently; staleness needs the tree to have changed under the workspace.
      await Deno.writeTextFile(`${dir1}/sub1.txt`, "sub1-work\n");
      await sh("jj", [
        "--no-pager",
        "--color=never",
        "--config",
        "user.name=t",
        "--config",
        "user.email=t@t",
        "squash",
        "--from",
        jj.bookmarkFor("sub1"),
        "--into",
        jj.bookmarkFor("spawner"),
        "--use-destination-message",
      ], dir1);

      // Previously threw: jj workspace add failed (1): The working copy is stale.
      await Deno.remove(dir2);
      await jj.addWorkspace(repo, "sub2", dir2, jj.bookmarkFor("spawner"));
      assertEquals(await Deno.readTextFile(`${dir2}/README.md`), "base\n");
    } finally {
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(dir1, { recursive: true }).catch(() => {});
      await Deno.remove(dir2, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // Regression: adopting the FIRST of two parallel subagents rewrites the spawner's
  // change, which rebases the second subagent's working-copy commit and leaves its
  // workspace stale — adoptChanges' opening snapshot (`jj st`) then refused with
  // "The working copy is stale" and the second adopt failed. snapshot() must repair
  // staleness and retry so every parallel branch stays adoptable.
  name: "jj: adoptChanges adopts a second parallel subagent whose copy went stale",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const dir1 = await Deno.makeTempDir({ prefix: "jjtest-ws-" });
    const dir2 = await Deno.makeTempDir({ prefix: "jjtest-ws-" });
    try {
      await jj.ensureWorkspace(repo, "spawner");
      await Deno.remove(dir1);
      await jj.addWorkspace(repo, "sub1", dir1, jj.bookmarkFor("spawner"));
      await Deno.remove(dir2);
      await jj.addWorkspace(repo, "sub2", dir2, jj.bookmarkFor("spawner"));

      // Both subagents work in parallel on their own copies; their finishing
      // turns snapshot the work (buildResult's diff), as in the real flow.
      await Deno.writeTextFile(`${dir1}/sub1.txt`, "sub1-work\n");
      await Deno.writeTextFile(`${dir2}/sub2.txt`, "sub2-work\n");
      await jj.snapshot(dir1);
      await jj.snapshot(dir2);

      // Adopting sub1 rewrites the spawner change; sub2's working-copy commit is
      // rebased under it and its workspace goes stale.
      await jj.adoptChanges(repo, dir1, "sub1", "spawner");
      // Previously threw: jj st failed (1): The working copy is stale.
      await jj.adoptChanges(repo, dir2, "sub2", "spawner");

      assertEquals(await Deno.readTextFile(`${repo}/sub1.txt`), "sub1-work\n");
      assertEquals(await Deno.readTextFile(`${repo}/sub2.txt`), "sub2-work\n");
      const spawnerDiff = await jj.diff(repo, "spawner");
      assertEquals(spawnerDiff.files.map((f) => f.path).sort(), ["sub1.txt", "sub2.txt"]);
      // Both subagent branches emptied but stay continuable.
      assertEquals((await jj.diff(dir1, "sub1")).files.length, 0);
      assertEquals((await jj.diff(dir2, "sub2")).files.length, 0);
    } finally {
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(dir1, { recursive: true }).catch(() => {});
      await Deno.remove(dir2, { recursive: true }).catch(() => {});
    }
  },
});

/** Set env vars for the duration of `fn`, restoring previous values after. */
async function withEnv(vars: Record<string, string>, fn: () => Promise<void>): Promise<void> {
  const prev = new Map<string, string | undefined>();
  for (const [k, v] of Object.entries(vars)) {
    prev.set(k, Deno.env.get(k));
    Deno.env.set(k, v);
  }
  try {
    await fn();
  } finally {
    for (const [k, v] of prev) {
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
  }
}

async function gitOut(repo: string, args: string[]): Promise<string> {
  const { code, stdout, stderr } = await new Deno.Command("git", {
    args,
    cwd: repo,
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (code !== 0) throw new Error(`git ${args.join(" ")}: ${new TextDecoder().decode(stderr)}`);
  return new TextDecoder().decode(stdout).trim();
}

async function exists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}

/** Temp roots for the external store + session workspaces, cleaned up by the caller. */
async function tempExternalBases(): Promise<{ jjBase: string; wsBase: string }> {
  return {
    jjBase: await Deno.makeTempDir({ prefix: "jjtest-store-" }),
    wsBase: await Deno.makeTempDir({ prefix: "jjtest-wsroot-" }),
  };
}

Deno.test({
  // External mode: a session on a plain git repo gets its own working copy and the
  // repo itself is never touched — no .jj, HEAD stays on its branch, git status is
  // unchanged, and the agent's edits never appear in the user's checkout.
  name: "jj: createSessionWorkspace keeps the user's repo pristine",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const { jjBase, wsBase } = await tempExternalBases();
    try {
      await withEnv({ BOUGH_JJ_BASE: jjBase, BOUGH_SUBAGENT_BASE: wsBase }, async () => {
        // Dirty the repo the way a real checkout is dirty: an uncommitted edit to
        // a tracked file and an untracked new file.
        await Deno.writeTextFile(`${repo}/README.md`, "base\nlocal edit\n");
        await Deno.writeTextFile(`${repo}/untracked.ts`, "export const x = 1;\n");
        const headRefBefore = await gitOut(repo, ["symbolic-ref", "HEAD"]);
        const statusBefore = await gitOut(repo, ["status", "--porcelain"]);

        const dir = await jj.createSessionWorkspace(repo, "s1");
        assertEquals(dir, jj.workspaceDirFor("s1"));

        // The repo is pristine: no .jj, same branch checkout, same dirty status.
        assertEquals(await exists(`${repo}/.jj`), false);
        assertEquals(await gitOut(repo, ["symbolic-ref", "HEAD"]), headRefBefore);
        assertEquals(await gitOut(repo, ["status", "--porcelain"]), statusBefore);
        assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\nlocal edit\n");

        // The workspace captured the working tree, dirty edit + untracked included.
        assertEquals(await Deno.readTextFile(`${dir}/README.md`), "base\nlocal edit\n");
        assertEquals(await Deno.readTextFile(`${dir}/untracked.ts`), "export const x = 1;\n");

        // The session diff starts empty; an agent edit shows up alone and never
        // lands in the user's checkout.
        assertEquals((await jj.diff(dir, "s1")).files.length, 0);
        await Deno.writeTextFile(`${dir}/agent.txt`, "agent-work\n");
        assertEquals((await jj.diff(dir, "s1")).files.map((f) => f.path), ["agent.txt"]);
        assertEquals(await exists(`${repo}/agent.txt`), false);

        // The session tip is reachable from plain git as branch bough/s1, rooted
        // at the repo's HEAD (via the base snapshot commit).
        const head = await gitOut(repo, ["rev-parse", "HEAD"]);
        assertEquals(await gitOut(repo, ["merge-base", "bough/s1", "HEAD"]), head);

        // accept seals the change in the isolated workspace; work stays on disk.
        await jj.accept(dir, "s1");
        assertEquals((await jj.diff(dir, "s1")).files.length, 0);
        assertEquals(await Deno.readTextFile(`${dir}/agent.txt`), "agent-work\n");

        // Idempotent: a resume reuses the same workspace.
        assertEquals(await jj.createSessionWorkspace(repo, "s1"), dir);
      });
    } finally {
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(jjBase, { recursive: true }).catch(() => {});
      await Deno.remove(wsBase, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // Apply-to-checkout: originRepo resolves the backing repo from jj plumbing, and
  // materialize delivers reviewed paths into the origin working tree (3-way) while
  // HEAD, branch, and unrelated files stay put.
  name: "jj: originRepo + materialize deliver session edits to the user's checkout",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const { jjBase, wsBase } = await tempExternalBases();
    try {
      await withEnv({ BOUGH_JJ_BASE: jjBase, BOUGH_SUBAGENT_BASE: wsBase }, async () => {
        const dir = await jj.createSessionWorkspace(repo, "m1");
        assertEquals(await jj.originRepo(dir), await Deno.realPath(repo));

        // Agent work in the isolated workspace: a new file and an edit.
        await Deno.writeTextFile(`${dir}/agent.txt`, "agent-work\n");
        await Deno.writeTextFile(`${dir}/README.md`, "base\nagent line\n");
        const headRefBefore = await gitOut(repo, ["symbolic-ref", "HEAD"]);

        // Deliver only agent.txt: it lands; the README edit stays back.
        await jj.materialize(dir, "m1", repo, ["agent.txt"]);
        assertEquals(await Deno.readTextFile(`${repo}/agent.txt`), "agent-work\n");
        assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\n");
        // Working tree only — the delivered file arrives untracked, never staged.
        assert((await gitOut(repo, ["status", "--porcelain"])).includes("?? agent.txt"));

        // Deliver the rest (no scope = whole change); HEAD/branch untouched.
        await jj.materialize(dir, "m1", repo, []);
        assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\nagent line\n");
        assertEquals(await gitOut(repo, ["symbolic-ref", "HEAD"]), headRefBefore);

        // Re-applying an already-delivered change is a clean no-op (3-way).
        await jj.materialize(dir, "m1", repo, ["agent.txt"]);
        assertEquals(await Deno.readTextFile(`${repo}/agent.txt`), "agent-work\n");
      });
    } finally {
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(jjBase, { recursive: true }).catch(() => {});
      await Deno.remove(wsBase, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // External mode on a clean repo bases the session directly off HEAD (no snapshot
  // commit), and forking branches a second workspace off the session's tip.
  name: "jj: createSessionWorkspace clean-repo base + fork via addWorkspace",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const { jjBase, wsBase } = await tempExternalBases();
    try {
      await withEnv({ BOUGH_JJ_BASE: jjBase, BOUGH_SUBAGENT_BASE: wsBase }, async () => {
        const dir1 = await jj.createSessionWorkspace(repo, "c1");
        // Clean tree → the session change's parent IS the repo's HEAD commit.
        const head = await gitOut(repo, ["rev-parse", "HEAD"]);
        assertEquals(await gitOut(repo, ["rev-parse", "bough/c1^"]), head);

        // Session work, snapshotted via diff.
        await Deno.writeTextFile(`${dir1}/one.txt`, "c1-work\n");
        assertEquals((await jj.diff(dir1, "c1")).files.map((f) => f.path), ["one.txt"]);

        // Fork: a new workspace branched off c1's tip inherits its work, then
        // diverges — and still nothing touches the repo checkout.
        const dir2 = jj.workspaceDirFor("c2");
        await jj.addWorkspace(dir1, "c2", dir2, jj.bookmarkFor("c1"));
        assertEquals(await Deno.readTextFile(`${dir2}/one.txt`), "c1-work\n");
        await Deno.writeTextFile(`${dir2}/two.txt`, "c2-work\n");
        assertEquals((await jj.diff(dir2, "c2")).files.map((f) => f.path), ["two.txt"]);
        assertEquals((await jj.diff(dir1, "c1")).files.map((f) => f.path), ["one.txt"]);
        assertEquals(await exists(`${repo}/.jj`), false);
        assertEquals(await exists(`${repo}/one.txt`), false);
      });
    } finally {
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(jjBase, { recursive: true }).catch(() => {});
      await Deno.remove(wsBase, { recursive: true }).catch(() => {});
    }
  },
});

Deno.test({
  // Regression: a session opened on a repo with uncommitted + untracked changes
  // must NOT wipe them. `ensureWorkspace` used to `jj new <HEAD>`, resetting the
  // working copy to the committed tree and deleting in-progress work.
  name: "jj: ensureWorkspace preserves uncommitted and untracked files",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      // Dirty the repo the way a real workspace is dirty: an untracked new file
      // and an uncommitted edit to a tracked file.
      await Deno.writeTextFile(`${repo}/untracked.ts`, "export const x = 1;\n");
      await Deno.writeTextFile(`${repo}/README.md`, "base\nlocal edit\n");

      await jj.ensureWorkspace(repo, "s1");

      // Both must still be on disk.
      assertEquals(await Deno.readTextFile(`${repo}/untracked.ts`), "export const x = 1;\n");
      assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\nlocal edit\n");

      // And they belong to the pre-session baseline, so the session's own diff is
      // empty until the agent changes something (no pre-existing dirt leaks in).
      const d0 = await jj.diff(repo, "s1");
      assertEquals(d0.files.length, 0);

      // A genuine agent edit then shows up alone.
      await Deno.writeTextFile(`${repo}/untracked.ts`, "export const x = 2;\n");
      const d1 = await jj.diff(repo, "s1");
      assertEquals(d1.files.map((f) => f.path), ["untracked.ts"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});
