// normalizeWorkspace / workspaceProblem — the two pure helpers that keep a bad
// workspace path from silently killing every tool in a session — plus the
// external-mode prepareWorkspace flow (isolated per-session working copies).
import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import * as jj from "../vcs/jj.ts";
import { normalizeWorkspace, prepareWorkspace, workspaceProblem } from "./workspace.ts";

Deno.test("normalizeWorkspace expands a leading ~", () => {
  const home = Deno.env.get("HOME")!;
  assertEquals(normalizeWorkspace("~/repos/app"), `${home}/repos/app`);
  assertEquals(normalizeWorkspace("~"), home);
});

Deno.test("normalizeWorkspace does not touch a real ~ mid-path or absolute paths", () => {
  assertEquals(normalizeWorkspace("/Users/x/repo"), "/Users/x/repo");
  // A tilde that isn't the home shorthand is left alone.
  assertEquals(normalizeWorkspace("/tmp/~backup"), "/tmp/~backup");
});

Deno.test("normalizeWorkspace absolutizes a relative path against cwd", () => {
  assertEquals(normalizeWorkspace("sub/dir"), `${Deno.cwd()}/sub/dir`);
});

Deno.test("workspaceProblem: ok for a real dir, message for missing/file", async () => {
  const dir = await Deno.makeTempDir();
  assertEquals(await workspaceProblem(dir), null);

  const missing = `${dir}/nope`;
  assertEquals((await workspaceProblem(missing))?.includes("does not exist"), true);

  const file = `${dir}/f.txt`;
  await Deno.writeTextFile(file, "x");
  assertEquals((await workspaceProblem(file))?.includes("not a directory"), true);

  await Deno.remove(dir, { recursive: true });
});

Deno.test("prepareWorkspace: a sandboxed turn gets a scratch dir created OUTSIDE the workspace", async () => {
  // A plain (non-repo) dir as the workspace: sandboxed=true, but the git/jj branch
  // is skipped, so this exercises the scratch-dir creation without needing jj on PATH.
  const ws = await Deno.makeTempDir({ prefix: "wstest-ws-" });
  const snapBase = await Deno.makeTempDir({ prefix: "wstest-snap-" });
  const scratchBase = await Deno.makeTempDir({ prefix: "wstest-scratch-" });
  const env = { BOUGH_SNAPSHOT_BASE: snapBase, BOUGH_SCRATCH_BASE: scratchBase };
  const prev = new Map<string, string | undefined>();
  for (const [k, v] of Object.entries(env)) {
    prev.set(k, Deno.env.get(k));
    Deno.env.set(k, v);
  }
  const db = new Db(":memory:");
  try {
    db.createSession({ id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1, workspace: ws });
    const p = await prepareWorkspace(db, "s1");
    assert(p.sandboxed);
    assertEquals(p.scratchDir, `${scratchBase}/s1`);
    assertEquals((await Deno.stat(p.scratchDir)).isDirectory, true);
    // the scratch dir is not inside the workspace — the whole point
    assert(!p.scratchDir.startsWith(ws), "scratch must live outside the workspace");
  } finally {
    db.close();
    for (const [k, v] of prev) v === undefined ? Deno.env.delete(k) : Deno.env.set(k, v);
    await Deno.remove(ws, { recursive: true }).catch(() => {});
    await Deno.remove(snapBase, { recursive: true }).catch(() => {});
    await Deno.remove(scratchBase, { recursive: true }).catch(() => {});
  }
});

Deno.test("prepareWorkspace: a non-sandboxed run (bare cwd) has no scratch dir", async () => {
  const db = new Db(":memory:");
  try {
    db.createSession({ id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1 });
    // No workspace column and no BOUGH_WORKSPACE → falls back to cwd, unsandboxed.
    const hadEnv = Deno.env.get("BOUGH_WORKSPACE");
    Deno.env.delete("BOUGH_WORKSPACE");
    try {
      const p = await prepareWorkspace(db, "s1");
      assertEquals(p.sandboxed, false);
      assertEquals(p.scratchDir, "");
    } finally {
      if (hadEnv !== undefined) Deno.env.set("BOUGH_WORKSPACE", hadEnv);
    }
  } finally {
    db.close();
  }
});

// ---- external-mode prepareWorkspace (needs jj + git on PATH) -----------------

async function sh(bin: string, args: string[], cwd: string): Promise<void> {
  const { code, stderr } = await new Deno.Command(bin, {
    args,
    cwd,
    stdout: "null",
    stderr: "piped",
  }).output();
  if (code !== 0) throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(stderr)}`);
}

async function tempGitRepo(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "wstest-" });
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

async function exists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}

Deno.test({
  // External mode end-to-end: a session on a plain git repo runs its turns in an
  // isolated working copy (workspace column repointed there), resumes into the
  // same dir, and a fork branches its own copy off the parent's tip. The user's
  // repo never gains a .jj and never changes checkout.
  name: "prepareWorkspace: plain git repo → isolated session workspace; fork inherits",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const jjBase = await Deno.makeTempDir({ prefix: "wstest-store-" });
    const wsBase = await Deno.makeTempDir({ prefix: "wstest-wsroot-" });
    const snapBase = await Deno.makeTempDir({ prefix: "wstest-snap-" });
    const env: Record<string, string> = {
      BOUGH_JJ_BASE: jjBase,
      BOUGH_SUBAGENT_BASE: wsBase,
      BOUGH_SNAPSHOT_BASE: snapBase,
    };
    const prev = new Map<string, string | undefined>();
    for (const [k, v] of Object.entries(env)) {
      prev.set(k, Deno.env.get(k));
      Deno.env.set(k, v);
    }
    const db = new Db(":memory:");
    try {
      db.createSession({
        id: "s1",
        parentId: null,
        title: "s1",
        kind: "root",
        createdAt: 1,
        workspace: repo,
      });

      // First turn: relocated into the session's own working copy, column repointed.
      const p1 = await prepareWorkspace(db, "s1");
      assert(p1.sandboxed);
      assertEquals(p1.cwd, jj.workspaceDirFor("s1"));
      assertEquals(db.getSessionRuntime("s1").workspace, p1.cwd);
      assert(db.getSessionRuntime("s1").base !== null, "base sentinel persisted");
      assertEquals(await exists(`${repo}/.jj`), false);

      // Session work lands in the isolated dir only.
      await Deno.writeTextFile(`${p1.cwd}/work.txt`, "s1-work\n");
      assertEquals((await jj.diff(p1.cwd, "s1")).files.map((f) => f.path), ["work.txt"]);
      assertEquals(await exists(`${repo}/work.txt`), false);

      // Resume: later turns run in the same dir.
      const p2 = await prepareWorkspace(db, "s1");
      assertEquals(p2.cwd, p1.cwd);

      // Fork: inherits the parent's workspace column (as fork() does), gets its
      // own working copy branched off the parent's tip.
      db.createSession({
        id: "s2",
        parentId: "s1",
        title: "s2",
        kind: "fork",
        createdAt: 2,
        workspace: db.getSessionRuntime("s1").workspace,
      });
      const pf = await prepareWorkspace(db, "s2");
      assertEquals(pf.cwd, jj.workspaceDirFor("s2"));
      assertEquals(db.getSessionRuntime("s2").workspace, pf.cwd);
      assertEquals(await Deno.readTextFile(`${pf.cwd}/work.txt`), "s1-work\n");

      // The fork diverges without touching the parent's copy or the repo.
      await Deno.writeTextFile(`${pf.cwd}/fork.txt`, "s2-work\n");
      assertEquals((await jj.diff(pf.cwd, "s2")).files.map((f) => f.path), ["fork.txt"]);
      assertEquals(await exists(`${p1.cwd}/fork.txt`), false);
      assertEquals(await exists(`${repo}/.jj`), false);
    } finally {
      db.close();
      for (const [k, v] of prev) {
        if (v === undefined) Deno.env.delete(k);
        else Deno.env.set(k, v);
      }
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(jjBase, { recursive: true }).catch(() => {});
      await Deno.remove(wsBase, { recursive: true }).catch(() => {});
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
    }
  },
});
