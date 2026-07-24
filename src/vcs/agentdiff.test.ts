import { assert, assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import * as agentdiff from "./agentdiff.ts";

Deno.test("parseAgentfsDiff maps A/M/D, drops dirs and AppleDouble sidecars", () => {
  const out = [
    "A f /._added.txt",
    "A f /added.txt",
    "M f /existing.txt",
    "D f /gone.txt",
    "A d /sub",
    "A f /sub/._nested.txt",
    "A f /sub/nested.txt",
  ].join("\n");
  const entries = agentdiff.parseAgentfsDiff(out);
  assertEquals(entries, [
    { status: "added", path: "added.txt" },
    { status: "modified", path: "existing.txt" },
    { status: "deleted", path: "gone.txt" },
    { status: "added", path: "sub/nested.txt" },
  ]);
});

Deno.test("deltaDbPath is HOME-anchored under .agentfs/run", () => {
  const prev = Deno.env.get("HOME");
  try {
    Deno.env.set("HOME", "/tmp/fake-home");
    assertEquals(agentdiff.deltaDbPath("sid-123"), "/tmp/fake-home/.agentfs/run/sid-123/delta.db");
  } finally {
    if (prev === undefined) Deno.env.delete("HOME");
    else Deno.env.set("HOME", prev);
  }
});

// --- integration: drive a real `agentfs run` overlay -------------------------

async function agentfsUsable(): Promise<boolean> {
  try {
    const r = await new Deno.Command("agentfs", {
      args: ["--version"],
      stdout: "null",
      stderr: "null",
    })
      .output();
    return r.code === 0;
  } catch {
    return false;
  }
}
const AGENTFS = await agentfsUsable();

/** Run a shell command inside the session overlay of `baseDir` (HOME=home so the
 *  delta lands where deltaDbPath expects it). */
async function overlayRun(home: string, baseDir: string, sid: string, cmd: string): Promise<void> {
  const r = await new Deno.Command("agentfs", {
    args: ["run", "--session", sid, "--", "/bin/sh", "-c", cmd],
    cwd: baseDir,
    env: { ...Deno.env.toObject(), HOME: home },
    stdout: "null",
    stderr: "null",
  }).output();
  if (r.code !== 0) throw new Error(`overlay run failed (${r.code})`);
}

/** Point HOME at a temp dir for the body, so agentfs run dirs are isolated + reaped. */
async function withHome(fn: (home: string) => Promise<void>): Promise<void> {
  const home = await Deno.makeTempDir({ prefix: "agentdiff-home-" });
  const prev = Deno.env.get("HOME");
  Deno.env.set("HOME", home);
  try {
    await fn(home);
  } finally {
    if (prev === undefined) Deno.env.delete("HOME");
    else Deno.env.set("HOME", prev);
    await Deno.remove(home, { recursive: true }).catch(() => {});
  }
}

Deno.test({
  name: "diff reports add/modify/delete from a live overlay",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      try {
        await Deno.writeTextFile(join(base, "keep.txt"), "keep\n");
        await Deno.writeTextFile(join(base, "mod.txt"), "a\nb\nc\n");
        await Deno.writeTextFile(join(base, "del.txt"), "gone\n");
        const sid = `t-diff-${crypto.randomUUID()}`;
        await overlayRun(
          home,
          base,
          sid,
          `printf 'a\nB\nc\n' > mod.txt; printf 'fresh\n' > add.txt; rm del.txt`,
        );
        const d = await agentdiff.diff(base, sid);
        assertEquals(d.source, "shadow");
        const byPath = new Map(d.files.map((f) => [f.path, f.status]));
        assertEquals(byPath.get("add.txt"), "added");
        assertEquals(byPath.get("mod.txt"), "modified");
        assertEquals(byPath.get("del.txt"), "deleted");
        assert(!byPath.has("keep.txt"), "untouched file must not appear");
        const mod = d.files.find((f) => f.path === "mod.txt")!;
        assert(mod.hunks.length > 0, "modified file has hunks");
        assert(mod.hunks[0].lines.some((l) => l === "-b"), "shows removed line");
        assert(mod.hunks[0].lines.some((l) => l === "+B"), "shows added line");
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "materialize delivers tip content into the origin checkout",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      const origin = await Deno.makeTempDir({ prefix: "agentdiff-origin-" });
      try {
        await Deno.writeTextFile(join(base, "mod.txt"), "a\nb\nc\n");
        await Deno.writeTextFile(join(origin, "mod.txt"), "a\nb\nc\n"); // origin == base
        const sid = `t-mat-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'a\nB\nc\n' > mod.txt; printf 'new\n' > add.txt`);
        const delivered = await agentdiff.materialize(base, sid, origin, []);
        assertEquals(delivered.sort(), ["add.txt", "mod.txt"]);
        assertEquals(await Deno.readTextFile(join(origin, "mod.txt")), "a\nB\nc\n");
        assertEquals(await Deno.readTextFile(join(origin, "add.txt")), "new\n");
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
        await Deno.remove(origin, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "accept folds the delta into base and clears the diff",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      try {
        await Deno.writeTextFile(join(base, "mod.txt"), "a\n");
        const sid = `t-acc-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'a\nb\n' > mod.txt; printf 'x\n' > add.txt; `);
        assert((await agentdiff.diff(base, sid)).files.length > 0);
        await agentdiff.accept(base, sid);
        // Base now carries the work…
        assertEquals(await Deno.readTextFile(join(base, "mod.txt")), "a\nb\n");
        assertEquals(await Deno.readTextFile(join(base, "add.txt")), "x\n");
        // …and the delta is gone, so the rail is clean.
        assertEquals(await agentdiff.hasDelta(sid), false);
        assertEquals((await agentdiff.diff(base, sid)).files.length, 0);
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
      }
    });
  },
});

async function git(cwd: string, args: string[], env?: Record<string, string>): Promise<string> {
  const r = await new Deno.Command("git", {
    args,
    cwd,
    env: { ...Deno.env.toObject(), ...(env ?? {}) },
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (r.code !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${new TextDecoder().decode(r.stderr)}`);
  }
  return new TextDecoder().decode(r.stdout);
}

Deno.test({
  name: "openPr commits the delta onto a new branch, pushes, and calls gh",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const bin = await Deno.makeTempDir({ prefix: "agentdiff-bin-" });
      const remote = await Deno.makeTempDir({ prefix: "agentdiff-remote-" });
      const origin = await Deno.makeTempDir({ prefix: "agentdiff-origin-" });
      const base = origin; // the session's base worktree IS the origin checkout here
      const prevPath = Deno.env.get("PATH") ?? "";
      try {
        // A fake gh on PATH that records its argv and prints a PR url.
        const ghLog = join(bin, "gh-args.txt");
        await Deno.writeTextFile(
          join(bin, "gh"),
          `#!/bin/sh\nprintf '%s\\n' "$@" > "${ghLog}"\necho https://github.com/acme/repo/pull/42\n`,
        );
        await Deno.chmod(join(bin, "gh"), 0o755);
        Deno.env.set("PATH", `${bin}:${prevPath}`);

        // A bare remote and an origin checkout with one commit on `main`.
        await git(remote, ["init", "--bare", "-b", "main", "."]);
        const cfg = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
        await git(origin, ["init", "-b", "main", "."], cfg);
        await git(origin, ["config", "user.email", "t@t"], cfg);
        await git(origin, ["config", "user.name", "t"], cfg);
        await Deno.writeTextFile(join(origin, "keep.txt"), "keep\n");
        await Deno.writeTextFile(join(origin, "mod.txt"), "a\nb\nc\n");
        await git(origin, ["add", "-A"], cfg);
        await git(origin, ["commit", "-m", "init"], cfg);
        await git(origin, ["remote", "add", "origin", remote], cfg);
        await git(origin, ["push", "-u", "origin", "main"], cfg);
        const headBefore = (await git(origin, ["rev-parse", "HEAD"], cfg)).trim();

        // Session edits via the overlay: one modify, one add.
        const sid = `t-pr-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'a\nB\nc\n' > mod.txt; printf 'new\n' > add.txt`);

        const res = await agentdiff.openPr(base, sid, origin, {
          title: "My change",
          body: "does things",
        });

        assertEquals(res.base, "main");
        assertEquals(res.pushed, true);
        assertEquals(res.url, "https://github.com/acme/repo/pull/42");
        assert(res.commit, "a commit was created");
        assertEquals(res.paths.sort(), ["add.txt", "mod.txt"]);
        assert(res.branch.startsWith("bough/"), `derived branch name: ${res.branch}`);

        // The origin's working tree and current branch are untouched.
        assertEquals((await git(origin, ["rev-parse", "HEAD"], cfg)).trim(), headBefore);
        assertEquals(await Deno.readTextFile(join(origin, "mod.txt")), "a\nB\nc\n"); // == folded tip (accept)

        // The PR branch exists locally and on the remote, with the session's content.
        const branchTree = await git(origin, ["show", `${res.branch}:add.txt`], cfg);
        assertEquals(branchTree, "new\n");
        const onRemote = await git(remote, ["rev-parse", `refs/heads/${res.branch}`]);
        assertEquals(onRemote.trim(), res.commit);

        // gh got the right base/head/title.
        const ghArgs = await Deno.readTextFile(ghLog);
        assert(ghArgs.includes("--base"), "gh received --base");
        assert(ghArgs.includes("main"), "gh targets main");
        assert(ghArgs.includes(res.branch), "gh heads the new branch");
        assert(ghArgs.includes("My change"), "gh got the title");
      } finally {
        Deno.env.set("PATH", prevPath);
        await Deno.remove(bin, { recursive: true }).catch(() => {});
        await Deno.remove(remote, { recursive: true }).catch(() => {});
        await Deno.remove(origin, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "openPr with no changes reports nothing to do",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async () => {
      const origin = await Deno.makeTempDir({ prefix: "agentdiff-origin-" });
      const cfg = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
      try {
        await git(origin, ["init", "-b", "main", "."], cfg);
        await git(origin, ["config", "user.email", "t@t"], cfg);
        await git(origin, ["config", "user.name", "t"], cfg);
        await Deno.writeTextFile(join(origin, "f.txt"), "x\n");
        await git(origin, ["add", "-A"], cfg);
        await git(origin, ["commit", "-m", "init"], cfg);
        const sid = `t-pr-empty-${crypto.randomUUID()}`;
        const res = await agentdiff.openPr(origin, sid, origin, { title: "nothing" });
        assertEquals(res.commit, null);
        assertEquals(res.pushed, false);
        assert(res.note?.includes("nothing to open a PR"), res.note ?? "");
      } finally {
        await Deno.remove(origin, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "undoAll discards the whole delta back to base",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      try {
        await Deno.writeTextFile(join(base, "mod.txt"), "a\n");
        const sid = `t-undo-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'CHANGED\n' > mod.txt`);
        const reverted = await agentdiff.undoAll(base, sid);
        assertEquals(reverted, ["mod.txt"]);
        assertEquals(await agentdiff.hasDelta(sid), false);
        assertEquals(await Deno.readTextFile(join(base, "mod.txt")), "a\n"); // base pristine
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
      }
    });
  },
});
