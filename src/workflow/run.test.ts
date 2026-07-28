/**
 * The workflow engine and its combinators, driven through a REAL
 * workflow worker with a fake `AgentRunner` in place of the subagents
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
 *   - **pause gates ADMISSION, not issuance.** A `parallel()` fan-out issues every
 *     call at dispatch, so a gate consulted once on the way in is a no-op for it —
 *     pause held nothing, the semaphore drained, and the run billed to completion for
 *     precisely the shape workflows exist for (spec §8). The fan-out and the
 *     sequential case are BOTH tested: the sequential one is the shape that always
 *     worked, so it is the regression the fix had to keep.
 *   - **a stopped run leaves no row in a non-terminal state.** A call parked on the
 *     gate when the stop lands must not journal a row after the wind-down swept them,
 *     and one that already holds a row must settle it. Both directions are asserted,
 *     because a `queued` row on a dead run reads as an agent still working and nothing
 *     is left in the process that could ever finish it.
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
 * Assertions come from `node:assert/strict` rather than `@std/assert`, which is not a
 * dependency of this repo.
 */
import { test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { WorkflowRun } from "../schema/parts.ts";
import {
  type AgentCall,
  type AgentRunner,
  callKey,
  comparePos,
  distinctLabel,
  emptyReplayPlan,
  isWorkflowLive,
  journalKey,
  pauseWorkflow,
  recoverOrphanedWorkflows,
  replayAudit,
  rerunWorkflow,
  resumeWorkflow,
  splitJournalKey,
  type StartOpts,
  startWorkflow,
  stopWorkflow,
  WORKFLOW_PROGRAM_PARAMS,
  type WorkflowCtx,
  workflowSummary,
  classifyDivergence,
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
  const home = mkdtempSync(join(tmpdir(), "bough-wf-"));
  return {
    db,
    bus,
    sessionId: session.id,
    events,
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
 * Run one engine call with `BOUGH_HOME` relocated, then put the environment back.
 * Set and restored around the call rather than for the whole file: every accessor
 * reads the variable live, and a test file that mutated it globally would reach into
 * every other test in the process.
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

const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Poll a condition with a deadline, so a broken wait fails as a message not a hang. */
async function until(what: string, cond: () => boolean, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await delay(5);
  }
}

/**
 * A runner whose every call parks until the test releases it by prompt. The pause and
 * stop tests below need to hold agents in flight at an exact moment, and a gate makes
 * that a fact about the schedule rather than a race against a timer.
 */
function gatedRunner(): {
  runner: AgentRunner;
  /** Prompts that actually reached the runner, in arrival order. */
  started: string[];
  /** Settle one parked call. Returns false if that prompt never arrived. */
  release(prompt: string): boolean;
  /** Settle everything parked right now. */
  releaseAll(): void;
} {
  const started: string[] = [];
  const gates = new Map<string, () => void>();
  return {
    started,
    runner: (call: AgentCall) => {
      started.push(call.prompt);
      return new Promise<string>((resolve) =>
        gates.set(call.prompt, () => resolve(`report ${call.prompt}`))
      );
    },
    release(prompt: string): boolean {
      const open = gates.get(prompt);
      if (!open) return false;
      gates.delete(prompt);
      open();
      return true;
    },
    releaseAll(): void {
      for (const [prompt, open] of [...gates]) {
        gates.delete(prompt);
        open();
      }
    },
  };
}

/** Six agents through one `parallel()` — the shape a workflow exists for. */
const FANOUT_SCRIPT =
  `return await parallel([0,1,2,3,4,5].map((i) => () => agent('work ' + i, { label: 'w' + i })))`;

// ---------------------------------------------------------------------------
// the combinators
// ---------------------------------------------------------------------------

test("pipeline does not barrier — B reaches stage 3 while A is still in stage 1", async () => {
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

test("a throwing pipeline stage drops that item to null and skips its rest", async () => {
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

test("parallel maps a thrower to null and never rejects", async () => {
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

test("parallel is a barrier — it resolves only once every thunk has settled", async () => {
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

test("Date.now(), argless new Date() and Math.random() throw inside the worker", async () => {
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

test("exit() is catchable inside the workflow worker", async () => {
  const h = harness();
  try {
    const { finished } = await runScript(
      h,
      echoRunner(),
      `
      try { process.exit(1) } catch (err) { return { message: err.message } }
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

test("the worker binds exactly the documented script parameters", async () => {
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

test("phase() and log() are fire-and-forget progress", async () => {
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

test("a script that throws fails the run with its message", async () => {
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

test("a script that does not parse is refused before a worker is spawned", async () => {
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
// structural journal positions
// ---------------------------------------------------------------------------
//
// The journal is prefix-bounded, so a call's POSITION is part of its identity. These
// hold the property that makes that safe: a position is a fact about the script's
// shape, never about how long an agent took.
//
// The defect they close (adversarial review, reproduced): position used to be the
// order calls reached the host. `pipeline()` has no barrier by design, so its stage-2
// calls are issued in stage-1 COMPLETION order — and an UNCHANGED script therefore
// journaled its calls in a different order on every run, transposing positions and
// re-billing everything past stage 1 on a relaunch. It hit spec §8's own canonical
// example.
//
// Latency is expressed as a GATE, not a timer. The reviewer's reproduction used 60ms
// against 1ms; a gate produces the identical schedule as a fact rather than a race,
// and the plan forbids timing assertions (§0).

/** prompt → structural coordinate, read back out of the journal key. */
function positions(db: SqliteDb, runId: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const a of db.listWorkflowAgents(runId)) out[a.prompt] = splitJournalKey(a.key).pos ?? "";
  return out;
}

/** The journal in DISPATCH order — which is exactly the thing that is not stable. */
function dispatched(db: SqliteDb, runId: string): string[] {
  return db.listWorkflowAgents(runId).map((a) => a.prompt);
}

/** A runner that fails the test if anything reaches it. For "must replay everything". */
const noLiveCalls: AgentRunner = (call) => {
  throw new Error(`a live agent ran during a replay-only run: ${call.prompt}`);
};

const TWO_STAGE = `
  return await pipeline(
    args.items,
    (item) => agent(\`s1 \${item}\`),
    (prev) => agent(\`s2 \${prev}\`),
  )
`;

/**
 * `s1 A` parks until B has gone all the way through stage 2 — the reviewer's 60ms/1ms
 * asymmetry, as a schedule instead of a race. Returns the arrival log.
 */
function skewedPipelineRunner(): { runner: AgentRunner; seen: string[] } {
  const seen: string[] = [];
  let releaseA = () => {};
  const aGate = new Promise<void>((resolve) => (releaseA = resolve));
  const runner: AgentRunner = async (call) => {
    seen.push(call.prompt);
    if (call.prompt === "s1 A") await aGate;
    if (call.prompt === "s2 s1 B") releaseA();
    return call.prompt;
  };
  return { runner, seen };
}

test(
  "THE REPRODUCTION: an unchanged pipeline with asymmetric stage-1 latency replays 4/4",
  async () => {
    const h = harness();
    try {
      const skew = skewedPipelineRunner();
      const first = await runScript(h, skew.runner, TWO_STAGE, { args: { items: ["A", "B"] } });
      assert.equal(first.finished.status, "done");

      // The transposition is real, and the source run is where it happens: stage 2 for
      // B was issued before stage 2 for A, because B's stage 1 finished first.
      assert.deepEqual(
        skew.seen,
        ["s1 A", "s1 B", "s2 s1 B", "s2 s1 A"],
        "the arrival order is latency order — this is the input to the defect",
      );
      assert.deepEqual(dispatched(h.db, first.finished.id), skew.seen);

      // The positions are NOT latency order. They are the (stage, item) cells —
      // STAGE-major, so that structural order implies causal order: every stage-1 cell
      // happens-after its own item's stage-0 cell, and the stage-0 cells are mutually
      // concurrent and sort adjacently. Item-major numbering sorted every cell of item 0
      // ahead of item 1, which let a cell dispatched AFTER a divergence sort before it
      // and replay — a stale hit against a tree a live agent had already rewritten.
      const sourcePos = positions(h.db, first.finished.id);
      assert.deepEqual(sourcePos, {
        "s1 A": "0.0.0.0",
        "s1 B": "0.0.1.0",
        "s2 s1 A": "0.1.0.0",
        "s2 s1 B": "0.1.1.0",
      });

      // Relaunch the BYTE-IDENTICAL script. Every call must replay; a single live call
      // is the defect. The runner throws rather than answering, so a miss cannot pass
      // by quietly returning the right value.
      const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: noLiveCalls };
      const done = completion(h.bus);
      await withHome(
        h.home,
        () => rerunWorkflow(ctx, first.finished.id, { script: TWO_STAGE }),
      );
      const second = await done;

      assert.equal(second.status, "done");
      const rows = h.db.listWorkflowAgents(second.id);
      assert.equal(rows.length, 4);
      assert.deepEqual(
        rows.map((r) => r.status),
        ["cached", "cached", "cached", "cached"],
        "an unchanged pipeline must replay 4/4",
      );
      assert.deepEqual(second.result, first.finished.result);

      // And the reason it worked: the coordinates matched even though the two runs
      // dispatched their stage-2 calls in different orders. Without the gate the
      // relaunch resolves its replayed prefix instantly, so A reaches stage 2 first —
      // the exact transposition that used to break the prefix.
      assert.deepEqual(positions(h.db, second.id), sourcePos);
      assert.notDeepEqual(
        dispatched(h.db, second.id),
        dispatched(h.db, first.finished.id),
        "the two runs really did dispatch in different orders",
      );
    } finally {
      h.close();
    }
  },
);

test("parallel slots keep stable positions under varying latency", async () => {
  const h = harness();
  try {
    // Two calls per slot, with an await between them, so the SECOND call of each slot
    // is issued in completion order rather than slot order. Slot 0 is the slow one.
    const script = `
      return await parallel(args.items.map((x) => async () => {
        const probed = await agent(\`probe \${x}\`)
        return await agent(\`deep \${probed}\`)
      }))
    `;
    let releaseFirst = () => {};
    const gate = new Promise<void>((resolve) => (releaseFirst = resolve));
    const seen: string[] = [];
    const runner: AgentRunner = async (call) => {
      seen.push(call.prompt);
      if (call.prompt === "probe a") await gate;
      if (call.prompt === "deep probe c") releaseFirst();
      return call.prompt;
    };

    const first = await runScript(h, runner, script, { args: { items: ["a", "b", "c"] } });
    assert.equal(first.finished.status, "done");
    assert.deepEqual(seen, [
      "probe a",
      "probe b",
      "probe c",
      "deep probe b",
      "deep probe c",
      "deep probe a",
    ], "slot 0's second call arrives LAST — arrival order is not slot order");

    // Each slot's calls are numbered by the slot, not by when it finished.
    assert.deepEqual(positions(h.db, first.finished.id), {
      "probe a": "0.0.0",
      "deep probe a": "0.0.1",
      "probe b": "0.1.0",
      "deep probe b": "0.1.1",
      "probe c": "0.2.0",
      "deep probe c": "0.2.1",
    });

    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: noLiveCalls };
    const done = completion(h.bus);
    await withHome(h.home, () => rerunWorkflow(ctx, first.finished.id, { script }));
    const second = await done;
    assert.equal(second.status, "done");
    assert.ok(
      h.db.listWorkflowAgents(second.id).every((r) => r.status === "cached"),
      "every parallel slot replayed",
    );
    assert.deepEqual(positions(h.db, second.id), positions(h.db, first.finished.id));
  } finally {
    h.close();
  }
});

test("nested parallel-inside-pipeline keeps every coordinate distinct", async () => {
  const h = harness();
  try {
    // Spec §8's own script shape: a pipeline whose second stage fans out.
    const script = `
      return await pipeline(
        args.items,
        (item) => agent(\`review \${item}\`),
        (prev) => parallel([
          () => agent(\`verify 1 of \${prev}\`),
          () => agent(\`verify 2 of \${prev}\`),
        ]),
      )
    `;
    let releaseX = () => {};
    const gate = new Promise<void>((resolve) => (releaseX = resolve));
    const runner: AgentRunner = async (call) => {
      if (call.prompt === "review x") await gate;
      if (call.prompt === "verify 2 of review y") releaseX();
      return call.prompt;
    };

    const first = await runScript(h, runner, script, { args: { items: ["x", "y"] } });
    assert.equal(first.finished.status, "done");

    // Read left to right: pipeline 0 · stage s · item i · the parallel() that stage
    // opened (slot 0 of the stage frame) · its thunk slot · the agent in that thunk.
    // Stage before item: see the stage-major note in the transposition test above.
    const pos = positions(h.db, first.finished.id);
    assert.deepEqual(pos, {
      "review x": "0.0.0.0",
      "review y": "0.0.1.0",
      "verify 1 of review x": "0.1.0.0.0.0",
      "verify 2 of review x": "0.1.0.0.1.0",
      "verify 1 of review y": "0.1.1.0.0.0",
      "verify 2 of review y": "0.1.1.0.1.0",
    });
    assert.equal(
      new Set(Object.values(pos)).size,
      6,
      "six calls, six distinct coordinates — the nested fan-outs do not collide",
    );

    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: noLiveCalls };
    const done = completion(h.bus);
    await withHome(h.home, () => rerunWorkflow(ctx, first.finished.id, { script }));
    const second = await done;
    assert.equal(second.status, "done");
    assert.equal(h.db.listWorkflowAgents(second.id).filter((r) => r.status === "cached").length, 6);
    assert.deepEqual(positions(h.db, second.id), pos);
  } finally {
    h.close();
  }
});

test("a genuinely edited pipeline stage still ends the prefix", async () => {
  const h = harness();
  try {
    const edited = TWO_STAGE.replace("s2 ${prev}", "s2 THOROUGHLY ${prev}");
    assert.notEqual(edited, TWO_STAGE, "the fixture must actually differ");

    const skew = skewedPipelineRunner();
    const first = await runScript(h, skew.runner, TWO_STAGE, { args: { items: ["A", "B"] } });
    assert.equal(first.finished.status, "done");

    const live: string[] = [];
    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: echoRunner(live) };
    const done = completion(h.bus);
    await withHome(h.home, () => rerunWorkflow(ctx, first.finished.id, { script: edited }));
    const second = await done;

    assert.equal(second.status, "done");
    // Stage 1 is untouched at both cells, so it replays. Both stage-2 calls were
    // edited, so both run — structural positions do not make replay less conservative,
    // they only make it reproducible.
    const statuses = Object.fromEntries(
      h.db.listWorkflowAgents(second.id).map((r) => [r.prompt, r.status]),
    );
    assert.deepEqual(statuses, {
      "s1 A": "cached",
      "s1 B": "cached",
      "s2 THOROUGHLY s1 A": "done",
      "s2 THOROUGHLY s1 B": "done",
    });
    assert.deepEqual(live.sort(), ["s2 THOROUGHLY s1 A", "s2 THOROUGHLY s1 B"]);
  } finally {
    h.close();
  }
});

test("a first run reports no divergence — there is nothing to diverge from", async () => {
  const h = harness();
  try {
    const first = await runScript(h, echoRunner(), TWO_STAGE, { args: { items: ["A"] } });
    assert.equal(first.finished.status, "done");
    // Found by booting the real server: every call of a FIRST run is live, and folding
    // it against an empty plan called each one a divergence — "the source run never
    // made a call at 0.0.0.0" on a run that has no source run.
    const audit = replayAudit(emptyReplayPlan(), h.db.listWorkflowAgents(first.finished.id));
    assert.deepEqual(audit, { diverged: null, divergedAt: null, forced: 0 });
  } finally {
    h.close();
  }
});

test("comparePos orders coordinates component-wise as numbers", () => {
  // Text ordering gets this backwards at ten items, which is not an exotic fan-out.
  assert.equal(comparePos("0.9", "0.10") < 0, true);
  assert.equal(comparePos("0.10", "0.9") > 0, true);
  assert.equal(comparePos("0.0.1.0", "0.1.0.0") < 0, true);
  assert.equal(comparePos("2", "2"), 0);
  // A prefix sorts before what extends it.
  assert.equal(comparePos("2", "2.0") < 0, true);
});

test("journalKey carries the coordinate and the content hash, both recoverable", () => {
  const call: AgentCall = { prompt: "p", label: "l" };
  const key = journalKey("0.1.1.0", callKey(call));
  assert.deepEqual(splitJournalKey(key), { pos: "0.1.1.0", content: callKey(call) });
  // A pre-coordinate key still parses — it simply has no position half.
  assert.deepEqual(splitJournalKey("deadbeef"), { pos: null, content: "deadbeef" });
});

// ---------------------------------------------------------------------------
// the journal
// ---------------------------------------------------------------------------

test("rerunning an unchanged script issues zero live agent calls", async () => {
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
test("editing one prompt re-runs that call and everything after it", async () => {
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

test("a failed call re-runs live, and so does everything after it", async () => {
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

test("callKey changes with every field that changes what the agent is asked", () => {
  const base: AgentCall = { prompt: "p", label: "l" };
  const key = callKey(base);
  assert.equal(callKey({ ...base }), key, "the key is a pure function of the call");
  assert.notEqual(callKey({ ...base, prompt: "p2" }), key);
  assert.notEqual(callKey({ ...base, label: "l2" }), key);
  assert.notEqual(callKey({ ...base, phase: "Review" }), key);
  assert.notEqual(callKey({ ...base, model: "opus" }), key);
  assert.notEqual(callKey({ ...base, schema: { type: "object" } }), key);
});

test("distinctLabel finds the line a shared-preamble sibling has not claimed", () => {
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

test("stop kills the worker and interrupts in-flight agents", async () => {
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

// ---------------------------------------------------------------------------
// pause and stop — the gate has to bite on a FAN-OUT, and settle every row
// ---------------------------------------------------------------------------

test("pause stops a parallel() fan-out from starting anything more", async () => {
  const h = harness();
  try {
    const g = gatedRunner();
    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: g.runner };
    const done = completion(h.bus);
    const run = await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script: FANOUT_SCRIPT,
        meta: { name: "test", description: "fan-out pause" },
        concurrency: 2,
      }));

    // Six calls, two slots. Within the first tick the script has ISSUED all six —
    // that is what `parallel()` does — so four of them are already past the
    // pre-journal gate and merely parked on the semaphore. This is the state a pause
    // used to be unable to touch.
    await until("two agents to be in flight", () => g.started.length === 2);
    await until("all six calls to be journaled", () => h.db.listWorkflowAgents(run.id).length === 6);

    const paused = pauseWorkflow(ctx, run.id);
    assert.equal(paused.status, "paused");

    // Release the two in flight. Under the defect this drained the semaphore queue and
    // launched the next two, and then the next two, to completion — pause changed
    // nothing for the only shape that needs it.
    assert.ok(g.release("work 0"));
    assert.ok(g.release("work 1"));
    await until(
      "the two in-flight agents to land done",
      () => h.db.listWorkflowAgents(run.id).filter((a) => a.status === "done").length === 2,
    );
    // The assertion is that something did NOT happen, so give it room to happen.
    await delay(80);

    assert.deepEqual(
      g.started,
      ["work 0", "work 1"],
      "no further agent started while the run was paused",
    );
    const rows = h.db.listWorkflowAgents(run.id);
    assert.equal(rows.length, 6, "every issued call is journaled — the run view shows the queue");
    assert.deepEqual(
      rows.map((a) => a.status),
      ["done", "done", "queued", "queued", "queued", "queued"],
      "a parked call stays queued; it is never shown as running",
    );
    assert.deepEqual(
      rows.filter((a) => a.status === "queued").map((a) => a.sessionId),
      [null, null, null, null],
      "a queued call has no subagent session, because none was launched",
    );
    assert.equal(h.db.getWorkflow(run.id)?.status, "paused");

    // Resume admits exactly the semaphore's width, not the whole backlog.
    resumeWorkflow(ctx, run.id);
    await until("two more agents to start", () => g.started.length === 4);
    await delay(30);
    assert.equal(g.started.length, 4, "resume respects the run's own concurrency");

    // Drain the rest. Each release frees a slot, which admits the next.
    while (h.db.getWorkflow(run.id)?.status === "running") {
      g.releaseAll();
      await delay(10);
    }
    const finished = await done;
    assert.equal(finished.status, "done");
    assert.deepEqual(finished.result, [
      "report work 0",
      "report work 1",
      "report work 2",
      "report work 3",
      "report work 4",
      "report work 5",
    ]);
    assert.equal(
      h.db.listWorkflowAgents(run.id).filter((a) => a.status === "done").length,
      6,
    );
  } finally {
    h.close();
  }
});

test("pause still gates a strictly sequential script (regression)", async () => {
  const h = harness();
  try {
    const g = gatedRunner();
    const logs: string[] = [];
    h.bus.subscribe((e) => {
      if (e.type === "workflow.log") logs.push((e.data as { line: string }).line);
    });

    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: g.runner };
    const done = completion(h.bus);
    const run = await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script: `const a = await agent('first')
                 log('past the first')
                 const b = await agent('second')
                 return [a, b]`,
        meta: { name: "test", description: "sequential pause" },
        concurrency: 4,
      }));

    await until("the first agent to start", () => g.started.length === 1);
    assert.equal(pauseWorkflow(ctx, run.id).status, "paused");

    // The in-flight agent is untouched by the pause and finishes normally.
    assert.ok(g.release("first"));
    await until(
      "the first agent to land done",
      () => h.db.listWorkflowAgents(run.id)[0]?.status === "done",
    );

    // `log` is the script's own signal that it got past the first call, so "nothing
    // new started" is anchored to a fact rather than to a sleep.
    await until("the script to reach its second call", () => logs.includes("past the first"));
    await delay(80);
    assert.deepEqual(g.started, ["first"], "no second agent while paused");
    assert.equal(
      h.db.listWorkflowAgents(run.id).length,
      1,
      "a call parked before it journals writes no row — the run view shows nothing pending",
    );

    resumeWorkflow(ctx, run.id);
    await until("the second agent to start", () => g.started.length === 2);
    g.releaseAll();
    const finished = await done;
    assert.equal(finished.status, "done");
    assert.deepEqual(finished.result, ["report first", "report second"]);
  } finally {
    h.close();
  }
});

test("stopping a paused fan-out settles every row — nothing is left queued", async () => {
  const h = harness();
  try {
    const g = gatedRunner();
    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: g.runner };
    const done = completion(h.bus);
    const run = await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script: FANOUT_SCRIPT,
        meta: { name: "test", description: "pause then stop" },
        concurrency: 2,
      }));

    await until("two agents to be in flight", () => g.started.length === 2);
    await until("all six calls to be journaled", () => h.db.listWorkflowAgents(run.id).length === 6);
    pauseWorkflow(ctx, run.id);
    g.release("work 0");
    g.release("work 1");
    await until(
      "four calls to be parked",
      () => h.db.listWorkflowAgents(run.id).filter((a) => a.status === "queued").length === 4,
    );

    // The sequence spec §8 recommends: pause to preserve what is in flight, then stop.
    const stopped = stopWorkflow(ctx, run.id);
    assert.equal(stopped.status, "stopped");
    assert.equal((await done).status, "stopped");
    await delay(60); // let every parked call unwind

    const rows = h.db.listWorkflowAgents(run.id);
    assert.equal(rows.length, 6, "a stop journals no new rows");
    assert.deepEqual(
      rows.filter((a) => a.status === "queued" || a.status === "running").map((a) => a.label),
      [],
      "a stopped run leaves NO row in a non-terminal state",
    );
    assert.deepEqual(
      rows.map((a) => a.status),
      ["done", "done", "stopped", "stopped", "stopped", "stopped"],
    );
    for (const row of rows.filter((a) => a.status === "stopped")) {
      assert.ok(row.finishedAt, `${row.label} has a finish time`);
      // Each parked call also settles ITSELF as it unwinds, and says which of the two
      // things happened to it. "stopped" alone reads the same for an agent that was
      // interrupted mid-answer and one that never got a slot; the run view and the
      // relaunch report both care about the difference (spec §6, error text).
      assert.match(String(row.error), /queued and never started/);
    }
    assert.deepEqual(g.started, ["work 0", "work 1"], "a stop starts nothing");
  } finally {
    h.close();
  }
});

test("stopping a paused sequential run leaves no orphan row behind", async () => {
  const h = harness();
  try {
    const g = gatedRunner();
    const logs: string[] = [];
    h.bus.subscribe((e) => {
      if (e.type === "workflow.log") logs.push((e.data as { line: string }).line);
    });

    const ctx: WorkflowCtx = { db: h.db, bus: h.bus, runner: g.runner };
    const done = completion(h.bus);
    const run = await withHome(h.home, () =>
      startWorkflow(ctx, {
        sessionId: h.sessionId,
        script: `const a = await agent('first')
                 log('past the first')
                 const b = await agent('second')
                 return [a, b]`,
        meta: { name: "test", description: "park then stop" },
        concurrency: 4,
      }));

    await until("the first agent to start", () => g.started.length === 1);
    pauseWorkflow(ctx, run.id);
    g.release("first");
    await until("the script to park on its second call", () => logs.includes("past the first"));
    await delay(30);
    assert.equal(h.db.listWorkflowAgents(run.id).length, 1, "the parked call has journaled nothing");

    stopWorkflow(ctx, run.id);
    assert.equal((await done).status, "stopped");
    // The stop opens the gate so the parked call does not leak; the call wakes to an
    // aborted run. Under the defect it journaled a fresh `queued` row AFTER the sweep
    // that was supposed to settle everything, and nothing in the process ever could.
    await delay(60);

    const rows = h.db.listWorkflowAgents(run.id);
    assert.equal(rows.length, 1, "a call unparked by the stop journals no row");
    assert.equal(rows[0].status, "done");
    assert.deepEqual(
      rows.filter((a) => a.status === "queued" || a.status === "running"),
      [],
      "a stopped run leaves NO row in a non-terminal state",
    );
  } finally {
    h.close();
  }
});

test("a finished run notifies its owning session", async () => {
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

test("boot recovery orphans a run the previous process left running", () => {
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

test("workflowSummary omits the script and counts the journal", async () => {
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

// Regression: a pure reorder must report `moved`, not `changed`. The occupied-slot
// test used to run first, which made `moved` unreachable for any reorder that
// preserves the call count — i.e. the commonest kind.
test("classifyDivergence: a swap is a MOVE, not an edit", () => {
  const plan = emptyReplayPlan();
  for (const [i, content] of ["review A", "review B", "review C"].entries()) {
    const step = {
      pos: String(i),
      content,
      key: `${i}:${content}`,
      idx: i,
      result: "ok",
      prompt: content,
    };
    plan.steps.push(step);
    plan.byPos.set(step.pos, step);
    plan.byContent.set(content, [step.pos]);
  }
  // Relaunch reorders to B, A, C. Position 0 now asks for "review B", which the source
  // ran at position 1. Not one prompt was edited.
  const d = classifyDivergence(plan, "0", "review B");
  assert.equal(d.kind, "moved");
  assert.equal(d.sourcePos, "1");
  assert.match(d.reason, /MOVED/);
});

test("classifyDivergence: a genuine edit is still reported as changed", () => {
  const plan = emptyReplayPlan();
  const step = { pos: "0", content: "review A", key: "0:review A", idx: 0, result: "ok", prompt: "review A" };
  plan.steps.push(step);
  plan.byPos.set("0", step);
  plan.byContent.set("review A", ["0"]);
  const d = classifyDivergence(plan, "0", "review A CAREFULLY");
  assert.equal(d.kind, "changed");
});
