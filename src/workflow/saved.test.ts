/**
 * Saved workflows at the filesystem level: what gets written, what gets listed, and
 * which version of a run's script "save this run" actually saves.
 *
 * The confinement acceptance criterion — a name cannot escape `~/.bough/workflows/saved`
 * — lives in `report.test.ts` with the rest of T5.8's criteria, and is asserted through
 * the route as well as the function. What this file adds is the half that decides
 * whether a saved workflow is the right SCRIPT: `saveRunAs` prefers the mirror the user
 * edited over the stored row, because the run whose result they liked is the one that
 * ran the edit, and saving the row would silently save the version they replaced.
 *
 * Hermetic and offline: no database beyond an in-memory one, no network, and
 * `BOUGH_HOME` relocated for every call that touches a path.
 *
 * Assertions come from `node:assert/strict`: jsr.io is denied by this environment's
 * egress policy, so `@std/assert` cannot resolve.
 */
import assert from "node:assert/strict";
import { openDb, type SqliteDb } from "../db/db.ts";
import { NotFoundError } from "../errors.ts";
import { mirrorScript } from "./journal.ts";
import {
  deleteSavedWorkflow,
  ensureSavedDir,
  listSavedWorkflows,
  normalizeName,
  readSavedWorkflow,
  saveRunAs,
  savedDir,
  saveWorkflow,
} from "./saved.ts";

const META = "export const meta = { name: 'branch-review', description: 'review a branch' }\n";

async function withHome<T>(fn: (home: string) => Promise<T>): Promise<T> {
  const home = await Deno.makeTempDir({ prefix: "bough-saved-" });
  const prior = Deno.env.get("BOUGH_HOME");
  Deno.env.set("BOUGH_HOME", home);
  try {
    return await fn(home);
  } finally {
    if (prior === undefined) Deno.env.delete("BOUGH_HOME");
    else Deno.env.set("BOUGH_HOME", prior);
    await Deno.remove(home, { recursive: true }).catch(() => {});
  }
}

/** A run row with a script, and nothing else the saved surface reads. */
function seedRun(db: SqliteDb, script: string): string {
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "s",
    kind: "root",
    createdAt: 1,
    parentId: null,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
  const id = crypto.randomUUID();
  db.createWorkflow({
    id,
    sessionId: session.id,
    name: "branch-review",
    description: "review a branch",
    script,
    phases: [],
    status: "done",
    currentPhase: null,
    result: null,
    error: null,
    args: null,
    resumeOf: null,
    createdAt: 1,
    finishedAt: 2,
  });
  return id;
}

Deno.test("saving a run saves the mirror the user edited, not the stored row", async () => {
  const db = openDb(":memory:");
  try {
    await withHome(async () => {
      const runId = seedRun(db, META + "return await agent('review the row version')");
      await mirrorScript(runId, META + "return await agent('review the EDITED version')");

      const saved = await saveRunAs(db, runId, "branch-review");
      assert.equal(saved.name, "branch-review");
      assert.equal(saved.description, "review a branch");
      assert.ok(saved.path.startsWith(savedDir() + "/"));

      const read = await readSavedWorkflow("branch-review");
      assert.match(read.script, /EDITED version/);
      assert.ok(read.bytes > 0);

      // No mirror on disk: the row is the fallback, so a cleaned ~/.bough still saves.
      const bare = seedRun(db, META + "return await agent('only the row')");
      const second = await saveRunAs(db, bare, "row-only");
      assert.match((await readSavedWorkflow(second.name)).script, /only the row/);

      await assert.rejects(() => saveRunAs(db, "no-such-run", "x"), NotFoundError);
    });
  } finally {
    db.close();
  }
});

Deno.test("saving is idempotent on the name, and listing carries meta.description", async () => {
  await withHome(async () => {
    assert.equal(await ensureSavedDir(), 0);
    await saveWorkflow("branch-review", META + "return 1");
    await saveWorkflow("branch-review", META + "return 2");
    await saveWorkflow("zzz-last", "return 3"); // no meta: listing still works

    const listed = await listSavedWorkflows();
    assert.deepEqual(listed.map((s) => s.name), ["branch-review", "zzz-last"]);
    assert.deepEqual(listed.map((s) => s.description), ["review a branch", ""]);
    assert.match((await readSavedWorkflow("branch-review")).script, /return 2/);
    assert.equal(await ensureSavedDir(), 2);

    assert.equal(await deleteSavedWorkflow("zzz-last"), true);
    assert.equal(await deleteSavedWorkflow("zzz-last"), false, "deleting twice is not an error");
    await assert.rejects(() => readSavedWorkflow("zzz-last"), NotFoundError);
  });
});

Deno.test("a name is normalized once: one trailing .js, trimmed, never doubled", () => {
  assert.equal(normalizeName("  branch-review  "), "branch-review");
  assert.equal(normalizeName("branch-review.js"), "branch-review");
  assert.equal(normalizeName("branch-review.JS"), "branch-review");
  assert.equal(normalizeName("a.js.js"), "a.js");
  assert.equal(normalizeName(undefined), "");
});
