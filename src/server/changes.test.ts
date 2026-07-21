import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import * as shadow from "../vcs/shadow.ts";
import * as clonefile from "../vcs/clonefile.ts";
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

/** Shadow store + workspace roots in temp dirs for the duration of `fn`. */
async function withShadowRoots(fn: () => Promise<void>): Promise<void> {
  const shadowBase = await Deno.makeTempDir({ prefix: "chg-shadow-" });
  const wsBase = await Deno.makeTempDir({ prefix: "chg-ws-" });
  const prev = new Map<string, string | undefined>([
    ["BOUGH_SHADOW_BASE", Deno.env.get("BOUGH_SHADOW_BASE")],
    ["BOUGH_SUBAGENT_BASE", Deno.env.get("BOUGH_SUBAGENT_BASE")],
  ]);
  Deno.env.set("BOUGH_SHADOW_BASE", shadowBase);
  Deno.env.set("BOUGH_SUBAGENT_BASE", wsBase);
  try {
    await fn();
  } finally {
    for (const [k, v] of prev) v === undefined ? Deno.env.delete(k) : Deno.env.set(k, v);
    await Deno.remove(shadowBase, { recursive: true }).catch(() => {});
    await Deno.remove(wsBase, { recursive: true }).catch(() => {});
  }
}

/** What the turn runner does on first turn: worktree + repointed workspace column. */
async function attachWorkspace(db: Db, repo: string, sessionId: string): Promise<string> {
  const dir = await shadow.createSessionWorkspace(repo, sessionId);
  db.setSessionWorkspace(sessionId, dir);
  return dir;
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
    (await h(jsonReq("POST", "/sessions/nope/changes/apply", { source: "shadow", paths: [] })))
      .status,
    404,
  );
  assertEquals((await h(jsonReq("POST", "/sessions/nope/changes/revert", {}))).status, 404);
  // a real session with no snapshot workspace can't be reverted — or applied
  const s = await (await h(jsonReq("POST", "/sessions", { title: "s" }))).json() as Session;
  assertEquals((await h(jsonReq("POST", `/sessions/${s.id}/changes/revert`, {}))).status, 400);
  assertEquals(
    (await h(jsonReq("POST", `/sessions/${s.id}/changes/apply`, { source: "shadow", paths: [] })))
      .status,
    400,
  );
  c.db.close();
});

// ---- shadow-backed changes (self-skips without git) -------------------------

Deno.test({
  name: "changes: shadow apply materializes into the origin and seals — rail clears",
  ignore: !gitAvailable,
  fn: async () => {
    await withShadowRoots(async () => {
      const repo = await tempGitRepo();
      const c = ctx();
      const h = createHandler(c);
      try {
        const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
          .json() as Session;
        const dir = await attachWorkspace(c.db, repo, s.id);
        await Deno.writeTextFile(`${dir}/new.txt`, "hi\n");

        const events: BoughEvent[] = [];
        c.bus.subscribe((e) => events.push(e));

        // diff shows the new file, tagged source shadow
        const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(diffs.length, 1);
        assertEquals(diffs[0].source, "shadow");
        assertEquals(diffs[0].files.map((f) => f.path), ["new.txt"]);

        // apply = materialize into the origin + seal: the file lands in the repo's
        // working tree, the session diff resets to empty, changes.updated fires.
        const applied = await h(
          jsonReq("POST", `/sessions/${s.id}/changes/apply`, {
            source: "shadow",
            paths: ["new.txt"],
          }),
        );
        assertEquals(applied.status, 200);
        assertEquals(await Deno.readTextFile(`${repo}/new.txt`), "hi\n");
        const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(after.diffs[0]?.files ?? [], []);
        assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));

        // post-seal edits diff cleanly on top — only the new work shows
        await Deno.writeTextFile(`${dir}/next.txt`, "more\n");
        const d2 = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(d2.diffs[0].files.map((f) => f.path), ["next.txt"]);
      } finally {
        await Deno.remove(repo, { recursive: true });
        c.db.close();
      }
    });
  },
});

Deno.test({
  name: "changes: shadow revert (whole-change) undoes the session's edit",
  ignore: !gitAvailable,
  fn: async () => {
    await withShadowRoots(async () => {
      const repo = await tempGitRepo();
      const c = ctx();
      const h = createHandler(c);
      try {
        const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
          .json() as Session;
        const dir = await attachWorkspace(c.db, repo, s.id);
        await Deno.writeTextFile(`${dir}/new.txt`, "hi\n");

        const events: BoughEvent[] = [];
        c.bus.subscribe((e) => events.push(e));

        // reading the diff snapshots the edit; revert then undoes it whole-change
        const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(diffs[0].files.map((f) => f.path), ["new.txt"]);
        const reverted = await h(jsonReq("POST", `/sessions/${s.id}/changes/revert`, {}));
        assertEquals(reverted.status, 200);
        assertEquals(await Deno.stat(`${dir}/new.txt`).then(() => true).catch(() => false), false);
        const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(after.diffs[0]?.files ?? [], []);
        assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));
      } finally {
        await Deno.remove(repo, { recursive: true });
        c.db.close();
      }
    });
  },
});

Deno.test({
  name: "changes: shadow per-path revert reverts only the selected file",
  ignore: !gitAvailable,
  fn: async () => {
    await withShadowRoots(async () => {
      const repo = await tempGitRepo();
      const c = ctx();
      const h = createHandler(c);
      try {
        const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
          .json() as Session;
        const dir = await attachWorkspace(c.db, repo, s.id);
        // Two edited files in the session's change.
        await Deno.writeTextFile(`${dir}/a.txt`, "a-work\n");
        await Deno.writeTextFile(`${dir}/b.txt`, "b-work\n");

        const events: BoughEvent[] = [];
        c.bus.subscribe((e) => events.push(e));

        // Snapshot both via the diff read.
        const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(diffs[0].files.map((f) => f.path).sort(), ["a.txt", "b.txt"]);

        // Per-path revert of a.txt only.
        const reverted = await h(
          jsonReq("POST", `/sessions/${s.id}/changes/revert`, { paths: ["a.txt"] }),
        );
        assertEquals(reverted.status, 200);
        assertEquals(await reverted.json(), { ok: true, reverted: "shadow", paths: ["a.txt"] });

        // a.txt is gone from the worktree; b.txt survives.
        assertEquals(await Deno.stat(`${dir}/a.txt`).then(() => true).catch(() => false), false);
        assertEquals(await Deno.readTextFile(`${dir}/b.txt`), "b-work\n");

        // The diff shrank to just b.txt.
        const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(after.diffs[0].files.map((f) => f.path), ["b.txt"]);
        assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));
      } finally {
        await Deno.remove(repo, { recursive: true });
        c.db.close();
      }
    });
  },
});

Deno.test({
  name: "changes: a subagent's unadopted branch surfaces in the spawner's rail; adopt clears it",
  ignore: !gitAvailable,
  fn: async () => {
    await withShadowRoots(async () => {
      const repo = await tempGitRepo();
      const c = ctx();
      const h = createHandler(c);
      try {
        const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
          .json() as Session;
        const dir = await attachWorkspace(c.db, repo, s.id);

        // A finished subagent with a branched worktree and an un-adopted edit —
        // what launch() sets up, minus the turn.
        c.db.createSession({
          id: "sub1",
          parentId: null,
          title: "essay writer",
          kind: "subagent",
          createdAt: 1,
          originId: s.id,
          originMessageId: "m1",
        });
        const subDir = await shadow.addWorkspace(dir, "sub1", shadow.workspaceDirFor("sub1"), s.id);
        c.db.setSessionWorkspace("sub1", subDir);
        await Deno.writeTextFile(`${subDir}/essay.txt`, "words\n");

        // The SPAWNER's rail carries the subagent's diff as a labeled section.
        const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        const sub = diffs.find((d) => d.subagentId === "sub1");
        assert(sub, "expected an unadopted subagent section");
        assertEquals(sub.label, "essay writer (unadopted)");
        assertEquals(sub.files.map((f) => f.path), ["essay.txt"]);

        const events: BoughEvent[] = [];
        c.bus.subscribe((e) => events.push(e));

        // Adopt (happy path): the branch folds into the spawner's worktree, both
        // rails move, and the subagent section drops out of the spawner's rail.
        const adopted = await h(jsonReq("POST", "/sessions/sub1/adopt"));
        assertEquals(adopted.status, 200);
        assertEquals(await Deno.readTextFile(`${dir}/essay.txt`), "words\n");
        assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));
        assert(events.some((e) => e.type === "changes.updated" && e.sessionId === "sub1"));
        const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
          diffs: Diff[];
        };
        assertEquals(after.diffs.filter((d) => d.subagentId).length, 0);
        // The work now rides the spawner's own diff.
        assert(after.diffs.some((d) => d.files.some((f) => f.path === "essay.txt")));
      } finally {
        await Deno.remove(repo, { recursive: true });
        c.db.close();
      }
    });
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
