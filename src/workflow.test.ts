import { assert, assertEquals, assertRejects } from "jsr:@std/assert@1";
import { Bus } from "./bus.ts";
import { Db } from "./db/db.ts";
import type { BoughEvent } from "./schema/parts.ts";
import {
  type AgentCall,
  callKey,
  evalMeta,
  isWorkflowLive,
  metaLiteral,
  pauseWorkflow,
  recoverOrphanedWorkflows,
  rerunWorkflow,
  resumeWorkflow,
  startWorkflow,
  stopWorkflow,
  type WorkflowCtx,
  workflowVerb,
} from "./workflow.ts";

// ---- fixtures --------------------------------------------------------------

const META = `export const meta = {
  name: 'test-flow',
  description: 'a test workflow',
  phases: [{ title: 'Find' }, { title: 'Verify', detail: 'check findings' }],
}\n`;

// Keep script mirrors out of the real ~/.bough.
Deno.env.set("BOUGH_WORKFLOW_DIR", Deno.makeTempDirSync({ prefix: "bough-wf-test" }));

function fixture(runner?: WorkflowCtx["runner"]) {
  const db = new Db(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const session = db.createSession({
    id: crypto.randomUUID(),
    parentId: null,
    title: "t",
    kind: "root",
    createdAt: Date.now(),
  });
  const notes: string[] = [];
  const ctx: WorkflowCtx = {
    db,
    bus,
    runner: runner ?? ((call) => Promise.resolve(`ran: ${call.prompt}`)),
    notify: (_sid, text) => notes.push(text),
  };
  return { db, bus, events, session, ctx, notes };
}

/** Await the run's terminal status via bus events (the worker is async). */
function finished(events: BoughEvent[], runId: string, timeoutMs = 15_000): Promise<string> {
  return new Promise((resolve, reject) => {
    const t0 = Date.now();
    const poll = () => {
      const last = events.filter((e) =>
        e.type === "workflow.updated" && (e.data as { id: string }).id === runId
      ).at(-1);
      const status = last && (last.data as { status: string }).status;
      if (status && status !== "running" && status !== "paused") return resolve(status);
      if (Date.now() - t0 > timeoutMs) return reject(new Error("workflow did not finish"));
      setTimeout(poll, 20);
    };
    poll();
  });
}

// ---- meta ------------------------------------------------------------------

Deno.test("metaLiteral: extracts the literal, surviving strings and comments", () => {
  const lit = metaLiteral(
    `export const meta = {
      name: 'x', // trailing {comment}
      description: "has {braces} and 'quotes'",
      phases: [{ title: \`t {y}\` }], /* {block} */
    }
    const rest = { not: "meta" }`,
  );
  assert(lit !== null && lit.startsWith("{") && lit.endsWith("}"));
  assert(lit.includes("has {braces}"));
  assert(!lit.includes("not:"));
  assertEquals(metaLiteral("const meta = {}"), null); // must be exported
});

Deno.test("evalMeta: validates shape; rejects a missing/invalid meta", async () => {
  const meta = await evalMeta(META + "return 1");
  assertEquals(meta.name, "test-flow");
  assertEquals(meta.phases?.length, 2);
  await assertRejects(() => evalMeta("return 1"), Error, "export const meta");
  await assertRejects(
    () => evalMeta("export const meta = { name: 'x' }\nreturn 1"),
    Error,
    "invalid workflow meta",
  );
});

Deno.test("callKey: stable for equal calls, distinct across prompt/opts changes", () => {
  const a: AgentCall = { prompt: "p", label: "l", phase: "F" };
  assertEquals(callKey(a), callKey({ ...a }));
  assert(callKey(a) !== callKey({ ...a, prompt: "q" }));
  assert(callKey(a) !== callKey({ ...a, model: "haiku" }));
});

// ---- running ---------------------------------------------------------------

Deno.test("startWorkflow: runs the script, journals agents, returns the result", async () => {
  const { db, events, session, ctx, notes } = fixture();
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `
      phase('Find')
      const found = await parallel(['a', 'b', 'c'].map((x) => () =>
        agent('find ' + x, { label: 'find:' + x, phase: 'Find' })))
      phase('Verify')
      const verdict = await agent('verify ' + found.join(','), { label: 'verify' })
      log('all done')
      return { found, verdict }
    `,
  });
  assertEquals(run.status, "running");
  assertEquals(run.name, "test-flow");
  assertEquals(await finished(events, run.id), "done");

  const done = db.getWorkflow(run.id)!;
  const result = done.result as { found: string[]; verdict: string };
  assertEquals(result.found.toSorted(), ["ran: find a", "ran: find b", "ran: find c"]);
  assertEquals(result.verdict, "ran: verify ran: find a,ran: find b,ran: find c");
  assertEquals(done.currentPhase, "Verify");

  const agents = db.listWorkflowAgents(run.id);
  assertEquals(agents.length, 4);
  assertEquals(agents.filter((a) => a.status === "done").length, 4);
  assertEquals(agents.filter((a) => a.phase === "Find").length, 3);
  // The verify call's phase falls back to the current phase() at call time.
  assertEquals(agents.find((a) => a.label === "verify")?.phase, "Verify");
  // Completion notified the owning session.
  assertEquals(notes.length, 1);
  assert(notes[0].includes('[workflow done] "test-flow"'));
  assert(notes[0].includes("4/4 agents succeeded"));
  // log() reached the bus.
  assert(events.some((e) =>
    e.type === "workflow.log" && (e.data as { line: string }).line === "all done"
  ));
});

Deno.test("agent failure: rejects into the script; parallel maps it to null", async () => {
  const { db, events, session, ctx } = fixture((call) =>
    call.prompt.includes("bad")
      ? Promise.reject(new Error("boom"))
      : Promise.resolve("ok:" + call.prompt)
  );
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `
      const r = await parallel([() => agent('good'), () => agent('bad')])
      let caught = null
      try { await agent('bad again') } catch (e) { caught = e.message }
      return { r, caught }
    `,
  });
  assertEquals(await finished(events, run.id), "done");
  const result = db.getWorkflow(run.id)!.result as { r: unknown[]; caught: string };
  assertEquals(result.r, ["ok:good", null]);
  assertEquals(result.caught, "boom");
  const agents = db.listWorkflowAgents(run.id);
  assertEquals(agents.filter((a) => a.status === "error").length, 2);
});

Deno.test("pipeline: stages flow per item; a throwing stage drops the item", async () => {
  const { db, events, session, ctx } = fixture((call) =>
    call.prompt === "stage1 b"
      ? Promise.reject(new Error("nope"))
      : Promise.resolve(call.prompt.toUpperCase())
  );
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `
      return await pipeline(['a', 'b'],
        (item) => agent('stage1 ' + item),
        (prev, item, i) => agent('stage2 ' + prev + ' #' + i))
    `,
  });
  assertEquals(await finished(events, run.id), "done");
  const result = db.getWorkflow(run.id)!.result as (string | null)[];
  assertEquals(result, ["STAGE2 STAGE1 A #0", null]);
});

Deno.test("script error: run finishes status=error with the message", async () => {
  const { db, events, session, ctx, notes } = fixture();
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `throw new Error('script exploded')`,
  });
  assertEquals(await finished(events, run.id), "error");
  assert(db.getWorkflow(run.id)!.error!.includes("script exploded"));
  assert(notes[0].includes("[workflow error]"));
});

Deno.test("stop: kills the worker, aborts in-flight agents, marks rows", async () => {
  let abortSeen = false;
  const { db, events, session, ctx } = fixture((_call, signal) =>
    new Promise((_resolve, reject) => {
      signal.addEventListener("abort", () => {
        abortSeen = true;
        reject(new Error("aborted"));
      });
    })
  );
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `await agent('hangs forever'); return 1`,
  });
  // Wait for the agent journal row to appear (the call is in flight).
  for (let i = 0; i < 200 && db.listWorkflowAgents(run.id).length === 0; i++) {
    await new Promise((r) => setTimeout(r, 10));
  }
  const stopped = stopWorkflow(ctx, run.id);
  assertEquals(stopped.status, "stopped");
  assert(!isWorkflowLive(run.id));
  assert(abortSeen);
  assertEquals(db.listWorkflowAgents(run.id)[0].status, "stopped");
  assertEquals(await finished(events, run.id), "stopped");
});

Deno.test("pause parks new agent() calls; resume releases them", async () => {
  const ran: string[] = [];
  let releaseFirst = () => {};
  const firstGate = new Promise<void>((r) => (releaseFirst = r));
  const { db, events, session, ctx } = fixture(async (call) => {
    ran.push(call.prompt);
    if (call.prompt === "first") await firstGate; // hold the run mid-flight
    return "ok";
  });
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `
      await agent('first')
      await agent('second')
      return 'done'
    `,
  });
  // The first agent is in flight (held on its gate) — pause, then let it finish:
  // the second call must park on the pause gate instead of starting.
  for (let i = 0; i < 200 && ran.length === 0; i++) await new Promise((r) => setTimeout(r, 10));
  pauseWorkflow(ctx, run.id);
  assertEquals(db.getWorkflow(run.id)!.status, "paused");
  releaseFirst();
  await new Promise((r) => setTimeout(r, 150));
  assertEquals(ran, ["first"]);
  // A parked call must not journal: the UI would show a session-less "running"
  // agent while the run sits paused (live-test finding).
  assertEquals(db.listWorkflowAgents(run.id).length, 1);
  resumeWorkflow(ctx, run.id);
  assertEquals(await finished(events, run.id), "done");
  assertEquals(ran, ["first", "second"]);
});

// ---- rerun + journal replay ------------------------------------------------

Deno.test("rerun: unchanged calls replay from the journal; changed calls run live", async () => {
  const ran: string[] = [];
  const { db, events, session, ctx } = fixture((call) => {
    ran.push(call.prompt);
    return Promise.resolve("live:" + call.prompt);
  });
  const script = (verifyPrompt: string) =>
    META + `
      const found = await parallel(['a', 'b'].map((x) => () => agent('find ' + x)))
      const v = await agent('${verifyPrompt}')
      return { found, v }
    `;
  const first = await startWorkflow(ctx, { sessionId: session.id, script: script("verify v1") });
  assertEquals(await finished(events, first.id), "done");
  assertEquals(ran.length, 3);

  // Rerun with only the verify prompt changed: the two finds replay cached.
  ran.length = 0;
  const second = await rerunWorkflow(ctx, first.id, { script: script("verify v2") });
  assertEquals(second.resumeOf, first.id);
  assertEquals(await finished(events, second.id), "done");
  assertEquals(ran, ["verify v2"]); // only the changed call ran live
  const agents = db.listWorkflowAgents(second.id);
  assertEquals(agents.filter((a) => a.status === "cached").length, 2);
  const result = db.getWorkflow(second.id)!.result as { found: string[]; v: string };
  assertEquals(result.found.toSorted(), ["live:find a", "live:find b"]);
  assertEquals(result.v, "live:verify v2");
});

Deno.test("rerun: failed calls are not cached — they run again", async () => {
  let failFirst = true;
  const { db, events, session, ctx } = fixture((call) => {
    if (call.prompt === "flaky" && failFirst) {
      failFirst = false;
      return Promise.reject(new Error("flake"));
    }
    return Promise.resolve("ok:" + call.prompt);
  });
  const script = META + `
    const r = await parallel([() => agent('stable'), () => agent('flaky')])
    return r
  `;
  const first = await startWorkflow(ctx, { sessionId: session.id, script });
  assertEquals(await finished(events, first.id), "done");
  assertEquals(db.getWorkflow(first.id)!.result, ["ok:stable", null]);

  const second = await rerunWorkflow(ctx, first.id, {});
  assertEquals(await finished(events, second.id), "done");
  // The stable call replayed; the flaky one ran live and succeeded this time.
  assertEquals(db.getWorkflow(second.id)!.result, ["ok:stable", "ok:flaky"]);
  const agents = db.listWorkflowAgents(second.id);
  assertEquals(agents.find((a) => a.prompt === "stable")?.status, "cached");
  assertEquals(agents.find((a) => a.prompt === "flaky")?.status, "done");
});

Deno.test("rerun: refuses while the source run is live", async () => {
  const { events, session, ctx } = fixture(() => new Promise(() => {}));
  const run = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `await agent('hang'); return 1`,
  });
  await assertRejects(() => rerunWorkflow(ctx, run.id), Error, "still running");
  stopWorkflow(ctx, run.id);
  assertEquals(await finished(events, run.id), "stopped");
});

// ---- recovery + verbs ------------------------------------------------------

Deno.test("recoverOrphanedWorkflows: marks stale running rows, spares live ones", async () => {
  const { db, events, session, ctx } = fixture();
  // A stale row as a dead server would leave it.
  const stale = db.createWorkflow({
    id: crypto.randomUUID(),
    sessionId: session.id,
    name: "stale",
    description: "d",
    script: "s",
    phases: [],
    status: "running",
    currentPhase: null,
    result: null,
    error: null,
    args: null,
    resumeOf: null,
    createdAt: Date.now(),
    finishedAt: null,
  });
  const liveRun = await startWorkflow(ctx, {
    sessionId: session.id,
    script: META + `await agent('x'); return 1`,
  });
  assertEquals(recoverOrphanedWorkflows(db), 1);
  assertEquals(db.getWorkflow(stale.id)!.status, "orphaned");
  assert(db.getWorkflow(liveRun.id)!.status !== "orphaned");
  assertEquals(await finished(events, liveRun.id), "done");
});

Deno.test("workflowVerb: start/status/list/stop dispatch; unknown verb rejects", async () => {
  const { events, session, ctx } = fixture();
  const started = await workflowVerb(ctx, session.id, "start", {
    script: META + `return await agent('go')`,
  }) as { id: string; name: string; scriptFile: string };
  assertEquals(started.name, "test-flow");
  assert(started.scriptFile.endsWith(`${started.id}.js`));
  assertEquals(await finished(events, started.id), "done");
  const status = await workflowVerb(ctx, session.id, "status", { id: started.id }) as {
    status: string;
    agents: { done: number; total: number; running: number; failed: number };
    agentRows: unknown[];
  };
  assertEquals(status.status, "done");
  assertEquals(status.agents, { total: 1, done: 1, running: 0, failed: 0 });
  assertEquals(status.agentRows.length, 1);
  const list = await workflowVerb(ctx, session.id, "list", {}) as unknown[];
  assertEquals(list.length, 1);
  await assertRejects(() => workflowVerb(ctx, session.id, "explode", {}), Error, "unknown workflow verb");
});
