/**
 * Shadow backend end-to-end through the real entry points: prepareWorkspace
 * → sessionChanges → applyChanges → revertChanges, on a real
 * git repo in a temp dir. The jj-era paths are untouched; this exercises the
 * flag-gated wiring of docs/shadow-snapshots.md phase 2.
 */
import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { prepareWorkspace } from "../supervisor/workspace.ts";
import { applyChanges, revertChanges, sessionChanges } from "../server/changes.ts";
import * as shadow from "./shadow.ts";

async function sh(cwd: string, bin: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command(bin, { args, cwd, stdout: "piped", stderr: "piped" }).output();
  if (r.code !== 0) {
    throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(r.stderr)}`);
  }
  return new TextDecoder().decode(r.stdout);
}

Deno.test("shadow flow: prepare → edit → changes → apply → revert", async () => {
  const repo = await Deno.makeTempDir({ prefix: "bough-flow-origin-" });
  await sh(repo, "git", "init", "-q", "-b", "main");
  await sh(repo, "git", "config", "user.name", "t");
  await sh(repo, "git", "config", "user.email", "t@t");
  await Deno.writeTextFile(`${repo}/app.txt`, "v1\n");
  await sh(repo, "git", "add", "-A");
  await sh(repo, "git", "commit", "-q", "-m", "init");

  const env = {
    BOUGH_SHADOW_BASE: await Deno.makeTempDir({ prefix: "bough-flow-shadow-" }),
    BOUGH_SUBAGENT_BASE: await Deno.makeTempDir({ prefix: "bough-flow-ws-" }),
    BOUGH_SNAPSHOT_BASE: await Deno.makeTempDir({ prefix: "bough-flow-snap-" }),
    BOUGH_SCRATCH_BASE: await Deno.makeTempDir({ prefix: "bough-flow-scratch-" }),
  };
  const prev = new Map<string, string | undefined>();
  for (const [k, v] of Object.entries(env)) {
    prev.set(k, Deno.env.get(k));
    Deno.env.set(k, v);
  }
  const db = new Db(":memory:");
  try {
    db.createSession({
      id: "f1",
      parentId: null,
      title: "flow test",
      kind: "root",
      createdAt: 1,
      workspace: repo,
    });
    // First turn: the session gets an isolated shadow worktree; origin untouched.
    const p = await prepareWorkspace(db, "f1");
    assertEquals(p.cwd, shadow.workspaceDirFor("f1"));
    assertEquals(p.warning, undefined);
    assertEquals(db.getSessionRuntime("f1").workspace, p.cwd);
    assertEquals((await sh(repo, "git", "status", "--porcelain")).trim(), "");

    // Resume runs where the column points.
    const p2 = await prepareWorkspace(db, "f1");
    assertEquals(p2.cwd, p.cwd);

    // Agent edits in the worktree → changes rail shows a shadow diff.
    await Deno.writeTextFile(`${p.cwd}/app.txt`, "v2\n");
    await Deno.writeTextFile(`${p.cwd}/new.txt`, "new\n");
    const diffs = await sessionChanges(db, "f1");
    assertEquals(diffs.length, 1);
    assertEquals(diffs[0].source, "shadow");
    assertEquals(diffs[0].files.map((f) => f.path).sort(), ["app.txt", "new.txt"]);

    // Partial apply delivers one path to the origin; rail keeps the rest.
    const r1 = await applyChanges(db, "f1", { source: "shadow", paths: ["new.txt"] });
    assertEquals(r1.applied, ["new.txt"]);
    assertEquals(r1.origin, await Deno.realPath(repo));
    assertEquals(r1.sealed, false);
    assertEquals(await Deno.readTextFile(`${repo}/new.txt`), "new\n");
    assertEquals(await Deno.readTextFile(`${repo}/app.txt`), "v1\n");

    // Full apply covers everything → sealed, rail clears.
    const r2 = await applyChanges(db, "f1", { source: "shadow", paths: [] });
    assert(r2.sealed);
    assertEquals(await Deno.readTextFile(`${repo}/app.txt`), "v2\n");
    assertEquals((await sessionChanges(db, "f1"))[0].files, []);

    // New edit, whole-change revert through the API.
    await Deno.writeTextFile(`${p.cwd}/oops.txt`, "oops\n");
    const reverted = await revertChanges(db, "f1");
    assertEquals(reverted, ["oops.txt"]);
    assertEquals((await sessionChanges(db, "f1"))[0].files, []);
  } finally {
    db.close();
    for (const [k, v] of prev) v === undefined ? Deno.env.delete(k) : Deno.env.set(k, v);
    for (const d of Object.values(env).slice(1)) {
      await Deno.remove(d, { recursive: true }).catch(() => {});
    }
    await Deno.remove(repo, { recursive: true }).catch(() => {});
  }
});
