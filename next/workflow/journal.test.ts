/**
 * The journal, proved where it actually has to hold: through the REAL
 * `permissions: "none"` workflow worker, with a counting fake `AgentRunner` standing
 * in for the subagents (plan §7 — "Subagents/workflows | Fake LLM + real
 * orchestration"). Nothing here mocks the bridge, because a rerun's whole claim is
 * about calls that were never made, and a fake bridge would let a passing test say
 * nothing about the real one.
 *
 * The three tests the task turns on, and why each is an invariant rather than a
 * feature:
 *
 *   - **An unchanged rerun issues ZERO agent calls.** This is the product promise of
 *     a workflow journal; a rerun that pays again for 40 agents is not a slow feature,
 *     it is a broken one (spec §8).
 *   - **Editing one prompt re-runs exactly that call.** The failure mode in the other
 *     direction — replaying a stale answer to an edited question — does not announce
 *     itself. It looks like a fast rerun and reads as wrong output.
 *   - **Reordering a script invalidates nothing.** Moving calls around is the second
 *     most common edit after changing a word, and an index-based journal gets it
 *     wrong on the first try.
 *
 * Plus the guard that keeps this module honest: the key computed HERE is asserted
 * equal to the key the engine actually wrote into `workflow_agents`. Two modules
 * holding the same hash is a drift risk, and this pins it behaviourally rather than
 * by inspection — if the engine's key changed, every report and plan in this file
 * would be a plausible-looking lie.
 *
 * Hermetic and offline: an in-memory database, a real bus, no network, no key, and
 * `BOUGH_HOME` pointed at a temp dir for the duration of every engine call, so the
 * script mirror never touches the real `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * denied by this environment's egress policy, so the jsr import declared in
 * `deno.json` cannot resolve.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { ConflictError, NotFoundError, PathError } from "../errors.ts";
import type { WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import {
  callKey,
  isReplayable,
  journalLabel,
  mirrorPath,
  mirrorScript,
  readMirror,
  replayableCount,
  replayIndex,
  rerun,
  type RerunDeps,
  rerunReport,
  resolveRerunScript,
  syncScriptMirrors,
  takeReplay,
} from "./journal.ts";
import {
  type AgentCall,
  type AgentRunner,
  isWorkflowLive,
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
  const home = Deno.makeTempDirSync({ prefix: "bough-journal-" });
  return {
    db,
    bus,
    sessionId: session.id,
    home,
    close() {
      db.close();
      try {
        Deno.removeSync(home, { recursive: true });
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
async function run(
  h: Harness,
  runner: AgentRunner,
  script: string,
): Promise<WorkflowRun> {
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

/**
 * Rerun through `journal.rerun` — the module under test — with the real engine
 * injected behind it, and wait for the new run to finish.
 */
async function rerunAndWait(
  h: Harness,
  runner: AgentRunner,
  request: { id: string; script?: string; args?: unknown },
): Promise<{ finished: WorkflowRun; script: string; replayable: number }> {
  const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
  const deps: RerunDeps = {
    start: (opts) => startWorkflow(ctx, { ...opts, concurrency: 4 }),
    isLive: isWorkflowLive,
  };
  const done = completion(h.bus);
  const result = await withHome(h.home, () => rerun(h.db, request, deps));
  return { finished: await done, script: result.script, replayable: result.replayable };
}

/** `prompt:status` per journal row, in call order — the shape every AC asserts on. */
function journal(db: SqliteDb, runId: string): string[] {
  return db.listWorkflowAgents(runId).map((a) => `${a.prompt}:${a.status}`);
}

// ---------------------------------------------------------------------------
// the three acceptance criteria
// ---------------------------------------------------------------------------

Deno.test("a rerun of an unchanged script issues zero agent calls", async () => {
  const h = harness();
  try {
    const script = `
      return await parallel([
        () => agent('review a.ts'),
        () => agent('review b.ts'),
        () => agent('review c.ts'),
      ])
    `;
    const first = await run(h, recorder().runner, script);
    assert.equal(first.status, "done");
    assert.equal(h.db.listWorkflowAgents(first.id).length, 3);

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, { id: first.id, script });

    assert.deepEqual(live.calls, [], "an unchanged rerun must not call a single agent");
    assert.equal(second.finished.status, "done");
    assert.deepEqual(second.finished.result, first.result, "and it must produce the same answer");
    assert.equal(second.finished.resumeOf, first.id, "a rerun points back, it never rewrites");
    assert.deepEqual(journal(h.db, second.finished.id), [
      "review a.ts:cached",
      "review b.ts:cached",
      "review c.ts:cached",
    ]);

    // The status is the UI's whole account of what replayed versus what ran.
    const report = rerunReport(h.db, second.finished.id);
    assert.equal(report.sourceId, first.id);
    assert.equal(report.replayed, 3);
    assert.equal(report.ran, 0);
    assert.deepEqual(report.ranPrompts, []);
  } finally {
    h.close();
  }
});

Deno.test("editing one call's prompt re-runs exactly that call and replays the rest", async () => {
  const h = harness();
  try {
    const script = (b: string) => `
      return await parallel([
        () => agent('review a.ts'),
        () => agent('${b}'),
        () => agent('review c.ts'),
      ])
    `;
    const first = await run(h, recorder().runner, script("review b.ts"));
    assert.equal(first.status, "done");

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, {
      id: first.id,
      script: script("review b.ts, and check the error paths"),
    });

    assert.deepEqual(live.calls, ["review b.ts, and check the error paths"]);
    assert.deepEqual(journal(h.db, second.finished.id), [
      "review a.ts:cached",
      "review b.ts, and check the error paths:done",
      "review c.ts:cached",
    ]);
    assert.deepEqual(second.finished.result, [
      "report: review a.ts",
      "report: review b.ts, and check the error paths",
      "report: review c.ts",
    ], "the replayed reports are the source run's, verbatim");

    const report = rerunReport(h.db, second.finished.id);
    assert.equal(report.replayed, 2);
    assert.equal(report.ran, 1);
    assert.deepEqual(report.ranPrompts, ["review b.ts, and check the error paths"]);
  } finally {
    h.close();
  }
});

Deno.test("a reordered but otherwise identical script does not invalidate anything", async () => {
  const h = harness();
  try {
    const first = await run(
      h,
      recorder().runner,
      `
        const a = await agent('review a.ts')
        const b = await agent('review b.ts')
        const c = await agent('review c.ts')
        return [a, b, c]
      `,
    );
    assert.equal(first.status, "done");

    const live = recorder();
    // Same three calls, issued in a different order, assembled into the same answer.
    const second = await rerunAndWait(h, live.runner, {
      id: first.id,
      script: `
        const c = await agent('review c.ts')
        const b = await agent('review b.ts')
        const a = await agent('review a.ts')
        return [a, b, c]
      `,
    });

    assert.deepEqual(live.calls, [], "reordering changes no key, so nothing re-runs");
    assert.deepEqual(second.finished.result, first.result);
    // The journal really is in the new order — the run reordered, and still replayed.
    assert.deepEqual(journal(h.db, second.finished.id), [
      "review c.ts:cached",
      "review b.ts:cached",
      "review a.ts:cached",
    ]);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the key, pinned against the engine that writes it
// ---------------------------------------------------------------------------

Deno.test("the key this module computes is the key the engine journals", async () => {
  const h = harness();
  try {
    const finished = await run(
      h,
      recorder().runner,
      `
        await agent('alpha')
        await agent('beta', { phase: 'Review', model: 'openai:gpt-5' })
        await agent('gamma', { label: 'the gamma pass' })
        return 'ok'
      `,
    );
    const rows = h.db.listWorkflowAgents(finished.id);
    assert.equal(rows.length, 3);
    for (const row of rows) {
      assert.equal(
        callKey({
          prompt: row.prompt,
          label: row.label,
          phase: row.phase ?? undefined,
          model: row.model ?? undefined,
        }),
        row.key,
        `key drift on "${row.prompt}" — journal.ts and the engine must agree`,
      );
    }
  } finally {
    h.close();
  }
});

Deno.test("changing a phase() title does not invalidate the calls under it", async () => {
  const h = harness();
  try {
    // The row records the phase the run was in for display; the KEY hashes only a
    // phase the call itself named. Renaming a heading must not re-run the fan-out.
    const first = await run(
      h,
      recorder().runner,
      `phase('Review')\nreturn await agent('review a.ts')`,
    );
    assert.equal(h.db.listWorkflowAgents(first.id)[0].phase, "Review");

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, {
      id: first.id,
      script: `phase('Audit')\nreturn await agent('review a.ts')`,
    });
    assert.deepEqual(live.calls, []);
    assert.equal(h.db.listWorkflowAgents(second.finished.id)[0].phase, "Audit");
    assert.equal(h.db.listWorkflowAgents(second.finished.id)[0].status, "cached");
  } finally {
    h.close();
  }
});

Deno.test("an opt that changes what the agent is asked re-runs the call", async () => {
  const h = harness();
  try {
    const first = await run(h, recorder().runner, `return await agent('audit', {model: 'a'})`);
    const live = recorder();
    const second = await rerunAndWait(h, live.runner, {
      id: first.id,
      script: `return await agent('audit', {model: 'b'})`,
    });
    assert.deepEqual(live.calls, ["audit"], "a different model is a different question");
    assert.equal(journal(h.db, second.finished.id)[0], "audit:done");
  } finally {
    h.close();
  }
});

Deno.test("a chain of reruns keeps replaying — a cached row is an answer too", async () => {
  const h = harness();
  try {
    const script = `return await agent('review a.ts')`;
    const first = await run(h, recorder().runner, script);
    const second = await rerunAndWait(h, recorder().runner, { id: first.id, script });
    const third = recorder();
    const last = await rerunAndWait(h, third.runner, { id: second.finished.id, script });
    assert.deepEqual(third.calls, [], "replaying a replay must not fall back to a live call");
    assert.equal(journal(h.db, last.finished.id)[0], "review a.ts:cached");
    assert.equal(last.finished.result, "report: review a.ts");
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the on-disk mirror — the "edit the file, press r" loop
// ---------------------------------------------------------------------------

Deno.test("a rerun with no script runs the mirror the user edited", async () => {
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
    assert.ok((await Deno.readTextFile(path)).includes("review a.ts"), "the run mirrored itself");

    // The user edits the file on disk — no request body, no re-POST.
    await Deno.writeTextFile(
      path,
      `
        const a = await agent('review a.ts')
        const b = await agent('review b.ts twice as hard')
        return [a, b]
      `,
    );

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, { id: first.id });

    assert.equal(second.script, "mirror");
    assert.deepEqual(live.calls, ["review b.ts twice as hard"]);
    assert.deepEqual(journal(h.db, second.finished.id), [
      "review a.ts:cached",
      "review b.ts twice as hard:done",
    ]);
  } finally {
    h.close();
  }
});

Deno.test("a rerun falls back to the stored script when the mirror is gone", async () => {
  const h = harness();
  try {
    const first = await run(h, recorder().runner, `return await agent('review a.ts')`);
    await withHome(h.home, async () => {
      await Deno.remove(mirrorPath(first.id));
    });

    const live = recorder();
    const second = await rerunAndWait(h, live.runner, { id: first.id });
    assert.equal(second.script, "stored");
    assert.deepEqual(live.calls, [], "the stored script is the same script — still zero calls");
    assert.equal(journal(h.db, second.finished.id)[0], "review a.ts:cached");
  } finally {
    h.close();
  }
});

Deno.test("syncScriptMirrors writes what is missing and never touches what is there", async () => {
  const h = harness();
  try {
    const first = await run(h, recorder().runner, `return await agent('review a.ts')`);
    await withHome(h.home, async () => {
      const path = mirrorPath(first.id);
      await Deno.remove(path);

      // Boot with the file gone: it comes back from the row.
      assert.deepEqual(await syncScriptMirrors(h.db), [first.id]);
      assert.equal(await Deno.readTextFile(path), first.script);

      // Boot again with the user's edit in place: untouched, or the restart would
      // replay the edit away on the next rerun.
      await Deno.writeTextFile(path, "// mine\nreturn await agent('review a.ts')");
      assert.deepEqual(await syncScriptMirrors(h.db), [], "an existing mirror is never rewritten");
      assert.equal(
        await Deno.readTextFile(path),
        "// mine\nreturn await agent('review a.ts')",
      );
    });
  } finally {
    h.close();
  }
});

Deno.test("a run id cannot name a file outside the workflows directory", () => {
  const home = Deno.makeTempDirSync({ prefix: "bough-journal-" });
  const prior = Deno.env.get("BOUGH_HOME");
  Deno.env.set("BOUGH_HOME", home);
  try {
    assert.throws(() => mirrorPath("../../etc/crontab"), PathError);
    assert.throws(() => mirrorPath("/etc/crontab"), PathError);
    assert.ok(mirrorPath("a-b-c").endsWith("/workflows/a-b-c.js"));
  } finally {
    if (prior === undefined) Deno.env.delete("BOUGH_HOME");
    else Deno.env.set("BOUGH_HOME", prior);
    Deno.removeSync(home, { recursive: true });
  }
});

Deno.test("mirrorScript and readMirror round-trip, and a missing mirror reads null", async () => {
  const home = await Deno.makeTempDir({ prefix: "bough-journal-" });
  await withHome(home, async () => {
    assert.equal(await readMirror("nope"), null);
    assert.equal(await mirrorScript("run-1", "return 1"), true);
    assert.equal(await readMirror("run-1"), "return 1");
  });
  await Deno.remove(home, { recursive: true });
});

// ---------------------------------------------------------------------------
// the key and the index, as pure functions
// ---------------------------------------------------------------------------

Deno.test("the key is stable for an unchanged call and changes with every opt", () => {
  // `label` is required by the engine's AgentCall — the key is a pure function of
  // the whole tuple, and the label is part of it.
  const base = { prompt: "review a.ts", label: "review a.ts" };
  const key = callKey(base);
  assert.equal(callKey({ ...base }), key, "the key is a pure function of the call");
  assert.equal(callKey({ prompt: "review a.ts", label: "review a.ts" }), key, "and carries nothing from the run");
  assert.notEqual(callKey({ ...base, prompt: "review a.ts " }), key);
  assert.notEqual(callKey({ ...base, label: "a" }), key);
  assert.notEqual(callKey({ ...base, phase: "Review" }), key);
  assert.notEqual(callKey({ ...base, model: "openai:gpt-5" }), key);
  assert.notEqual(callKey({ ...base, schema: { type: "object" } }), key);
  assert.notEqual(
    callKey({ ...base, schema: { type: "object", additionalProperties: false } }),
    callKey({ ...base, schema: { type: "object" } }),
  );
});

Deno.test("the key does not depend on the order the script wrote its opts in", () => {
  assert.equal(
    callKey({ prompt: "p", label: "p", model: "m", phase: "Review" }),
    callKey({ phase: "Review", prompt: "p", label: "p", model: "m" }),
  );
});

Deno.test("the label is part of the key, and the engine owns defaulting it", () => {
  // This module no longer derives a label of its own. It used to, and the two
  // definitions drifted: the engine defaults to the first PHYSICAL line of the
  // prompt, this one defaulted to the first non-empty line, trimmed. A prompt with
  // CRLF or trailing whitespace on line 1 then keyed differently depending on which
  // copy ran — so a rerun missed cache with no error and no signal. `callKey` is now
  // re-exported from the engine and `label` is required; defaulting happens once, in
  // the engine, before it journals.
  assert.notEqual(
    callKey({ prompt: "alpha\nbeta", label: "" }),
    callKey({ prompt: "alpha\nbeta", label: "alpha" }),
  );
  assert.equal(journalLabel("  \n  alpha \nbeta"), "alpha");
  assert.equal(journalLabel("x".repeat(60)), `${"x".repeat(39)}…`);
  assert.equal(journalLabel(""), "");
});

Deno.test("only a finished call with an answer replays", () => {
  const row = (patch: Partial<WorkflowAgent>): WorkflowAgent => ({
    id: "a",
    runId: "r",
    idx: 0,
    key: "k",
    label: "l",
    phase: null,
    prompt: "p",
    model: null,
    status: "done",
    result: "the report",
    error: null,
    sessionId: null,
    startedAt: 0,
    finishedAt: 1,
    ...patch,
  });
  assert.equal(isReplayable(row({})), true);
  assert.equal(isReplayable(row({ status: "cached" })), true);
  assert.equal(isReplayable(row({ status: "error", result: null })), false);
  assert.equal(isReplayable(row({ status: "stopped" })), false);
  assert.equal(isReplayable(row({ status: "running", result: null })), false);
  assert.equal(isReplayable(row({ status: "queued", result: null })), false);
  assert.equal(isReplayable(row({ result: null })), false, "no answer is no answer");
});

Deno.test("the replay index is FIFO per key, so N identical calls get N results", () => {
  const h = harness();
  try {
    const runRow = h.db.createWorkflow({
      id: "run-1",
      sessionId: h.sessionId,
      name: "n",
      description: "d",
      script: "s",
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
    const add = (
      idx: number,
      key: string,
      status: WorkflowAgent["status"],
      result: string | null,
    ) =>
      h.db.createWorkflowAgent({
        id: `a${idx}`,
        runId: runRow.id,
        idx,
        key,
        label: `l${idx}`,
        phase: null,
        prompt: "p",
        model: null,
        status,
        result,
        error: null,
        sessionId: null,
        startedAt: 0,
        finishedAt: 1,
      });
    add(0, "same", "done", "first");
    add(1, "same", "done", "second");
    add(2, "other", "error", null);
    add(3, "third", "cached", "replayed once already");

    const index = replayIndex(h.db, runRow.id);
    assert.equal(replayableCount(index), 3, "the failed call is not replayable");
    assert.equal(takeReplay(index, "same"), "first");
    assert.equal(takeReplay(index, "same"), "second");
    assert.equal(takeReplay(index, "same"), undefined, "a key's queue is exhausted, not reused");
    assert.equal(takeReplay(index, "other"), undefined);
    assert.equal(takeReplay(index, "third"), "replayed once already");
    assert.equal(replayableCount(replayIndex(h.db, "no-such-run")), 0);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the rerun boundary
// ---------------------------------------------------------------------------

function stubDeps(over: Partial<RerunDeps> = {}): RerunDeps & { started: unknown[] } {
  const started: unknown[] = [];
  return {
    started,
    start(opts) {
      started.push(opts);
      return Promise.resolve({ ...(opts as unknown as WorkflowRun), id: "new" });
    },
    ...over,
  };
}

Deno.test("rerun refuses an unknown run, a live one, and a malformed request", async () => {
  const h = harness();
  try {
    const finished = await run(h, recorder().runner, `return await agent('a')`);
    await assert.rejects(
      () => rerun(h.db, { id: "nope" }, stubDeps()),
      (err: Error) => err instanceof NotFoundError,
    );
    await assert.rejects(
      () => rerun(h.db, { id: finished.id }, stubDeps({ isLive: () => true })),
      (err: Error) =>
        err instanceof ConflictError && /still running — stop it first/.test(err.message),
    );
    await assert.rejects(
      () => rerun(h.db, { script: "return 1" }, stubDeps()),
      (err: Error) => /workflow\.rerun\(\{id, script\?, args\?\}\)/.test(err.message),
    );
  } finally {
    h.close();
  }
});

Deno.test("rerun keeps the source run's args unless the caller replaces them", async () => {
  const h = harness();
  try {
    const finished = await run(h, recorder().runner, `return await agent('a')`);
    const kept = stubDeps();
    await withHome(h.home, () => rerun(h.db, { id: finished.id }, kept));
    assert.deepEqual(Object.keys(kept.started[0] as object).includes("args"), false);

    const replaced = stubDeps();
    await withHome(
      h.home,
      () => rerun(h.db, { id: finished.id, args: { files: ["x"] } }, replaced),
    );
    assert.deepEqual((replaced.started[0] as { args: unknown }).args, { files: ["x"] });
  } finally {
    h.close();
  }
});

Deno.test("rerun reports its script provenance and what is available to replay", async () => {
  const h = harness();
  try {
    const finished = await run(
      h,
      recorder().runner,
      `return await parallel([() => agent('a'), () => agent('b')])`,
    );
    const explicit = await withHome(
      h.home,
      () => rerun(h.db, { id: finished.id, script: "return 1" }, stubDeps()),
    );
    assert.equal(explicit.script, "explicit");
    assert.equal(explicit.replayable, 2);
    assert.equal(explicit.source.id, finished.id);
    assert.equal((explicit.run as unknown as { resumeOf: string }).resumeOf, finished.id);

    // Provenance is resolved from the run row, so it is testable without an engine.
    assert.deepEqual(await withHome(h.home, () => resolveRerunScript(finished, "  ")), {
      script: finished.script,
      from: "mirror",
    }, "a blank override is not an override");
  } finally {
    h.close();
  }
});

Deno.test("rerunReport counts a run that is still in flight", () => {
  const h = harness();
  try {
    const base = {
      sessionId: h.sessionId,
      name: "n",
      description: "d",
      script: "s",
      phases: [],
      currentPhase: null,
      result: null,
      error: null,
      args: null,
      createdAt: 1,
    };
    // `resume_of` is a real foreign key — a rerun always points at a run that exists.
    h.db.createWorkflow({
      ...base,
      id: "source-run",
      status: "done",
      resumeOf: null,
      finishedAt: 2,
    });
    h.db.createWorkflow({
      ...base,
      id: "run-1",
      status: "running",
      resumeOf: "source-run",
      finishedAt: null,
    });
    const add = (idx: number, status: WorkflowAgent["status"]) =>
      h.db.createWorkflowAgent({
        id: `a${idx}`,
        runId: "run-1",
        idx,
        key: `k${idx}`,
        label: `l${idx}`,
        phase: null,
        prompt: `p${idx}`,
        model: null,
        status,
        result: status === "cached" || status === "done" ? "r" : null,
        error: null,
        sessionId: null,
        startedAt: 0,
        finishedAt: null,
      });
    add(0, "cached");
    add(1, "done");
    add(2, "error");
    add(3, "running");
    add(4, "stopped");

    const report = rerunReport(h.db, "run-1");
    assert.equal(report.sourceId, "source-run");
    assert.deepEqual(
      [report.total, report.replayed, report.ran, report.failed, report.stopped, report.pending],
      [5, 1, 1, 1, 1, 1],
    );
    assert.deepEqual(report.ranPrompts, ["p1", "p2", "p3", "p4"]);
    assert.throws(() => rerunReport(h.db, "nope"), NotFoundError);
  } finally {
    h.close();
  }
});
