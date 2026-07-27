/**
 * Relaunching from a journal, and the one property that makes it safe: **replay is
 * prefix-bounded.**
 *
 * The test that matters here is the third one, and it asserts something that looks
 * like a regression until you know why. Editing call 3 of 6 does NOT re-run only call
 * 3: calls 4, 5 and 6 run live too, with their journal keys byte-identical to the
 * source run's. A key-matching cache would have served all three instantly. This engine
 * refuses, because workflow agents share ONE checkout — the key covers what an agent is
 * asked, not the filesystem it is asked about, and the moment call 3 runs live the tree
 * calls 4-6 were answered against no longer exists (spec §8, "Replay is prefix-bounded").
 * So the test pins the unchanged keys explicitly and then asserts those calls ran
 * anyway. Without that assertion the file would pass against a plain key cache, which
 * is the implementation this task exists to replace.
 *
 * The pause test is the other half of the steering model. There is no mid-run input: a
 * paused run keeps its dispatched agents and gates only NEW ones, so the work in flight
 * when you hit pause is finished, journaled, and REPLAYS on the next relaunch — while a
 * call that never started has no row and starts over. That asymmetry is the reason the
 * spec tells users to pause before stopping, and it is only true if a paused run really
 * does journal what it already dispatched.
 *
 * Hermetic and offline, in the shape `run.test.ts` established: an in-memory database,
 * a real bus, REAL `permissions: "none"` workers running the scripts, and a fake
 * `AgentRunner` in place of every subagent — no network, no key, no LLM. `BOUGH_HOME`
 * points at a temp dir for the duration of each engine call, so the script mirror never
 * touches the real `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is denied
 * by this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { createHandler } from "../server/app.ts";
import {
  type AgentCall,
  type AgentRunner,
  callKey,
  pauseWorkflow,
  startWorkflow,
  stopWorkflow,
  type WorkflowCtx,
} from "./run.ts";
import {
  type RelaunchDeps,
  relaunchLine,
  relaunchPreview,
  relaunchReport,
  relaunchWorkflow,
  type WithRelaunch,
} from "./relaunch.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

interface Harness {
  db: SqliteDb;
  bus: Bus;
  sessionId: string;
  events: BoughEvent[];
  home: string;
  close(): void;
}

function harness(): Harness {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => void events.push(e));
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "the orchestrator",
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
  const home = Deno.makeTempDirSync({ prefix: "bough-relaunch-" });
  return {
    db,
    bus,
    sessionId: session.id,
    events,
    home,
    close() {
      db.close();
      try {
        Deno.removeSync(home, { recursive: true });
      } catch { /* already gone */ }
    },
  };
}

/** Relocate `BOUGH_HOME` for one engine call, then put the environment back. */
async function withHome<T>(home: string, fn: () => Promise<T>): Promise<T> {
  const prior = Deno.env.get("BOUGH_HOME");
  Deno.env.set("BOUGH_HOME", home);
  try {
    return await fn();
  } finally {
    if (prior === undefined) Deno.env.delete("BOUGH_HOME");
    else Deno.env.set("BOUGH_HOME", prior);
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

const META = `export const meta = { name: 'audit', description: 'a relaunch fixture' }\n`;

/** A runner that reports its own prompt back and records what it was asked. */
function echoRunner(seen: string[] = []): AgentRunner {
  return (call: AgentCall) => {
    seen.push(call.prompt);
    return Promise.resolve(`report: ${call.prompt}`);
  };
}

/** Start a run directly on the engine and wait for it to finish. The source run. */
async function sourceRun(
  h: Harness,
  runner: AgentRunner,
  script: string,
  args?: unknown,
): Promise<WorkflowRun> {
  const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
  const done = completion(h.bus);
  await withHome(h.home, () =>
    startWorkflow(ctx, {
      sessionId: h.sessionId,
      script,
      meta: { name: "audit", description: "a relaunch fixture" },
      concurrency: 4,
      ...(args === undefined ? {} : { args }),
    }));
  return await done;
}

/**
 * The injection seam, wired to a fake runner. Production fills the same shape from
 * `workflow/control.ts` in `server/main.ts`; here it is one object literal, which is
 * the whole reason this test needs no subagent, no turn and no key.
 */
function deps(h: Harness, runner: AgentRunner): RelaunchDeps {
  return {
    ctxFor: () => ({
      workflowCtx: { db: h.db, bus: h.bus, runner },
      bind: () => {},
    }),
  };
}

function appCtx(h: Harness): AppCtx {
  return { db: h.db, bus: h.bus };
}

/** Relaunch and wait for the new run to finish. */
async function relaunch(
  h: Harness,
  sourceId: string,
  runner: AgentRunner,
  opts: { script?: string; args?: unknown } = {},
): Promise<{ run: WorkflowRun; finished: WorkflowRun }> {
  const done = completion(h.bus);
  const result = await withHome(
    h.home,
    () => relaunchWorkflow(appCtx(h), sourceId, opts, deps(h, runner)),
  );
  return { run: result.run, finished: await done };
}

function statuses(db: SqliteDb, runId: string): string[] {
  return db.listWorkflowAgents(runId).map((a) => `${a.prompt}:${a.status}`);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Wait for a journal predicate, bounded. Never a timing assertion — a settle. */
async function until(
  probe: () => boolean,
  what: string,
  ms = 10_000,
): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (probe()) return;
    await sleep(5);
  }
  throw new Error(`timed out waiting for ${what}`);
}

// The six-call fixture. Sequential awaits, so the call ORDER — which is what the
// prefix rule is defined over — is a fact about the script and not about scheduling.
function sixCalls(third: string): string {
  return `${META}
    const out = []
    out.push(await agent('review a.ts'))
    out.push(await agent('review b.ts'))
    out.push(await agent('${third}'))
    out.push(await agent('review d.ts'))
    out.push(await agent('review e.ts'))
    out.push(await agent('review f.ts'))
    return out
  `;
}

// ---------------------------------------------------------------------------
// an unchanged script
// ---------------------------------------------------------------------------

Deno.test("an unchanged script replays every call and issues zero live agent calls", async () => {
  const h = harness();
  try {
    const script = sixCalls("review c.ts");
    const source = await sourceRun(h, echoRunner(), script);
    assert.equal(source.status, "done");
    assert.equal(h.db.listWorkflowAgents(source.id).length, 6);

    const live: string[] = [];
    const { run, finished } = await relaunch(h, source.id, echoRunner(live), { script });

    assert.deepEqual(live, [], "an unchanged relaunch must not call a single agent");
    assert.equal(finished.status, "done");
    assert.deepEqual(finished.result, source.result, "the replayed run returns the same answers");
    const rows = h.db.listWorkflowAgents(run.id);
    assert.equal(rows.length, 6);
    assert.ok(rows.every((r) => r.status === "cached"), statuses(h.db, run.id).join(", "));

    const report = relaunchReport(h.db, run.id);
    assert.equal(report.replayed, 6);
    assert.equal(report.ranLive, 0);
    assert.equal(report.divergedAt, null, "nothing diverged, so the prefix covered the run");
    assert.equal(report.forced, 0);
    assert.equal(report.available, 6);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// THE prefix test
// ---------------------------------------------------------------------------

Deno.test(
  "editing call 3 of 6 replays 1-2 and runs 3-6 live, INCLUDING the calls whose key never changed",
  async () => {
    const h = harness();
    try {
      const source = await sourceRun(h, echoRunner(), sixCalls("review c.ts"));
      assert.equal(source.status, "done");
      const before = h.db.listWorkflowAgents(source.id);
      assert.deepEqual(before.map((r) => r.status), Array(6).fill("done"));

      const live: string[] = [];
      const { run, finished } = await relaunch(h, source.id, echoRunner(live), {
        script: sixCalls("review c.ts THOROUGHLY"),
      });

      // The edited call and everything after it ran. Nothing before it did.
      assert.deepEqual(live, [
        "review c.ts THOROUGHLY",
        "review d.ts",
        "review e.ts",
        "review f.ts",
      ]);
      assert.deepEqual(statuses(h.db, run.id), [
        "review a.ts:cached",
        "review b.ts:cached",
        "review c.ts THOROUGHLY:done",
        "review d.ts:done",
        "review e.ts:done",
        "review f.ts:done",
      ]);

      // ── THE ASSERTION THIS FILE EXISTS FOR ──────────────────────────────────
      // Calls 4, 5 and 6 were not edited. Their journal keys in the relaunched run
      // are byte-identical to the source run's keys at the same positions, and the
      // source has a stored answer for each. A key-matching cache would have served
      // all three for free. They ran anyway, because call 3 ran live first and the
      // agents share one checkout.
      const after = h.db.listWorkflowAgents(run.id);
      for (const idx of [3, 4, 5]) {
        assert.equal(
          after[idx].key,
          before[idx].key,
          `call ${idx} was not edited — its key must be unchanged`,
        );
        assert.notEqual(before[idx].result, null, "the source run answered it");
        assert.equal(
          after[idx].status,
          "done",
          `call ${idx} has an unchanged key AND a stored answer, and it must still have ` +
            `run live: replay stops at the first change and never resumes`,
        );
      }
      // Stated once more against the key function itself, so the test does not depend
      // on the engine having written the row it is reading.
      assert.equal(
        after[5].key,
        callKey({ prompt: "review f.ts", label: "review f.ts" }),
        "the last call's key is exactly what an unchanged script produces",
      );

      const report = relaunchReport(h.db, run.id);
      assert.equal(report.replayed, 2);
      assert.equal(report.ranLive, 4);
      assert.equal(report.divergedAt, 2, "replay stopped at the edited call");
      assert.equal(report.forced, 3, "three unchanged calls ran live behind the edit");
      assert.equal(report.available, 6);
      assert.match(relaunchLine(report), /replay stopped at call 2/);
      assert.match(relaunchLine(report), /3 unchanged calls ran live/);

      assert.deepEqual(finished.result, [
        "report: review a.ts",
        "report: review b.ts",
        "report: review c.ts THOROUGHLY",
        "report: review d.ts",
        "report: review e.ts",
        "report: review f.ts",
      ]);
    } finally {
      h.close();
    }
  },
);

Deno.test("a source call that failed ends the prefix — its successors re-run too", async () => {
  const h = harness();
  try {
    const script = `${META}
      const a = await agent('first')
      let b = null
      try { b = await agent('flaky') } catch { b = 'failed' }
      const c = await agent('third')
      return [a, b, c]
    `;
    const source = await sourceRun(
      h,
      (call) =>
        call.prompt === "flaky"
          ? Promise.reject(new Error("transient"))
          : Promise.resolve(`report: ${call.prompt}`),
      script,
    );
    assert.deepEqual(
      h.db.listWorkflowAgents(source.id).map((r) => r.status),
      ["done", "error", "done"],
    );

    const live: string[] = [];
    const { run } = await relaunch(h, source.id, echoRunner(live), { script });

    // 'third' has an unchanged key and a stored answer. It runs anyway: the failed
    // call before it re-ran live, and a re-run agent works in the same checkout.
    assert.deepEqual(live, ["flaky", "third"]);
    assert.deepEqual(statuses(h.db, run.id), ["first:cached", "flaky:done", "third:done"]);
    assert.equal(relaunchReport(h.db, run.id).forced, 1);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// pause, and what survives it
// ---------------------------------------------------------------------------

Deno.test(
  "a paused run's in-flight agent finishes and is journaled, so it replays on the next relaunch",
  async () => {
    const h = harness();
    try {
      const script = `${META}
        const a = await agent('the long one')
        const b = await agent('the one after it')
        return [a, b]
      `;
      let started = false;
      let release = () => {};
      const held = new Promise<void>((resolve) => (release = resolve));
      const seen: string[] = [];
      const runner: AgentRunner = async (call) => {
        seen.push(call.prompt);
        if (call.prompt === "the long one") {
          started = true;
          await held;
        }
        return `report: ${call.prompt}`;
      };

      const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
      const ended = completion(h.bus);
      const run = await withHome(h.home, () =>
        startWorkflow(ctx, {
          sessionId: h.sessionId,
          script,
          meta: { name: "audit", description: "a relaunch fixture" },
          concurrency: 4,
        }));

      await until(() => started, "the first agent to be dispatched");

      // Pause while that agent is in flight. Pause gates NEW calls only — it must not
      // reach the one already dispatched (spec §8).
      const paused = pauseWorkflow(ctx, run.id);
      assert.equal(paused.status, "paused");

      release();
      await until(
        () => h.db.listWorkflowAgents(run.id)[0]?.status === "done",
        "the in-flight agent to finish under the pause",
      );

      // The second call is parked on the gate: no agent, and no row, so the run view
      // never shows a "running" agent that has not started.
      await sleep(50);
      const during = h.db.listWorkflowAgents(run.id);
      assert.equal(during.length, 1, "a gated call must not journal a row");
      assert.equal(during[0].result, "report: the long one", "the finished agent is journaled");
      assert.deepEqual(seen, ["the long one"], "pause started nothing new");

      // Stop it: the script cannot be edited in place, so this is how a run is steered.
      stopWorkflow(ctx, run.id);
      const stopped = await ended;
      assert.equal(stopped.status, "stopped");
      assert.equal(
        h.db.listWorkflowAgents(run.id)[0].status,
        "done",
        "stopping must not undo work the pause preserved",
      );

      // The relaunch: the agent that finished under the pause replays, the one that
      // never started runs.
      const live: string[] = [];
      const { run: next, finished } = await relaunch(h, run.id, echoRunner(live), { script });
      assert.deepEqual(live, ["the one after it"], "only the call that never ran costs an agent");
      assert.deepEqual(statuses(h.db, next.id), [
        "the long one:cached",
        "the one after it:done",
      ]);
      assert.deepEqual(finished.result, ["report: the long one", "report: the one after it"]);
    } finally {
      h.close();
    }
  },
);

// ---------------------------------------------------------------------------
// a relaunch is a new run
// ---------------------------------------------------------------------------

Deno.test("a relaunch gets a new run id and leaves the source run's rows untouched", async () => {
  const h = harness();
  try {
    const script = sixCalls("review c.ts");
    const source = await sourceRun(h, echoRunner(), script);
    const beforeRun = h.db.getWorkflow(source.id)!;
    const beforeRows: WorkflowAgent[] = h.db.listWorkflowAgents(source.id);

    const { run } = await relaunch(h, source.id, echoRunner(), {
      script: sixCalls("review c.ts DIFFERENTLY"),
    });

    assert.notEqual(run.id, source.id, "a relaunch is a NEW run — nothing is rewritten");
    assert.equal(run.resumeOf, source.id);
    assert.equal(run.sessionId, source.sessionId);

    // The source is byte-identical afterwards: same status, same rows, same results,
    // same subagent sessions. History is a tree (spec §2.4).
    assert.deepEqual(h.db.getWorkflow(source.id), beforeRun);
    assert.deepEqual(h.db.listWorkflowAgents(source.id), beforeRows);
  } finally {
    h.close();
  }
});

Deno.test("a relaunch inherits the source run's args unless the caller replaces them", async () => {
  const h = harness();
  try {
    const script = `${META}
      const out = []
      for (const f of args.files) out.push(await agent('review ' + f))
      return out
    `;
    const source = await sourceRun(h, echoRunner(), script, { files: ["one", "two"] });
    assert.deepEqual(source.result, ["report: review one", "report: review two"]);

    const live: string[] = [];
    const { run, finished } = await relaunch(h, source.id, echoRunner(live), { script });
    assert.deepEqual(live, [], "the inherited args reproduce the same calls, so all replay");
    assert.deepEqual((h.db.getWorkflow(run.id))!.args, { files: ["one", "two"] });
    assert.deepEqual(finished.result, source.result);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// refusals and reporting
// ---------------------------------------------------------------------------

Deno.test("relaunching a run that is still live is refused, not raced", async () => {
  const h = harness();
  try {
    const script = `${META}
      const a = await agent('the long one')
      return [a]
    `;
    let release = () => {};
    const held = new Promise<void>((resolve) => (release = resolve));
    let started = false;
    const ctx: WorkflowCtx = {
      db: h.db,
      bus: h.bus,
      runner: async (call) => {
        started = true;
        await held;
        return call.prompt;
      },
    };
    const ended = completion(h.bus);
    const run = await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script,
        meta: { name: "audit", description: "a relaunch fixture" },
      }));
    await until(() => started, "the run to be in flight");

    await assert.rejects(
      () => withHome(h.home, () => relaunchWorkflow(appCtx(h), run.id, {}, deps(h, echoRunner()))),
      (err: Error) => {
        assert.match(err.message, /still running/);
        assert.match(err.message, /stop it first/);
        assert.match(err.message, /[Pp]ause before you stop/);
        return true;
      },
    );

    release();
    await ended;
  } finally {
    h.close();
  }
});

Deno.test("an unknown source run is a 404, not an empty replay", async () => {
  const h = harness();
  try {
    await assert.rejects(
      () => relaunchWorkflow(appCtx(h), "no-such-run", {}, deps(h, echoRunner())),
      /workflow no-such-run not found/,
    );
  } finally {
    h.close();
  }
});

Deno.test("an unwired relaunch seam fails loudly instead of running agentless", async () => {
  const h = harness();
  try {
    const source = await sourceRun(h, echoRunner(), sixCalls("review c.ts"));
    await assert.rejects(
      () => relaunchWorkflow(appCtx(h), source.id, {}),
      /not wired/,
    );
  } finally {
    h.close();
  }
});

Deno.test("the preview reports what the source journal offers, before anything runs", async () => {
  const h = harness();
  try {
    const script = `${META}
      const a = await agent('first')
      let b = null
      try { b = await agent('flaky') } catch { b = null }
      return [a, b]
    `;
    const source = await sourceRun(
      h,
      (call) =>
        call.prompt === "flaky"
          ? Promise.reject(new Error("transient"))
          : Promise.resolve(call.prompt),
      script,
    );
    const preview = relaunchPreview(h.db, source.id);
    assert.equal(preview.journaled, 2);
    assert.equal(preview.answers, 1);
    assert.equal(
      preview.replayablePrefix,
      1,
      "the prefix stops at the failed call even though a later one might have answered",
    );
  } finally {
    h.close();
  }
});

Deno.test("the replay report reads the same while a run is still in flight", async () => {
  const h = harness();
  try {
    const script = `${META}
      const a = await agent('first')
      const b = await agent('second')
      return [a, b]
    `;
    const source = await sourceRun(h, echoRunner(), script);

    let release = () => {};
    const held = new Promise<void>((resolve) => (release = resolve));
    const runner: AgentRunner = async (call) => {
      await held;
      return `report: ${call.prompt}`;
    };
    const done = completion(h.bus);
    const result = await withHome(
      h.home,
      () =>
        relaunchWorkflow(
          appCtx(h),
          source.id,
          { script: script.replace("'second'", "'second, revised'") },
          deps(h, runner),
        ),
    );
    await until(
      () => h.db.listWorkflowAgents(result.run.id).length === 2,
      "the diverged call to be journaled",
    );

    const mid = relaunchReport(h.db, result.run.id);
    assert.equal(mid.replayed, 1);
    assert.equal(mid.pending, 1, "a call still running is pending, never counted as paid");
    assert.equal(mid.final, false);
    assert.equal(mid.replayed + mid.ranLive + mid.pending, mid.total, "the buckets sum");

    release();
    await done;
    const end = relaunchReport(h.db, result.run.id);
    assert.equal(end.final, true);
    assert.equal(end.replayed, 1);
    assert.equal(end.ranLive, 1);
    assert.deepEqual(end.livePrompts, ["second, revised"]);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the HTTP surface
// ---------------------------------------------------------------------------

Deno.test("POST /workflows/:id/relaunch starts a new run and GET .../replay counts it", async () => {
  const h = harness();
  try {
    const script = sixCalls("review c.ts");
    const source = await sourceRun(h, echoRunner(), script);

    const live: string[] = [];
    const ctx = appCtx(h) as AppCtx & WithRelaunch;
    ctx.relaunch = deps(h, echoRunner(live));
    const handler = createHandler(ctx);

    const done = completion(h.bus);
    const res = await withHome(
      h.home,
      () =>
        handler(
          new Request(`http://127.0.0.1/workflows/${source.id}/relaunch`, {
            method: "POST",
            body: JSON.stringify({ script: sixCalls("review c.ts AGAIN") }),
          }),
        ),
    );
    assert.equal(res.status, 201, "the receipt is immediate — the run is detached");
    const body = await res.json();
    assert.equal(body.source, source.id);
    assert.notEqual(body.workflow.id, source.id);
    assert.equal(body.script, "explicit");
    assert.equal(body.replay.replayablePrefix, 6, "the preview states what was on offer");
    await done;

    const report = await handler(
      new Request(`http://127.0.0.1/workflows/${body.workflow.id}/replay`),
    );
    assert.equal(report.status, 200);
    const counts = await report.json();
    assert.equal(counts.replayed, 2);
    assert.equal(counts.ranLive, 4);
    assert.equal(counts.forced, 3);
    assert.equal(counts.divergedAt, 2);
    assert.match(counts.line, /2 replayed, 4 ran live of 6/);

    // The unwired case answers with the wiring bug rather than a broken run.
    const bare = createHandler(appCtx(h));
    const refused = await bare(
      new Request(`http://127.0.0.1/workflows/${source.id}/relaunch`, { method: "POST" }),
    );
    assert.equal(refused.status, 500);
    assert.match((await refused.json()).error, /not wired/);
  } finally {
    h.close();
  }
});
