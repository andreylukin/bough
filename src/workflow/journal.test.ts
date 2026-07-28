/**
 * The journal's on-disk half — the script mirror and the script a relaunch resolves —
 * proved through the REAL workflow worker, with a counting fake
 * `AgentRunner` standing in for the subagents (plan §7 — "Subagents/workflows | Fake
 * LLM + real orchestration").
 *
 * The test this file exists for is the "edit the file, relaunch" loop, and it is an
 * invariant rather than a convenience: a relaunch that ran the STORED row instead of
 * the edited mirror would look like a working relaunch that quietly ignored the fix,
 * which is the same failure shape as a stale replay — wrong output, no error, nothing
 * to notice it by. So the edit is made on disk, with no request body, and the assertion
 * is that exactly the edited call ran live.
 *
 * WHAT MOVED OUT (T5.8). This file used to test a second journal that nothing called: a
 * duplicate `callKey`, a duplicate replay index, a duplicate rerun boundary and a
 * duplicate rerun report. That surface is deleted, so the tests for it are too — the
 * behaviours they covered are asserted against the LIVE path instead:
 *   - the key, and prefix-bounded replay — `workflow/run.test.ts`;
 *   - what a relaunch replayed and what it ran live — `workflow/report.test.ts`;
 *   - the relaunch boundary — `workflow/control.test.ts`.
 * Deleting a test whose subject is gone is not lost coverage; keeping it would have
 * been coverage of code no request can reach.
 *
 * Hermetic and offline: an in-memory database, a real bus, no network, no key, and
 * `BOUGH_HOME` pointed at a temp dir for the duration of every engine call, so the
 * script mirror never touches the real `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`, which is not a
 * dependency of this repo.
 */
import { test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { PathError } from "../errors.ts";
import type { WorkflowRun } from "../schema/parts.ts";
import {
  mirrorPath,
  mirrorScript,
  readMirror,
  resolveRerunScript,
  syncScriptMirrors,
} from "./journal.ts";
import {
  type AgentCall,
  type AgentRunner,
  rerunWorkflow,
  startWorkflow,
  type WorkflowCtx,
} from "./run.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

interface Harness {
  db: SqliteDb;
  bus: Bus;
  sessionId: string;
  home: string;
  close(): void;
}

function harness(): Harness {
  const db = openDb(":memory:");
  const bus = new Bus();
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "the orchestrator",
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
  const home = mkdtempSync(join(tmpdir(), "bough-journal-"));
  return {
    db,
    bus,
    sessionId: session.id,
    home,
    close() {
      db.close();
      try {
        rmSync(home, { recursive: true, force: true });
      } catch { /* already gone */ }
    },
  };
}

/**
 * Relocate `BOUGH_HOME` for one call and put it back. Around the call rather than for
 * the file: every path accessor reads the variable live, so a file that mutated it
 * globally would reach into every other test in the process.
 */
async function withHome<T>(home: string, fn: () => Promise<T>): Promise<T> {
  const prior = process.env["BOUGH_HOME"];
  process.env["BOUGH_HOME"] = home;
  try {
    return await fn();
  } finally {
    if (prior === undefined) delete process.env["BOUGH_HOME"];
    else process.env["BOUGH_HOME"] = prior;
  }
}

/** Resolves with the run row the first time a run reaches a terminal status. */
function completion(bus: Bus, ms = 20_000): Promise<WorkflowRun> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(new Error(`workflow did not finish within ${ms}ms`));
    }, ms);
    const off = bus.subscribe((e) => {
      if (e.type !== "workflow.updated") return;
      const run = e.data as WorkflowRun;
      if (run.status === "running" || run.status === "paused") return;
      clearTimeout(timer);
      off();
      resolve(run);
    });
  });
}

/** A runner that answers with its own prompt and records every call it was asked. */
function recorder(): { runner: AgentRunner; calls: string[] } {
  const calls: string[] = [];
  const runner: AgentRunner = (call: AgentCall) => {
    calls.push(call.prompt);
    return Promise.resolve(`report: ${call.prompt}`);
  };
  return { runner, calls };
}

/** Start a run and wait for it to finish. */
async function run(h: Harness, runner: AgentRunner, script: string): Promise<WorkflowRun> {
  const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
  const done = completion(h.bus);
  await withHome(h.home, () =>
    startWorkflow(ctx, {
      sessionId: h.sessionId,
      script,
      meta: { name: "journal-test", description: "a journal test workflow" },
      concurrency: 4,
    }));
  return await done;
}

/** Relaunch through the LIVE path, and wait for the new run to finish. */
async function rerunAndWait(
  h: Harness,
  runner: AgentRunner,
  id: string,
  script?: string,
): Promise<WorkflowRun> {
  const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
  const done = completion(h.bus);
  await withHome(
    h.home,
    () => rerunWorkflow(ctx, id, script === undefined ? {} : { script }),
  );
  return await done;
}

/** `prompt:status` per journal row, in call order — the shape every AC asserts on. */
function journal(db: SqliteDb, runId: string): string[] {
  return db.listWorkflowAgents(runId).map((a) => `${a.prompt}:${a.status}`);
}

// ---------------------------------------------------------------------------
// the on-disk mirror — the "edit the file, relaunch" loop
// ---------------------------------------------------------------------------

test("a relaunch with no script runs the mirror the user edited", async () => {
  const h = harness();
  try {
    const first = await run(
      h,
      recorder().runner,
      `
        const a = await agent('review a.ts')
        const b = await agent('review b.ts')
        return [a, b]
      `,
    );
    const path = await withHome(h.home, () => Promise.resolve(mirrorPath(first.id)));
    assert.ok((await readFile(path, "utf8")).includes("review a.ts"), "the run mirrored itself");

    // The user edits the file on disk — no request body, no re-POST.
    await writeFile(
      path,
      `
        const a = await agent('review a.ts')
        const b = await agent('review b.ts twice as hard')
        return [a, b]
      `,
    );

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, first.id);

    assert.deepEqual(live.calls, ["review b.ts twice as hard"]);
    assert.deepEqual(journal(h.db, second.id), [
      "review a.ts:cached",
      "review b.ts twice as hard:done",
    ]);
  } finally {
    h.close();
  }
});

test("a relaunch falls back to the stored script when the mirror is gone", async () => {
  const h = harness();
  try {
    const first = await run(h, recorder().runner, `return await agent('review a.ts')`);
    await withHome(h.home, async () => {
      await rm(mirrorPath(first.id));
      assert.deepEqual(await resolveRerunScript(first), {
        script: first.script,
        from: "stored",
      });
    });

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, first.id);
    assert.deepEqual(live.calls, [], "the stored script is the same script — still zero calls");
    assert.equal(journal(h.db, second.id)[0], "review a.ts:cached");
  } finally {
    h.close();
  }
});

test("syncScriptMirrors writes what is missing and never touches what is there", async () => {
  const h = harness();
  try {
    const first = await run(h, recorder().runner, `return await agent('review a.ts')`);
    await withHome(h.home, async () => {
      const path = mirrorPath(first.id);
      await rm(path);

      // Boot with the file gone: it comes back from the row.
      assert.deepEqual(await syncScriptMirrors(h.db), [first.id]);
      assert.equal(await readFile(path, "utf8"), first.script);

      // Boot again with the user's edit in place: untouched, or the restart would
      // replay the edit away on the next relaunch.
      await writeFile(path, "// mine\nreturn await agent('review a.ts')");
      assert.deepEqual(await syncScriptMirrors(h.db), [], "an existing mirror is never rewritten");
      assert.equal(
        await readFile(path, "utf8"),
        "// mine\nreturn await agent('review a.ts')",
      );
    });
  } finally {
    h.close();
  }
});

test("a run id cannot name a file outside the workflows directory", () => {
  const home = mkdtempSync(join(tmpdir(), "bough-journal-"));
  const prior = process.env["BOUGH_HOME"];
  process.env["BOUGH_HOME"] = home;
  try {
    assert.throws(() => mirrorPath("../../etc/crontab"), PathError);
    assert.throws(() => mirrorPath("/etc/crontab"), PathError);
    assert.ok(mirrorPath("a-b-c").endsWith("/workflows/a-b-c.js"));
  } finally {
    if (prior === undefined) delete process.env["BOUGH_HOME"];
    else process.env["BOUGH_HOME"] = prior;
    rmSync(home, { recursive: true, force: true });
  }
});

test("mirrorScript and readMirror round-trip, and a missing mirror reads null", async () => {
  const home = await mkdtemp(join(tmpdir(), "bough-journal-"));
  await withHome(home, async () => {
    assert.equal(await readMirror("nope"), null);
    assert.equal(await mirrorScript("run-1", "return 1"), true);
    assert.equal(await readMirror("run-1"), "return 1");
  });
  await rm(home, { recursive: true, force: true });
});

// ---------------------------------------------------------------------------
// which script a relaunch runs
// ---------------------------------------------------------------------------

test("script provenance: explicit wins, then the mirror, then the stored row", async () => {
  const h = harness();
  try {
    const finished = await run(h, recorder().runner, `return await agent('a')`);
    await withHome(h.home, async () => {
      assert.deepEqual(await resolveRerunScript(finished, "return 1"), {
        script: "return 1",
        from: "explicit",
      });
      // A blank override is not an override: it is what a form posts when the user
      // cleared the box, and running nothing is not what they asked for.
      assert.deepEqual(await resolveRerunScript(finished, "   "), {
        script: finished.script,
        from: "mirror",
      });
      await writeFile(mirrorPath(finished.id), "// edited\nreturn await agent('a')");
      assert.deepEqual(await resolveRerunScript(finished), {
        script: "// edited\nreturn await agent('a')",
        from: "mirror",
      });
    });
  } finally {
    h.close();
  }
});
