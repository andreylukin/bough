import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import * as clonefile from "../vcs/clonefile.ts";
import { headSha } from "../vcs/repodiff.ts";
import { type AppCtx, createHandler } from "./app.ts";
import type { BoughEvent, Session } from "../schema/parts.ts";
import type { Diff } from "../schema/changes.ts";

function ctx(opts: { snapshotBase?: string } = {}): AppCtx {
  const bus = new Bus();
  return {
    db: new Db(":memory:"),
    bus,
    snapshotBase: opts.snapshotBase,
  };
}

const jsonReq = (method: string, path: string, body?: unknown) =>
  new Request("http://x" + path, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });

async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}
const gitAvailable = await canRun("git");
const cpAvailable = (await canRun("git")) && (await canRun("cp"));

async function tempGitRepo(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "chg-" });
  const sh = async (bin: string, args: string[]) => {
    const { code } = await new Deno.Command(bin, { args, cwd: dir, stdout: "null", stderr: "null" })
      .output();
    if (code !== 0) throw new Error(`${bin} ${args.join(" ")}`);
  };
  await sh("git", ["init", "-q", "."]);
  await Deno.writeTextFile(`${dir}/README.md`, "base\n");
  await sh("git", ["add", "-A"]);
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
  ]);
  return dir;
}

/**
 * What the turn runner's workspace prep does on a session's first turn: pin the
 * sha the session started from. There is no relocation and no overlay — the
 * session's workspace IS `repo`, so "the session's edits" are simply whatever
 * the tree gained since this sha.
 */
async function startSession(c: AppCtx, repo: string, title = "s"): Promise<Session> {
  const h = createHandler(c);
  const s = await (await h(jsonReq("POST", "/sessions", { title, workspace: repo })))
    .json() as Session;
  c.db.setSessionBase(s.id, (await headSha(repo))!);
  return s;
}

// ---- workspace on the API + empty/404 paths (no shell-out) ------------------

Deno.test("workspace persists on create and returns on the session", async () => {
  const c = ctx();
  const h = createHandler(c);
  // The server validates the workspace exists on disk (a bad path would otherwise
  // kill every tool in the session), so use a real dir.
  const proj = await Deno.makeTempDir();
  const created = await (await h(jsonReq("POST", "/sessions", { title: "w", workspace: proj })))
    .json() as Session;
  assertEquals(created.workspace, proj);
  const got = await (await h(jsonReq("GET", `/sessions/${created.id}`))).json() as {
    session: Session;
  };
  assertEquals(got.session.workspace, proj);
  c.db.close();
  await Deno.remove(proj, { recursive: true });
});

Deno.test("create rejects a workspace that doesn't exist", async () => {
  const c = ctx();
  const h = createHandler(c);
  const res = await h(jsonReq("POST", "/sessions", { title: "w", workspace: "/no/such/dir/xyz" }));
  assertEquals(res.status, 400);
  c.db.close();
});

Deno.test("GET changes is empty when the session has no snapshots", async () => {
  const c = ctx({ snapshotBase: await Deno.makeTempDir() });
  const h = createHandler(c);
  const s = await (await h(jsonReq("POST", "/sessions", { title: "s" }))).json() as Session;
  const res = await h(jsonReq("GET", `/sessions/${s.id}/changes`));
  assertEquals(res.status, 200);
  assertEquals(await res.json(), { diffs: [] });
  c.db.close();
});

Deno.test("changes endpoints 404 on unknown session; revert 400 without a workspace", async () => {
  const c = ctx();
  const h = createHandler(c);
  assertEquals((await h(jsonReq("GET", "/sessions/nope/changes"))).status, 404);
  assertEquals(
    (await h(jsonReq("POST", "/sessions/nope/changes/apply", { source: "repo", paths: [] })))
      .status,
    404,
  );
  assertEquals((await h(jsonReq("POST", "/sessions/nope/changes/revert", {}))).status, 404);
  // a real session with no repo workspace can't be reverted — or applied
  const s = await (await h(jsonReq("POST", "/sessions", { title: "s" }))).json() as Session;
  assertEquals((await h(jsonReq("POST", `/sessions/${s.id}/changes/revert`, {}))).status, 400);
  assertEquals(
    (await h(jsonReq("POST", `/sessions/${s.id}/changes/apply`, { source: "repo", paths: [] })))
      .status,
    400,
  );
  c.db.close();
});

// ---- repo-source changes: the user's checkout (self-skips without git) ------

Deno.test({
  name: "changes: the rail is `git diff <base>` — tracked edits AND untracked files",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await startSession(c, repo);
      // The agent works in the checkout, so "making an edit" is just writing there.
      await Deno.writeTextFile(`${repo}/README.md`, "base\nmore\n");
      await Deno.writeTextFile(`${repo}/new.txt`, "hi\n");

      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(diffs.length, 1);
      assertEquals(diffs[0].source, "repo");
      const byPath = new Map(diffs[0].files.map((f) => [f.path, f.status]));
      assertEquals([...byPath.keys()].sort(), ["README.md", "new.txt"]);
      assertEquals(byPath.get("README.md"), "modified");
      assertEquals(byPath.get("new.txt"), "added"); // untracked ⇒ all-added
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

Deno.test({
  name: "changes: repo apply delivers nothing — the work is already in the checkout",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await startSession(c, repo);
      await Deno.writeTextFile(`${repo}/new.txt`, "hi\n");

      // Apply used to materialize a shadow worktree into the origin. There is no
      // second copy to carry over now: the reviewer commits, or reverts.
      const applied = await h(
        jsonReq("POST", `/sessions/${s.id}/changes/apply`, {
          source: "repo",
          paths: ["new.txt"],
        }),
      );
      assertEquals(applied.status, 200);
      const body = await applied.json() as { applied: string[]; origin: string; sealed: boolean };
      assertEquals(body.applied, []);
      assertEquals(body.origin, repo);
      assertEquals(body.sealed, false);
      // …and the rail still shows the work, because nothing was sealed away.
      const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(after.diffs[0].files.map((f) => f.path), ["new.txt"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

Deno.test({
  name: "changes: revert restores a tracked file and deletes a session-created one",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await startSession(c, repo);
      await Deno.writeTextFile(`${repo}/README.md`, "clobbered\n");
      await Deno.writeTextFile(`${repo}/new.txt`, "hi\n");

      const events: BoughEvent[] = [];
      c.bus.subscribe((e) => events.push(e));

      const reverted = await h(jsonReq("POST", `/sessions/${s.id}/changes/revert`, {}));
      assertEquals(reverted.status, 200);
      const { paths } = await reverted.json() as { paths: string[] };
      assertEquals(paths.sort(), ["README.md", "new.txt"]);

      // Tracked file back at its base content; created file gone from disk.
      assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\n");
      assertEquals(await exists(`${repo}/new.txt`), false);
      const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(after.diffs[0]?.files ?? [], []);
      assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

Deno.test({
  name: "changes: per-path revert reverts only the selected file",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await startSession(c, repo);
      await Deno.writeTextFile(`${repo}/a.txt`, "a-work\n");
      await Deno.writeTextFile(`${repo}/b.txt`, "b-work\n");

      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(diffs[0].files.map((f) => f.path).sort(), ["a.txt", "b.txt"]);

      const reverted = await h(
        jsonReq("POST", `/sessions/${s.id}/changes/revert`, { paths: ["a.txt"] }),
      );
      assertEquals(reverted.status, 200);
      assertEquals(await reverted.json(), { ok: true, reverted: "repo", paths: ["a.txt"] });

      // The change shrank to just b.txt — a.txt is gone from the tree, b.txt kept.
      const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(after.diffs[0].files.map((f) => f.path), ["b.txt"]);
      assertEquals(after.diffs[0].files[0].status, "added");
      assertEquals(await exists(`${repo}/a.txt`), false);
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

Deno.test({
  name: "changes: a subagent's edits ride the spawner's own diff — no adopt section",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await startSession(c, repo);
      // A finished subagent — what launch() sets up, minus the turn. It shares the
      // spawner's workspace, so there is no branch to surface and nothing to adopt.
      c.db.createSession({
        id: "sub1",
        parentId: null,
        title: "essay writer",
        kind: "subagent",
        createdAt: 1,
        workspace: repo,
        originId: s.id,
        originMessageId: "m1",
      });
      await Deno.writeTextFile(`${repo}/essay.txt`, "words\n");

      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      // One section, not two: there is no separate subagent group to adopt.
      assertEquals(diffs.length, 1);
      assert(diffs[0].files.some((f) => f.path === "essay.txt"));
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

Deno.test({
  name: "changes: build/cache noise (__pycache__/*.pyc/.DS_Store) is filtered from the diff",
  ignore: !gitAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await startSession(c, repo);
      await Deno.writeTextFile(`${repo}/real.py`, "x = 1\n");
      await Deno.mkdir(`${repo}/__pycache__`, { recursive: true });
      await Deno.writeTextFile(`${repo}/__pycache__/real.cpython-312.pyc`, "junk\n");
      await Deno.writeTextFile(`${repo}/mod.pyc`, "junk\n");
      await Deno.writeTextFile(`${repo}/.DS_Store`, "junk\n");

      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      // Only the real source file survives; the noise is filtered from display.
      assertEquals(diffs[0].files.map((f) => f.path), ["real.py"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

// ---- clonefile-backed changes (self-skips without cp/git) ------------------

Deno.test({
  name: "changes: clonefile diff + apply copies the approved edit back",
  ignore: !cpAvailable,
  fn: async () => {
    const base = await Deno.makeTempDir({ prefix: "snap-" });
    const orig = await Deno.makeTempFile({ prefix: "cfg-" });
    await Deno.writeTextFile(orig, "v1\n");
    const c = ctx({ snapshotBase: base });
    const h = createHandler(c);
    try {
      const s = await (await h(jsonReq("POST", "/sessions", { title: "s" }))).json() as Session;
      await clonefile.snapshotPaths(s.id, [orig], base);
      // agent edits the clone (not the original)
      await Deno.writeTextFile(`${clonefile.sessionDir(s.id, base)}${orig}`, "v2\n");

      const events: BoughEvent[] = [];
      c.bus.subscribe((e) => events.push(e));

      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(diffs.length, 1);
      assertEquals(diffs[0].source, "clonefile");
      assertEquals(diffs[0].files.map((f) => f.path), [orig]);
      assertEquals(await Deno.readTextFile(orig), "v1\n"); // original still pristine

      const applied = await h(
        jsonReq("POST", `/sessions/${s.id}/changes/apply`, { source: "clonefile", paths: [orig] }),
      );
      assertEquals(applied.status, 200);
      assertEquals(await Deno.readTextFile(orig), "v2\n"); // approved edit copied back
      assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));
    } finally {
      await Deno.remove(base, { recursive: true });
      await Deno.remove(orig).catch(() => {});
      c.db.close();
    }
  },
});

async function exists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}
