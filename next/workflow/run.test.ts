/**
 * The workflow engine and its combinators, driven through a REAL
 * `permissions: "none"` worker with a fake `AgentRunner` in place of the subagents
 * (plan §7: "Workers | Real workers, trivial programs. Assert on the bridge
 * protocol." and "Subagents/workflows | Fake LLM + real orchestration.").
 *
 * Nothing here mocks `postMessage`, because the things that can go wrong are
 * concurrency and lifecycle and a fake bridge would prove neither. Three of these
 * are invariant tests rather than feature tests, and they are why the file exists:
 *
 *   - **`pipeline` does not barrier.** Item B reaches stage 3 while item A is still
 *     in stage 1. A barrier here would idle a whole fan-out behind its slowest item
 *     at every stage boundary, and the bug is invisible in a test that only checks
 *     the returned values (spec §8).
 *   - **`parallel` maps a thrower to `null` and never rejects.** One failed branch
 *     must not discard the siblings that succeeded (spec §8, plan §6.9).
 *   - **`Date.now()` throws inside the worker.** Journal rerun keys each call by
 *     `hash(prompt + opts)`; a clock reading in a prompt makes replay a silent no-op
 *     that fails as wrong output, not as an error (plan §6.15).
 *
 * The no-barrier test asserts through the HOST, not a clock: the fake runner records
 * the order calls actually arrived in, and observes that A's stage-1 call is still
 * in flight at the instant B's stage-3 call is made. A timing assertion would be a
 * flake; this is a fact about the schedule.
 *
 * Hermetic and offline: an in-memory database, a real bus, no network, no key, and
 * `BOUGH_HOME` pointed at a temp dir for the duration of each engine call so the
 * script mirror never touches the real `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * denied by this environment's egress policy, so the jsr import declared in
 * `deno.json` cannot resolve.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { WorkflowRun } from "../schema/parts.ts";
import {
  type AgentCall,
  type AgentRunner,
  callKey,
  distinctLabel,
  isWorkflowLive,
  recoverOrphanedWorkflows,
  rerunWorkflow,
  type StartOpts,
  startWorkflow,
  stopWorkflow,
  WORKFLOW_PROGRAM_PARAMS,
  type WorkflowCtx,
  workflowSummary,
} from "./run.ts";

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
  const home = Deno.makeTempDirSync({ prefix: "bough-wf-" });
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

/**
 * Run one engine call with `BOUGH_HOME` relocated, then put the environment back.
 * Set and restored around the call rather than for the whole file: every accessor
 * reads the variable live, and a test file that mutated it globally would reach into
 * every other test in the process.
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

/** Start a run and wait for it to finish. The common shape of every test below. */
async function runScript(
  h: Harness,
  runner: AgentRunner,
  script: string,
  opts: Partial<StartOpts> = {},
  notify?: (sessionId: string, text: string) => void,
): Promise<{ ctx: WorkflowCtx; run: WorkflowRun; finished: WorkflowRun }> {
  const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner, notify };
  const done = completion(h.bus);
  const run = await withHome(
    h.home,
    () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script,
        meta: { name: "test", description: "a test workflow" },
        concurrency: 4,
        ...opts,
      }),
  );
  return { ctx, run, finished: await done };
}

/** A runner that reports its own prompt back, so stage output is inspectable. */
function echoRunner(seen: string[] = []): AgentRunner {
  return (call: AgentCall) => {
    seen.push(call.prompt);
    return Promise.resolve(call.prompt);
  };
}

// ---------------------------------------------------------------------------
// the combinators
// ---------------------------------------------------------------------------

Deno.test("pipeline does not barrier — B reaches stage 3 while A is still in stage 1", async () => {
  const h = harness();
  try {
    const seen: string[] = [];
    let releaseA = () => {};
    const aGate = new Promise<void>((resolve) => (releaseA = resolve));
    let aInFlight = false;
    let bReachedStage3WhileAWasStuck: boolean | undefined;

    const runner: AgentRunner = async (call) => {
      seen.push(call.prompt);
      // A's first stage parks until B has gone all the way through.
      if (call.prompt === "s1 A") {
        aInFlight = true;
        await aGate;
        aInFlight = false;
      }
      if (call.prompt.startsWith("s3 ") && call.prompt.endsWith("B")) {
        bReachedStage3WhileAWasStuck = aInFlight;
        releaseA();
      }
      return call.prompt;
    };

    const { finished } = await runScript(
      h,
      runner,
      `
      const out = await pipeline(
        args.items,
        (item) => agent(\`s1 \${item}\`),
        (prev) => agent(\`s2 \${prev}\`),
        (prev) => agent(\`s3 \${prev}\`),
      )
      return out
      `,
      { args: { items: ["A", "B"] } },
    );

    // THE assertion: at the moment B's third stage ran, A had not left its first.
    assert.equal(
      bReachedStage3WhileAWasStuck,
      true,
      "B reached stage 3 only after A left stage 1 — pipeline barriered between stages",
    );
    // And the same fact from the call log: every one of B's stages precedes A's second.
    assert.deepEqual(seen, [
      "s1 A",
      "s1 B",
      "s2 s1 B",
      "s3 s2 s1 B",
      "s2 s1 A",
      "s3 s2 s1 A",
    ]);
    assert.equal(finished.status, "done");
    assert.deepEqual(finished.result, ["s3 s2 s1 A", "s3 s2 s1 B"], "results keep input order");
  } finally {
    h.close();
  }
});

Deno.test("a throwing pipeline stage drops that item to null and skips its rest", async () => {
  const h = harness();
  try {
    const seen: string[] = [];
    const runner: AgentRunner = (call) => {
      seen.push(call.prompt);
      if (call.prompt === "s1 B") return Promise.reject(new Error("the subagent failed"));
      return Promise.resolve(call.prompt);
    };

    const { finished } = await runScript(
      h,
      runner,
      `
      return await pipeline(
        args.items,
        (item) => agent(\`s1 \${item}\`),
        (prev) => agent(\`s2 \${prev}\`),
      )
      `,
      { args: { items: ["A", "B"] } },
    );

    assert.equal(finished.status, "done", "one failed item must not fail the run");
    assert.deepEqual(finished.result, ["s2 s1 A", null]);
    assert.ok(!seen.some((p) => p.includes("s2 s1 B")), "B's remaining stages must be skipped");
    // The failure is journaled as a failure, distinguishably from a stop.
    const rows = h.db.listWorkflowAgents(finished.id);
    const failed = rows.find((r) => r.prompt === "s1 B");
    assert.equal(failed?.status, "error");
    assert.match(failed?.error ?? "", /the subagent failed/);
  } finally {
    h.close();
  }
});

Deno.test("parallel maps a thrower to null and never rejects", async () => {
  const h = harness();
  try {
    const runner: AgentRunner = (call) =>
      call.prompt === "boom"
        ? Promise.reject(new Error("the subagent failed"))
        : Promise.resolve(`report: ${call.prompt}`);

    const { finished } = await runScript(
      h,
      runner,
      `
      let rejected = false
      const out = await parallel([
        () => agent('first'),
        () => agent('boom'),
        () => { throw new Error('a stage that throws synchronously') },
        () => 'a plain value',
      ]).catch(() => { rejected = true; return ['REJECTED'] })
      return { out, rejected }
      `,
    );

    assert.equal(finished.status, "done");
    const result = finished.result as { out: unknown[]; rejected: boolean };
    assert.equal(result.rejected, false, "parallel() must never reject");
    assert.deepEqual(result.out, [
      "report: first",
      null,
      null,
      "a plain value",
    ]);
  } finally {
    h.close();
  }
});

Deno.test("parallel is a barrier — it resolves only once every thunk has settled", async () => {
  const h = harness();
  try {
    let finishedCount = 0;
    const gates: Array<() => void> = [];
    const runner: AgentRunner = async (call) => {
      await new Promise<void>((resolve) => gates.push(resolve));
      finishedCount++;
      return call.prompt;
    };

    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
    const done = completion(h.bus);
    await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script: `return await parallel([() => agent('a'), () => agent('b'), () => agent('c')])`,
        meta: { name: "test", description: "barrier" },
        concurrency: 4,
      }));

    // Let the three calls arrive, then release them one at a time. The run cannot
    // finish while any of them is outstanding.
    while (gates.length < 3) await new Promise((r) => setTimeout(r, 5));
    gates.shift()!();
    gates.shift()!();
    await new Promise((r) => setTimeout(r, 10));
    assert.equal(finishedCount, 2);
    gates.shift()!();

    const finished = await done;
    assert.equal(finished.status, "done");
    assert.deepEqual(finished.result, ["a", "b", "c"]);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// determinism (invariant 15)
// ---------------------------------------------------------------------------

Deno.test("Date.now(), argless new Date() and Math.random() throw inside the worker", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `
      const caught = {}
      const probe = (name, fn) => {
        try { fn(); caught[name] = 'NO THROW' }
        catch (err) { caught[name] = err.message }
      }
      probe('now', () => Date.now())
      probe('newDate', () => new Date())
      probe('random', () => Math.random())
      probe('perf', () => performance.now())
      probe('uuid', () => crypto.randomUUID())
      // An argument means the caller supplied the instant — that still works.
      caught.fromArgs = new Date(0).toISOString()
      caught.parse = Date.parse('1970-01-01T00:00:00.000Z')
      return caught
      `,
    );

    assert.equal(finished.status, "done");
    const c = finished.result as Record<string, string>;
    for (const key of ["now", "newDate", "random", "perf", "uuid"]) {
      assert.notEqual(c[key], "NO THROW", `${key} must not be available inside a workflow`);
      assert.match(c[key], /deterministic/, `${key}'s message must say why`);
    }
    // The message has to say what to do instead, or the model just retries (spec §6).
    assert.match(c.now, /Date\.now\(\)/);
    assert.match(c.now, /args/);
    assert.match(c.random, /index/);
    // A timestamp handed in through args stays usable.
    assert.equal(c.fromArgs, "1970-01-01T00:00:00.000Z");
    assert.equal(c.parse as unknown as number, 0);
  } finally {
    h.close();
  }
});

Deno.test("exit() is catchable inside the workflow worker", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `
      try { Deno.exit(1) } catch (err) { return { message: err.message } }
      return { message: 'NO THROW' }
      `,
    );
    // An uncaught exit would terminate the worker silently and strand the run until
    // its wall timeout — the run finishing at all is half the assertion (plan §6.2).
    assert.equal(finished.status, "done");
    assert.match((finished.result as { message: string }).message, /exit\(1\) is not available/);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the bridge
// ---------------------------------------------------------------------------

Deno.test("the worker binds exactly the documented script parameters", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `return ${JSON.stringify(WORKFLOW_PROGRAM_PARAMS)}.map((n) => n + ':' + typeof eval(n))`,
    );
    assert.equal(finished.status, "done");
    // `args` is a value, the rest are callable. The point of the probe is that
    // nothing is UNDEFINED: run.ts pre-flights scripts against this list, so a name
    // it admits that the worker does not bind is a script that validates and then
    // fails to compile (see the note on WORKFLOW_PROGRAM_PARAMS).
    const bound = finished.result as string[];
    for (const entry of bound) {
      assert.ok(!entry.endsWith(":undefined"), `${entry} — not bound in the worker`);
    }
  } finally {
    h.close();
  }
});

Deno.test("phase() and log() are fire-and-forget progress", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `
      phase('Review')
      log('starting the review')
      console.log('a console line is a log line too')
      const r = await agent('do the thing')
      phase('Verify')
      return r
      `,
    );
    assert.equal(finished.status, "done");
    assert.equal(finished.currentPhase, "Verify");
    const lines = h.events
      .filter((e) => e.type === "workflow.log")
      .map((e) => (e.data as { line: string }).line);
    assert.deepEqual(lines, ["starting the review", "a console line is a log line too"]);
    // The agent row picked up the phase that was current when it was journaled.
    assert.equal(h.db.listWorkflowAgents(finished.id)[0].phase, "Review");
  } finally {
    h.close();
  }
});

Deno.test("a script that throws fails the run with its message", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `throw new Error('the script gave up')`,
    );
    assert.equal(finished.status, "error");
    assert.match(finished.error ?? "", /the script gave up/);
  } finally {
    h.close();
  }
});

Deno.test("a script that does not parse is refused before a worker is spawned", async () => {
  const h = harness();
  try {
    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: echoRunner() };
    await assert.rejects(
      () =>
        withHome(h.home, () =>
          startWorkflow(ctx, {
            sessionId: h.sessionId,
            script: `const agent = 1; return agent`,
            meta: { name: "test", description: "shadowing" },
          })),
      /does not parse/,
    );
    assert.equal(h.db.listWorkflows().length, 0, "a refused script leaves no run row");
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the journal
// ---------------------------------------------------------------------------

Deno.test("rerunning an unchanged script issues zero live agent calls", async () => {
  const h = harness();
  try {
    const script = `
      return await parallel([
        () => agent('review a.ts'),
        () => agent('review b.ts'),
        () => agent('review c.ts'),
      ])
    `;
    const first = await runScript(h, echoRunner(), script);
    assert.equal(first.finished.status, "done");

    const live: string[] = [];
    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: echoRunner(live) };
    const done = completion(h.bus);
    await withHome(h.home, () => rerunWorkflow(ctx, first.finished.id, { script }));
    const second = await done;

    assert.equal(second.status, "done");
    assert.deepEqual(live, [], "an unchanged rerun must not call a single agent");
    assert.deepEqual(second.result, first.finished.result);
    assert.equal(second.resumeOf, first.finished.id);
    const rows = h.db.listWorkflowAgents(second.id);
    assert.equal(rows.length, 3);
    assert.ok(rows.every((r) => r.status === "cached"));
  } finally {
    h.close();
  }
});

// NOTE (T5.7): replay is PREFIX-BOUNDED, so this is no longer "re-runs exactly that
// call". Editing the second of three re-runs the second AND the third, whose key never
// changed — agents share one checkout, so a call after a live one may be asking about a
// tree that no longer matches the answer in the journal (spec §8). The full statement of
// that rule, including the assertion that the unchanged keys really are unchanged, is
// `workflow/relaunch.test.ts`; this keeps the engine-level version of it honest.
Deno.test("editing one prompt re-runs that call and everything after it", async () => {
  const h = harness();
  try {
    const script = (b: string) => `
      return await parallel([
        () => agent('review a.ts'),
        () => agent('${b}'),
        () => agent('review c.ts'),
      ])
    `;
    const first = await runScript(h, echoRunner(), script("review b.ts"));

    const live: string[] = [];
    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: echoRunner(live) };
    const done = completion(h.bus);
    await withHome(
      h.home,
      () => rerunWorkflow(ctx, first.finished.id, { script: script("review b.ts THOROUGHLY") }),
    );
    const second = await done;

    assert.deepEqual(live, ["review b.ts THOROUGHLY", "review c.ts"]);
    assert.deepEqual(second.result, [
      "review a.ts",
      "review b.ts THOROUGHLY",
      "review c.ts",
    ]);
    const statuses = h.db.listWorkflowAgents(second.id).map((r) => `${r.prompt}:${r.status}`);
    assert.deepEqual(statuses, [
      "review a.ts:cached",
      "review b.ts THOROUGHLY:done",
      // Unchanged, and it ran anyway: replay stopped at the edit above it.
      "review c.ts:done",
    ]);
  } finally {
    h.close();
  }
});

Deno.test("a failed call re-runs live, and so does everything after it", async () => {
  const h = harness();
  try {
    const script = `return await parallel([() => agent('flaky'), () => agent('steady')])`;
    let fail = true;
    const first = await runScript(
      h,
      (call) =>
        fail && call.prompt === "flaky"
          ? Promise.reject(new Error("transient"))
          : Promise.resolve(call.prompt),
      script,
    );
    assert.deepEqual(first.finished.result, [null, "steady"]);

    fail = false;
    const live: string[] = [];
    const ctx: WorkflowCtx = {
      db: h.db,
      bus: h.bus,
      runner: (call) => {
        live.push(call.prompt);
        return Promise.resolve(call.prompt);
      },
    };
    const done = completion(h.bus);
    await withHome(h.home, () => rerunWorkflow(ctx, first.finished.id, { script }));
    const second = await done;

    // The failure re-runs because it may be what the author just fixed — and `steady`
    // re-runs behind it under the prefix rule (T5.7): a live agent may have moved the
    // shared checkout, so its stored answer is no longer about the tree it described.
    assert.deepEqual(live, ["flaky", "steady"], "the failure ends the replayable prefix");
    assert.deepEqual(second.result, ["flaky", "steady"]);
  } finally {
    h.close();
  }
});

Deno.test("callKey changes with every field that changes what the agent is asked", () => {
  const base: AgentCall = { prompt: "p", label: "l" };
  const key = callKey(base);
  assert.equal(callKey({ ...base }), key, "the key is a pure function of the call");
  assert.notEqual(callKey({ ...base, prompt: "p2" }), key);
  assert.notEqual(callKey({ ...base, label: "l2" }), key);
  assert.notEqual(callKey({ ...base, phase: "Review" }), key);
  assert.notEqual(callKey({ ...base, model: "opus" }), key);
  assert.notEqual(callKey({ ...base, schema: { type: "object" } }), key);
});

Deno.test("distinctLabel finds the line a shared-preamble sibling has not claimed", () => {
  const prompt = "You are contributing evidence to a thorough audit.\nReview src/server/app.ts";
  // 40 characters INCLUDING the ellipsis — the label is rendered into a fixed-width
  // rail, so the budget is the whole string, not the text before the marker.
  const preamble = "You are contributing evidence to a thor…";
  assert.equal(preamble.length, 40);
  assert.equal(distinctLabel(prompt, []), preamble);
  assert.equal(
    distinctLabel(prompt, [preamble]),
    "Review src/server/app.ts",
  );
  // Identical prompts have no distinct line left, so they are numbered instead.
  assert.match(distinctLabel("same", ["same"]), /same #2$/);
});

// ---------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------

Deno.test("stop kills the worker and interrupts in-flight agents", async () => {
  const h = harness();
  try {
    let aborted = false;
    let started = 0;
    const runner: AgentRunner = (_call, signal) => {
      started++;
      return new Promise<string>((_resolve, reject) => {
        signal.addEventListener("abort", () => {
          aborted = true;
          reject(new Error("interrupted"));
        }, { once: true });
      });
    };

    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner };
    const done = completion(h.bus);
    const run = await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script: `return await agent('a long one')`,
        meta: { name: "test", description: "stop" },
      }));

    while (started === 0) await new Promise((r) => setTimeout(r, 5));
    const stopped = stopWorkflow(ctx, run.id);

    assert.equal(stopped.status, "stopped");
    assert.equal(aborted, true, "stop must interrupt the subagent turn, not just the script");
    assert.equal(isWorkflowLive(run.id), false);
    assert.equal(h.db.listWorkflowAgents(run.id)[0].status, "stopped");
    // The stop itself is what settles the run; nothing else arrives afterwards.
    assert.equal((await done).status, "stopped");
  } finally {
    h.close();
  }
});

Deno.test("a finished run notifies its owning session", async () => {
  const h = harness();
  try {
    const notes: Array<{ sessionId: string; text: string }> = [];
    const { finished } = await runScript(
      h,
      echoRunner(),
      `return await agent('one thing')`,
      {},
      (sessionId, text) => void notes.push({ sessionId, text }),
    );
    assert.equal(finished.status, "done");
    assert.equal(notes.length, 1);
    assert.equal(notes[0].sessionId, h.sessionId);
    assert.match(notes[0].text, /\[workflow done\]/);
    assert.match(notes[0].text, /1\/1 agents succeeded/);
  } finally {
    h.close();
  }
});

Deno.test("boot recovery orphans a run the previous process left running", () => {
  const h = harness();
  try {
    const run = h.db.createWorkflow({
      id: crypto.randomUUID(),
      sessionId: h.sessionId,
      name: "stranded",
      description: "left running by a dead process",
      script: "return 1",
      phases: [],
      status: "running",
      currentPhase: null,
      result: null,
      error: null,
      args: null,
      resumeOf: null,
      createdAt: 1_000,
      finishedAt: null,
    });
    h.db.createWorkflowAgent({
      id: crypto.randomUUID(),
      runId: run.id,
      idx: 0,
      key: "k",
      label: "l",
      phase: null,
      prompt: "p",
      model: null,
      status: "running",
      result: null,
      error: null,
      sessionId: null,
      startedAt: 1_000,
      finishedAt: null,
    });

    const recovered = recoverOrphanedWorkflows(h.db, h.bus, () => 9_999);

    assert.deepEqual(recovered, [run.id]);
    const after = h.db.getWorkflow(run.id)!;
    assert.equal(after.status, "orphaned");
    assert.match(after.error ?? "", /the server restarted/);
    assert.equal(after.finishedAt, 9_999);
    assert.equal(h.db.listWorkflowAgents(run.id)[0].status, "stopped");
    // Recovery is announced, so a client that connects after the restart is not
    // left rendering a run that died with the previous process.
    assert.ok(
      h.events.some((e) => e.type === "workflow.updated" && (e.data as WorkflowRun).id === run.id),
    );
    // Idempotent: a second pass finds nothing left to recover.
    assert.deepEqual(recoverOrphanedWorkflows(h.db, h.bus, () => 10_000), []);
  } finally {
    h.close();
  }
});

Deno.test("workflowSummary omits the script and counts the journal", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `return await parallel([() => agent('one'), () => agent('two')])`,
    );
    const summary = workflowSummary(h.db, h.db.getWorkflow(finished.id)!);
    assert.equal("script" in summary, false, "a list of runs must not carry N script bodies");
    assert.deepEqual(summary.agents, {
      total: 2,
      done: 2,
      cached: 0,
      running: 0,
      queued: 0,
      failed: 0,
    });
    assert.match(String(summary.scriptFile), new RegExp(`${finished.id}\\.js$`));
  } finally {
    h.close();
  }
});
