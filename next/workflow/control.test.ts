/**
 * Workflow lifecycle control, driven END TO END: a real `permissions: "none"` worker
 * running a real script, the real engine and journal underneath it, the real HTTP
 * router on top, and a fake subagent launcher standing in for the only part that
 * would need a key.
 *
 * The two tests this file exists for are the acceptance criteria, and both are about
 * what happens to work that is ALREADY RUNNING when a control verb arrives:
 *
 *   - **stop interrupts its children rather than orphaning them.** A stop that only
 *     killed the worker would leave a fan-out of subagent turns billing with nobody
 *     left to read their reports. The assertion is on the turn registry — after
 *     `POST /workflows/:id/stop` no child turn is still running — not merely on the
 *     run's status column, because writing `stopped` is the easy half and the half
 *     that can be wrong without any test noticing (spec §8).
 *   - **pause admits nothing new and disturbs nothing in flight.** The running agent
 *     finishes normally and lands `done`; the script's next `agent()` call parks
 *     BEFORE it journals, so the run view never shows an agent that has not started.
 *     Both halves are asserted, because a "pause" that killed the in-flight agent
 *     would pass a test that only counted new ones.
 *
 * The third invariant here is the one spec §8 states and nothing else enforces:
 * **subagent caps do not apply inside a workflow.** The exemption is proved by
 * running six agents at once through the REAL cap path — under the tree-wide cap of
 * 4 the fifth launch would be refused, and the test asserts six live children with no
 * failed row.
 *
 * Waiting is done on facts, never on a sleep-and-hope: a run's terminal status
 * arrives on the bus, a script's progress arrives as a `workflow.log` line, and
 * everything else polls a condition with a deadline. The one deliberate short delay
 * is in the pause test, where the assertion is that something did NOT happen.
 *
 * Hermetic and offline: an in-memory database, a real bus, no network, no key, and
 * `BOUGH_HOME` pointed at a temp dir for the duration of every engine call so the
 * script mirror never touches the real `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * denied by this environment's egress policy, so the jsr import declared in
 * `deno.json` cannot resolve.
 */
import assert from "node:assert/strict";
import type { SubagentLaunch, SubagentResult } from "../agents/subagent.ts";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { LaunchFn } from "../hostfn/delegate.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { WorkflowAgent, WorkflowRun } from "../schema/parts.ts";
import { createHandler } from "../server/app.ts";
import { TurnRegistry } from "../turn/queue.ts";
import type { AppCtx, TurnCtx } from "../types.ts";
import {
  startWorkflowRun,
  type WithWorkflowControl,
  WorkflowAgentRegistry,
  type WorkflowControlDeps,
} from "./control.ts";
import { isWorkflowLive } from "./run.ts";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/** One launched stand-in for a subagent, with its settlement in the test's hands. */
interface FakeChild {
  sessionId: string;
  task: string;
  label: string;
  /** Settle the child's turn. Called by the test, or by the interrupt cascade. */
  finish(result: Partial<SubagentResult>): void;
  settled: boolean;
}

interface Harness {
  db: SqliteDb;
  bus: Bus;
  ctx: AppCtx & WithWorkflowControl;
  handler: (req: Request) => Promise<Response>;
  sessionId: string;
  events: BoughEvent[];
  logs: string[];
  children: FakeChild[];
  registry: TurnRegistry;
  agents: WorkflowAgentRegistry;
  control: WorkflowControlDeps;
  home: string;
  close(): void;
}

function harness(): Harness {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  const logs: string[] = [];
  bus.subscribe((e) => {
    events.push(e);
    if (e.type === "workflow.log") logs.push((e.data as { line: string }).line);
  });
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "the orchestrator",
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });

  const children: FakeChild[] = [];
  const registry = new TurnRegistry();
  const agents = new WorkflowAgentRegistry();

  /**
   * A subagent that exists as far as the database, the tree and the turn registry
   * are concerned, and whose turn ends exactly when the test says so — or when
   * something interrupts it, which is the path the stop test measures.
   */
  const launch: LaunchFn = (ctx: TurnCtx, task: string, opts): SubagentLaunch => {
    const child = db.createSession({
      id: crypto.randomUUID(),
      title: opts.name ?? "agent",
      kind: "subagent",
      createdAt: Date.now(),
      parentId: null,
      originId: ctx.sessionId,
      originMessageId: ctx.messageId,
      workspace: ctx.workspace,
      originDir: ctx.workspace,
    });
    const taskMessage = db.createMessage({
      id: crypto.randomUUID(),
      sessionId: child.id,
      role: "user",
      parts: [{ type: "text", text: task }],
      pending: false,
      createdAt: Date.now(),
    });
    const supervisor = db.createMessage({
      id: crypto.randomUUID(),
      sessionId: child.id,
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: Date.now(),
    });

    // Claiming the session in the registry is what makes `interruptTurn` reach this
    // child at all — the registry, not the database, is the authority on "running".
    const controller = registry.begin(child.id);
    let settle!: (r: SubagentResult) => void;
    const result = new Promise<SubagentResult>((resolve) => (settle = resolve));

    const record: FakeChild = {
      sessionId: child.id,
      task,
      label: opts.name ?? "",
      settled: false,
      finish(partial) {
        if (record.settled) return;
        record.settled = true;
        registry.end(child.id, controller);
        db.updateMessage(supervisor.id, [{ type: "text", text: partial.report ?? "" }], false);
        settle({
          sessionId: child.id,
          title: child.title,
          ok: partial.ok ?? true,
          status: partial.status ?? (partial.ok === false ? "error" : "done"),
          report: partial.report ?? "done",
          changedFiles: [],
        });
      },
    };
    // The cascade under test: a stop aborts the child's turn, and a turn that was
    // aborted reports `interrupted` — not silence, and not `done`.
    controller.signal.addEventListener("abort", () => {
      record.finish({ ok: false, status: "interrupted", report: "stopped mid-flight" });
    }, { once: true });

    children.push(record);
    return {
      sessionId: child.id,
      title: child.title,
      session: child,
      taskMessage,
      messageId: supervisor.id,
      result,
    };
  };

  const control: WorkflowControlDeps = { launch, registry, agents };
  const ctx: AppCtx & WithWorkflowControl = { db, bus, workflowControl: control };
  const home = Deno.makeTempDirSync({ prefix: "bough-wfctl-" });

  return {
    db,
    bus,
    ctx,
    handler: createHandler(ctx, { onUnexpectedError: () => {} }),
    sessionId: session.id,
    events,
    logs,
    children,
    registry,
    agents,
    control,
    home,
    close() {
      db.close();
      try {
        Deno.removeSync(home, { recursive: true });
      } catch { /* already gone */ }
    },
  };
}

/** Run one call with `BOUGH_HOME` relocated, then put the environment back. */
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

const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Poll a condition with a deadline. Every wait in this file goes through here. */
async function until(what: string, cond: () => boolean, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await delay(5);
  }
}

interface Answer<T> {
  status: number;
  body: T;
}

async function request<T = Record<string, unknown>>(
  h: Harness,
  method: string,
  path: string,
  body?: unknown,
): Promise<Answer<T>> {
  const res = await withHome(h.home, () =>
    h.handler(
      new Request(`http://127.0.0.1:4321${path}`, {
        method,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }),
    ));
  return { status: res.status, body: (await res.json()) as T };
}

/** Resolves with the run row the first time a run reaches a terminal status. */
function completion(bus: Bus, ms = 15_000): Promise<WorkflowRun> {
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

const META = (name: string) =>
  `export const meta = { name: '${name}', description: 'a control test' }\n`;

/** Two agents in flight at once, failures tolerated — `parallel` never rejects. */
const PARALLEL_SCRIPT = META("two-in-flight") +
  `const out = await parallel([
     () => agent('alpha', { label: 'alpha' }),
     () => agent('beta', { label: 'beta' }),
   ])
   return out`;

/** Strictly sequential: the second call cannot be made until the first returns. */
const SEQUENTIAL_SCRIPT = META("one-then-one") +
  `const first = await agent('first', { label: 'first' })
   log('after first')
   const second = await agent('second', { label: 'second' })
   return [first, second]`;

function rowsOf(h: Harness, runId: string): WorkflowAgent[] {
  return h.db.listWorkflowAgents(runId);
}

// ---------------------------------------------------------------------------
// stop — the acceptance criterion
// ---------------------------------------------------------------------------

Deno.test("stop kills the worker AND interrupts the subagent turns in flight", async () => {
  const h = harness();
  try {
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: PARALLEL_SCRIPT,
    });
    assert.equal(started.status, 201);
    const runId = started.body.id;
    assert.equal(started.body.status, "running");

    await until("both agents to be in flight", () => h.children.length === 2);
    assert.deepEqual(h.registry.runningSessions.length, 2);
    assert.deepEqual(rowsOf(h, runId).map((a) => a.status), ["running", "running"]);
    // Both calls are singly addressable while they run — that is what per-agent
    // control needs and what a claim-by-key scheme would not give.
    assert.equal(h.agents.forRun(runId).length, 2);

    const stopped = await request<WorkflowRun>(h, "POST", `/workflows/${runId}/stop`);
    assert.equal(stopped.status, 200);
    assert.equal(stopped.body.status, "stopped");

    // THE assertion: the children were interrupted, not orphaned. Nothing is left
    // running in the registry, and each child's turn ended through the abort path.
    assert.deepEqual(h.registry.runningSessions, []);
    assert.ok(h.children.every((c) => c.settled), "every child's turn settled");
    assert.equal(isWorkflowLive(runId), false, "the run is no longer live");

    for (const row of rowsOf(h, runId)) {
      assert.equal(row.status, "stopped", `agent ${row.label} is stopped`);
      assert.ok(row.sessionId, "a stopped agent still points at its branch");
    }

    // The handles come back as the calls unwind, so a later stop cannot reach a
    // controller for work that is over.
    await until("the live handles to be released", () => h.agents.forRun(runId).length === 0);

    const detail = await request<{ workflow: WorkflowRun; agents: unknown[]; live: boolean }>(
      h,
      "GET",
      `/workflows/${runId}`,
    );
    assert.equal(detail.status, 200);
    assert.equal(detail.body.workflow.status, "stopped");
    assert.equal(detail.body.live, false);
    assert.equal(detail.body.agents.length, 2);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// pause — the acceptance criterion
// ---------------------------------------------------------------------------

Deno.test("pause lets the running agent finish and admits no new ones; resume releases", async () => {
  const h = harness();
  try {
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: SEQUENTIAL_SCRIPT,
    });
    const runId = started.body.id;

    await until("the first agent to launch", () => h.children.length === 1);

    const paused = await request<WorkflowRun>(h, "POST", `/workflows/${runId}/pause`);
    assert.equal(paused.status, 200);
    assert.equal(paused.body.status, "paused");

    // Half one: the agent already in flight is untouched by the pause and finishes
    // normally. A pause that reached it would show up here as `stopped`.
    assert.equal(h.children[0].settled, false, "pause did not disturb the running agent");
    h.children[0].finish({ ok: true, report: "one" });
    await until(
      "the in-flight agent to land done",
      () => rowsOf(h, runId)[0]?.status === "done",
    );
    assert.equal(rowsOf(h, runId)[0].result, "one");

    // Half two: the script runs on to its next call and parks. `log('after first')`
    // is the script's own signal that it got past the first agent, so the "nothing
    // new started" assertion is anchored to a fact rather than to a sleep.
    await until("the script to reach its second call", () => h.logs.includes("after first"));
    await delay(50);
    assert.equal(h.children.length, 1, "no second agent was launched while paused");
    assert.equal(
      rowsOf(h, runId).length,
      1,
      "a parked call journals nothing — the run view never shows an agent that has not started",
    );
    assert.equal(h.db.getWorkflow(runId)?.status, "paused");

    const finish = completion(h.bus);
    const resumed = await request<WorkflowRun>(h, "POST", `/workflows/${runId}/resume`);
    assert.equal(resumed.status, 200);
    assert.equal(resumed.body.status, "running");

    await until("the second agent to launch", () => h.children.length === 2);
    h.children[1].finish({ ok: true, report: "two" });

    const done = await finish;
    assert.equal(done.status, "done");
    assert.deepEqual(done.result, ["one", "two"]);
    assert.deepEqual(rowsOf(h, runId).map((a) => a.status), ["done", "done"]);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the run's own semaphore, and the caps that do not apply to it
// ---------------------------------------------------------------------------

Deno.test("a run's semaphore meters the fan-out; the subagent caps do not apply", async () => {
  const h = harness();
  try {
    const script = META("six-at-once") +
      `return await parallel(
         [0,1,2,3,4,5].map((i) => () => agent('work ' + i, { label: 'w' + i })),
       )`;

    const finish = completion(h.bus);
    // Six at once is the point: the tree-wide subagent cap is 4, so a fifth launch
    // would be REFUSED if the caps applied here — this path takes an exempt lease.
    const run = await withHome(
      h.home,
      () =>
        startWorkflowRun(
          h.ctx,
          { sessionId: h.sessionId, script, concurrency: 6 },
          h.control,
        ),
    );

    await until("all six agents to be in flight at once", () => h.children.length === 6);
    assert.equal(h.registry.runningSessions.length, 6);
    assert.deepEqual(
      rowsOf(h, run.id).filter((a) => a.status === "error").map((a) => a.error),
      [],
      "no launch was refused by a cap",
    );

    for (const child of h.children) child.finish({ ok: true, report: child.task });
    const done = await finish;
    assert.equal(done.status, "done");
    assert.deepEqual(done.result, ["work 0", "work 1", "work 2", "work 3", "work 4", "work 5"]);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// single-agent control
// ---------------------------------------------------------------------------

Deno.test("stopping one agent fails that call only — the run carries on", async () => {
  const h = harness();
  try {
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: PARALLEL_SCRIPT,
    });
    const runId = started.body.id;
    await until("both agents to be in flight", () => h.children.length === 2);

    const finish = completion(h.bus);
    const alpha = rowsOf(h, runId).find((a) => a.label === "alpha")!;
    const answer = await request<WorkflowAgent>(
      h,
      "POST",
      `/workflows/${runId}/agents/${alpha.id}/stop`,
    );
    assert.equal(answer.status, 200);

    // The child's turn is interrupted; its sibling is untouched and still running.
    await until("alpha's turn to end", () => !h.registry.isRunning(alpha.sessionId!));
    const beta = h.children.find((c) => c.label === "beta")!;
    assert.equal(beta.settled, false, "the sibling agent kept running");

    h.children.find((c) => c.label === "beta")!.finish({ ok: true, report: "beta-report" });
    const done = await finish;

    // `parallel` maps the failed slot to null and never rejects (spec §8), so the
    // run completes with the sibling's result intact.
    assert.equal(done.status, "done");
    assert.deepEqual(done.result, [null, "beta-report"]);

    const rows = rowsOf(h, runId);
    const alphaRow = rows.find((a) => a.id === alpha.id)!;
    assert.notEqual(alphaRow.status, "done");
    assert.match(String(alphaRow.error), /interrupted/);
    assert.equal(rows.find((a) => a.label === "beta")!.status, "done");
  } finally {
    h.close();
  }
});

Deno.test("restarting one agent re-issues it on a fresh session, script still parked", async () => {
  const h = harness();
  try {
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: PARALLEL_SCRIPT,
    });
    const runId = started.body.id;
    await until("both agents to be in flight", () => h.children.length === 2);

    const finish = completion(h.bus);
    const alpha = rowsOf(h, runId).find((a) => a.label === "alpha")!;
    const firstSession = alpha.sessionId;
    const answer = await request<WorkflowAgent>(
      h,
      "POST",
      `/workflows/${runId}/agents/${alpha.id}/restart`,
    );
    assert.equal(answer.status, 200);

    // A third child for the same call: the abandoned attempt's turn is interrupted
    // and the SAME prompt is re-issued on a new session. One journal row throughout —
    // the script is still awaiting the promise it was already awaiting.
    await until("the call to be re-issued", () => h.children.length === 3);
    assert.equal(h.children[2].task, "alpha");
    assert.equal(rowsOf(h, runId).length, 2, "a restart does not journal a second row");
    const again = rowsOf(h, runId).find((a) => a.id === alpha.id)!;
    assert.equal(again.status, "running");
    assert.notEqual(again.sessionId, firstSession);

    for (const child of h.children) child.finish({ ok: true, report: `${child.task}-report` });
    const done = await finish;
    assert.equal(done.status, "done");
    assert.deepEqual(done.result, ["alpha-report", "beta-report"]);
  } finally {
    h.close();
  }
});

Deno.test("single-agent control refuses what it cannot reach, and says which", async () => {
  const h = harness();
  try {
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: PARALLEL_SCRIPT,
    });
    const runId = started.body.id;
    await until("both agents to be in flight", () => h.children.length === 2);
    const alpha = rowsOf(h, runId).find((a) => a.label === "alpha")!;

    const bogus = await request<{ error: string }>(
      h,
      "POST",
      `/workflows/${runId}/agents/${alpha.id}/frobnicate`,
    );
    assert.equal(bogus.status, 400);
    assert.match(bogus.body.error, /'stop'/);
    assert.match(bogus.body.error, /'restart'/);

    const missing = await request<{ error: string }>(
      h,
      "POST",
      `/workflows/${runId}/agents/nope/stop`,
    );
    assert.equal(missing.status, 404);

    await request(h, "POST", `/workflows/${runId}/stop`);
    await until("the run to wind down", () => !isWorkflowLive(runId));

    // A finished call is not stoppable, and the message says so rather than 500ing.
    const late = await request<{ error: string }>(
      h,
      "POST",
      `/workflows/${runId}/agents/${alpha.id}/stop`,
    );
    assert.equal(late.status, 409);
    assert.match(late.body.error, /not running/);
  } finally {
    h.close();
  }
});

// ---------------------------------------------------------------------------
// the REST surface itself
// ---------------------------------------------------------------------------

Deno.test("the workflow routes are reachable, and refuse at the door", async () => {
  const h = harness();
  try {
    const empty = await request<{ workflows: unknown[] }>(h, "GET", "/workflows");
    assert.equal(empty.status, 200);
    assert.deepEqual(empty.body.workflows, []);

    const missing = await request<{ error: string }>(h, "GET", "/workflows/nope");
    assert.equal(missing.status, 404);
    assert.match(missing.body.error, /nope/);

    // Submit-time rejection: a script with no `meta` literal never reaches a worker
    // and never writes a row (spec §8, T5.3's "reject at submit, not mid-run").
    const bad = await request<{ error: string }>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: "return 1",
    });
    assert.equal(bad.status, 400);
    assert.match(bad.body.error, /export const meta/);
    assert.deepEqual(h.db.listWorkflows().length, 0, "a refused submit persists nothing");

    const noSession = await request<{ error: string }>(h, "POST", "/workflows", {
      sessionId: "ghost",
      script: PARALLEL_SCRIPT,
    });
    assert.equal(noSession.status, 404);

    // A live run, so list/get/pause have something real to answer about.
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: PARALLEL_SCRIPT,
    });
    const runId = started.body.id;
    await until("both agents to be in flight", () => h.children.length === 2);

    const listed = await request<{ workflows: { id: string; script?: string }[] }>(
      h,
      "GET",
      `/workflows?session=${h.sessionId}`,
    );
    assert.deepEqual(listed.body.workflows.map((w) => w.id), [runId]);
    assert.equal(
      listed.body.workflows[0].script,
      undefined,
      "a summary omits the script — a list of N copies of it is a payload nobody reads",
    );

    const wrongMethod = await h.handler(
      new Request(`http://127.0.0.1:4321/workflows/${runId}/stop`, { method: "GET" }),
    );
    assert.equal(wrongMethod.status, 405);

    await request(h, "POST", `/workflows/${runId}/stop`);
    await until("the run to wind down", () => !isWorkflowLive(runId));

    // Pausing a run nothing is executing is a 409, not a courtesy 200: there is no
    // worker left to instruct.
    const latePause = await request<{ error: string }>(h, "POST", `/workflows/${runId}/pause`);
    assert.equal(latePause.status, 409);

    // Stop is idempotent — the caller asked for a state the run is already in.
    const reStop = await request<WorkflowRun>(h, "POST", `/workflows/${runId}/stop`);
    assert.equal(reStop.status, 200);
    assert.equal(reStop.body.status, "stopped");
  } finally {
    h.close();
  }
});

Deno.test("rerun replays the journal of a finished run and refuses a live one", async () => {
  const h = harness();
  try {
    const started = await request<WorkflowRun>(h, "POST", "/workflows", {
      sessionId: h.sessionId,
      script: SEQUENTIAL_SCRIPT,
    });
    const runId = started.body.id;
    const finish = completion(h.bus);

    // A rerun while the source is still running would race the journal it replays.
    await until("the first agent to launch", () => h.children.length === 1);
    const early = await request<{ error: string }>(h, "POST", `/workflows/${runId}/rerun`);
    assert.equal(early.status, 409);
    assert.match(early.body.error, /still running/);

    h.children[0].finish({ ok: true, report: "one" });
    await until("the second agent to launch", () => h.children.length === 2);
    h.children[1].finish({ ok: true, report: "two" });
    const done = await finish;
    assert.equal(done.status, "done");

    const replayFinished = completion(h.bus);
    const rerun = await request<WorkflowRun>(h, "POST", `/workflows/${runId}/rerun`);
    assert.equal(rerun.status, 201);
    assert.equal(rerun.body.resumeOf, runId);

    const replayed = await replayFinished;
    assert.equal(replayed.status, "done");
    assert.deepEqual(replayed.result, ["one", "two"]);
    assert.equal(h.children.length, 2, "an unchanged rerun issues ZERO live agent calls");
    assert.deepEqual(
      rowsOf(h, replayed.id).map((a) => a.status),
      ["cached", "cached"],
    );
  } finally {
    h.close();
  }
});
