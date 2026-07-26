// normalizeWorkspace / workspaceProblem — the two pure helpers that keep a bad
// workspace path from silently killing every tool in a session — plus the
// prepareWorkspace flow, which now runs the turn IN the user's checkout and only
// records the base sha the Changes rail diffs against.
import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import * as repodiff from "../vcs/repodiff.ts";
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

Deno.test("prepareWorkspace: a session-scoped turn gets a scratch dir created OUTSIDE the workspace", async () => {
  // A plain (non-repo) dir as the workspace: sessionScoped=true, but the base-sha
  // capture is skipped, so this exercises scratch-dir creation without needing git.
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
    db.createSession({
      id: "s1",
      parentId: null,
      title: "s1",
      kind: "root",
      createdAt: 1,
      workspace: ws,
    });
    const p = await prepareWorkspace(db, "s1");
    assert(p.sessionScoped);
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

Deno.test("prepareWorkspace: a bare-cwd run (no explicit workspace) has no scratch dir", async () => {
  const db = new Db(":memory:");
  try {
    db.createSession({ id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1 });
    // No workspace column and no BOUGH_WORKSPACE → falls back to cwd, not session-scoped.
    const hadEnv = Deno.env.get("BOUGH_WORKSPACE");
    Deno.env.delete("BOUGH_WORKSPACE");
    try {
      const p = await prepareWorkspace(db, "s1");
      assertEquals(p.sessionScoped, false);
      assertEquals(p.scratchDir, "");
    } finally {
      if (hadEnv !== undefined) Deno.env.set("BOUGH_WORKSPACE", hadEnv);
    }
  } finally {
    db.close();
  }
});

// ---- prepareWorkspace on a real repo (needs git on PATH) --------------------

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

const gitAvailable = await canRun("git");

Deno.test({
  // The whole in-place contract in one flow: a session on a git repo runs its
  // turns in THAT repo (no relocation, workspace column untouched), the first
  // turn pins the base sha, later turns and forks land in the same dir, and the
  // Changes rail reads the edits straight out of the checkout.
  name: "prepareWorkspace: turns run in the user's checkout; base sha pinned once; fork inherits",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const snapBase = await Deno.makeTempDir({ prefix: "wstest-snap-" });
    const scratchBase = await Deno.makeTempDir({ prefix: "wstest-scratch-" });
    const env: Record<string, string> = {
      BOUGH_SNAPSHOT_BASE: snapBase,
      BOUGH_SCRATCH_BASE: scratchBase,
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

      // First turn: runs in the repo itself, and records where it started.
      const p1 = await prepareWorkspace(db, "s1");
      assert(p1.sessionScoped);
      assertEquals(p1.cwd, repo);
      assertEquals(db.getSessionRuntime("s1").workspace, repo);
      const head = await repodiff.headSha(repo);
      assertEquals(db.getSessionRuntime("s1").base, head);

      // Session work lands in the user's tree — that IS the delivery mechanism —
      // and the rail reports it as this session's change.
      await Deno.writeTextFile(`${repo}/work.txt`, "s1-work\n");
      const d1 = await repodiff.diffSince(repo, db.getSessionRuntime("s1").base);
      assertEquals(d1.files.map((f) => f.path), ["work.txt"]);

      // Resume: the base is pinned once, so later turns keep diffing against the
      // session's true starting point even as commits land on top.
      const p2 = await prepareWorkspace(db, "s1");
      assertEquals(p2.cwd, repo);
      assertEquals(db.getSessionRuntime("s1").base, head);

      // Fork: a sibling carrying the target's workspace column simply shares the
      // checkout — there is no copy to branch, so it sees the same tree.
      db.createSession({
        id: "s2",
        parentId: null,
        title: "s2",
        kind: "fork",
        createdAt: 2,
        workspace: db.getSessionRuntime("s1").workspace,
        originId: "s1",
      });
      const pf = await prepareWorkspace(db, "s2");
      assertEquals(pf.cwd, repo);
      assertEquals(await Deno.readTextFile(`${pf.cwd}/work.txt`), "s1-work\n");
    } finally {
      db.close();
      for (const [k, v] of prev) {
        if (v === undefined) Deno.env.delete(k);
        else Deno.env.set(k, v);
      }
      await Deno.remove(repo, { recursive: true });
      await Deno.remove(snapBase, { recursive: true }).catch(() => {});
      await Deno.remove(scratchBase, { recursive: true }).catch(() => {});
    }
  },
});
