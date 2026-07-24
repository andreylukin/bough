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

/** The diff substrate is agentfs now, so edits are made through a real overlay run
 *  (they land in the session delta, not the worktree disk); skip where it's absent. */
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

async function overlayEdit(sid: string, dir: string, cmd: string): Promise<void> {
  const r = await new Deno.Command("agentfs", {
    args: ["run", "--session", sid, "--", "/bin/sh", "-c", cmd],
    cwd: dir,
    stdout: "null",
    stderr: "null",
  }).output();
  if (r.code !== 0) throw new Error(`overlay edit failed (${r.code})`);
}

Deno.test({
  name: "shadow flow: prepare → edit → changes → apply → revert",
  ignore: !AGENTFS,
  fn: async () => {
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
      // agentfs keys its delta dirs off HOME; isolate it so the diff substrate finds
      // the same path the overlay runs created, and the temp HOME reaps the deltas.
      HOME: await Deno.makeTempDir({ prefix: "bough-flow-home-" }),
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

      // Agent edits through the overlay → changes rail shows the agentfs diff.
      await overlayEdit("f1", p.cwd, `printf 'v2\n' > app.txt; printf 'new\n' > new.txt`);
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
      await overlayEdit("f1", p.cwd, `printf 'oops\n' > oops.txt`);
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
  },
});
