import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import * as jj from "../vcs/jj.ts";
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
const jjAvailable = (await canRun("jj")) && (await canRun("git")) &&
  await jj.version().then(() => true).catch(() => false);
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

Deno.test("changes endpoints 404 on unknown session; revert 400 without a jj workspace", async () => {
  const c = ctx();
  const h = createHandler(c);
  assertEquals((await h(jsonReq("GET", "/sessions/nope/changes"))).status, 404);
  assertEquals(
    (await h(jsonReq("POST", "/sessions/nope/changes/apply", { source: "jj", paths: [] }))).status,
    404,
  );
  assertEquals((await h(jsonReq("POST", "/sessions/nope/changes/revert", {}))).status, 404);
  // a real session with no jj workspace can't be reverted — or jj-applied
  const s = await (await h(jsonReq("POST", "/sessions", { title: "s" }))).json() as Session;
  assertEquals((await h(jsonReq("POST", `/sessions/${s.id}/changes/revert`, {}))).status, 400);
  assertEquals(
    (await h(jsonReq("POST", `/sessions/${s.id}/changes/apply`, { source: "jj", paths: [] })))
      .status,
    400,
  );
  c.db.close();
});

// ---- jj-backed changes (self-skips without jj) -----------------------------

Deno.test({
  name: "changes: jj apply accepts the change — files stay, the rail clears",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
        .json() as Session;
      await jj.ensureWorkspace(repo, s.id); // what the turn runner does on first turn
      await Deno.writeTextFile(`${repo}/new.txt`, "hi\n");

      const events: BoughEvent[] = [];
      c.bus.subscribe((e) => events.push(e));

      // diff shows the new file, tagged source jj
      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(diffs.length, 1);
      assertEquals(diffs[0].source, "jj");
      assertEquals(diffs[0].files.map((f) => f.path), ["new.txt"]);

      // apply = accept & advance: file stays on disk, the session diff resets to
      // empty (the change is sealed, the bookmark moved to a fresh child), and
      // changes.updated is emitted so the rail refetches.
      const applied = await h(
        jsonReq("POST", `/sessions/${s.id}/changes/apply`, { source: "jj", paths: ["new.txt"] }),
      );
      assertEquals(applied.status, 200);
      assert(await Deno.stat(`${repo}/new.txt`).then(() => true).catch(() => false));
      const after = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(after.diffs[0]?.files ?? [], []);
      assert(events.some((e) => e.type === "changes.updated" && e.sessionId === s.id));

      // post-accept edits diff cleanly on top — only the new work shows
      await Deno.writeTextFile(`${repo}/next.txt`, "more\n");
      const d2 = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(d2.diffs[0].files.map((f) => f.path), ["next.txt"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
      c.db.close();
    }
  },
});

Deno.test({
  name: "changes: jj revert (whole-change) undoes the session's edit",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
        .json() as Session;
      await jj.ensureWorkspace(repo, s.id);
      await Deno.writeTextFile(`${repo}/new.txt`, "hi\n");

      const events: BoughEvent[] = [];
      c.bus.subscribe((e) => events.push(e));

      // reading the diff snapshots the edit; revert then undoes it whole-change
      const { diffs } = await (await h(jsonReq("GET", `/sessions/${s.id}/changes`))).json() as {
        diffs: Diff[];
      };
      assertEquals(diffs[0].files.map((f) => f.path), ["new.txt"]);
      const reverted = await h(jsonReq("POST", `/sessions/${s.id}/changes/revert`, {}));
      assertEquals(reverted.status, 200);
      assertEquals(await Deno.stat(`${repo}/new.txt`).then(() => true).catch(() => false), false);
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
  name: "changes: jj per-path revert reverts only the selected file",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    const c = ctx();
    const h = createHandler(c);
    try {
      const s = await (await h(jsonReq("POST", "/sessions", { title: "s", workspace: repo })))
        .json() as Session;
      await jj.ensureWorkspace(repo, s.id);
      // Two edited files in the session's change.
      await Deno.writeTextFile(`${repo}/a.txt`, "a-work\n");
      await Deno.writeTextFile(`${repo}/b.txt`, "b-work\n");

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
      assertEquals(await reverted.json(), { ok: true, reverted: "jj", paths: ["a.txt"] });

      // a.txt is gone from disk; b.txt survives.
      assertEquals(await Deno.stat(`${repo}/a.txt`).then(() => true).catch(() => false), false);
      assertEquals(await Deno.readTextFile(`${repo}/b.txt`), "b-work\n");

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
