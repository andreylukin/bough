import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { type AppCtx, createHandler } from "./app.ts";
import type { WorkflowRun } from "../db/db.ts";

// Keep script mirrors out of the real ~/.bough.
Deno.env.set("BOUGH_WORKFLOW_DIR", Deno.makeTempDirSync({ prefix: "bough-wf-routes" }));

function ctx(): AppCtx {
  return {
    db: new Db(":memory:"),
    bus: new Bus(),
    envDir: Deno.makeTempDirSync({ prefix: "app-env-" }),
  };
}

const req = (method: string, path: string, body?: unknown) =>
  new Request("http://x" + path, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });

const META = `export const meta = {
  name: 'route-flow',
  description: 'route test workflow',
  phases: [{ title: 'Only' }],
}\n`;

function seedSession(c: AppCtx, id = "S"): void {
  c.db.createSession({ id, parentId: null, title: "s", kind: "root", createdAt: 1 });
}

/** Poll the db until the run leaves running/paused (worker completion is async). */
async function settled(c: AppCtx, id: string): Promise<WorkflowRun> {
  for (let i = 0; i < 300; i++) {
    const run = c.db.getWorkflow(id)!;
    if (run.status !== "running" && run.status !== "paused") return run;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("workflow did not settle");
}

Deno.test("POST /workflows starts a run; GET lists and fetches it", async () => {
  const c = ctx();
  const h = createHandler(c);
  seedSession(c);

  const created = await h(req("POST", "/workflows", {
    sessionId: "S",
    script: META + "log('hi'); return {answer: 42}",
  }));
  assertEquals(created.status, 201);
  const run = await created.json() as WorkflowRun;
  assertEquals(run.name, "route-flow");
  const done = await settled(c, run.id);
  assertEquals(done.status, "done");
  assertEquals(done.result, { answer: 42 });

  const list = await (await h(req("GET", "/workflows?session=S"))).json() as {
    workflows: { id: string; status: string; scriptFile: string }[];
  };
  assertEquals(list.workflows.length, 1);
  assertEquals(list.workflows[0].id, run.id);
  assert(list.workflows[0].scriptFile.endsWith(`${run.id}.js`));
  // The mirror file exists with the script text.
  assert((await Deno.readTextFile(list.workflows[0].scriptFile)).includes("route-flow"));

  const got = await (await h(req("GET", `/workflows/${run.id}`))).json() as {
    workflow: WorkflowRun;
    agents: unknown[];
  };
  assertEquals(got.workflow.status, "done");
  assertEquals(got.agents, []);

  assertEquals((await h(req("GET", "/workflows/nope"))).status, 404);
  c.db.close();
});

Deno.test("POST /workflows validates: bad body 400, bad meta 400, bad session 404", async () => {
  const c = ctx();
  const h = createHandler(c);
  seedSession(c);
  assertEquals((await h(req("POST", "/workflows", { script: "x" }))).status, 400);
  assertEquals(
    (await h(req("POST", "/workflows", { sessionId: "S", script: "return 1" }))).status,
    400, // no meta
  );
  assertEquals(
    (await h(req("POST", "/workflows", { sessionId: "nope", script: META + "return 1" }))).status,
    404,
  );
  c.db.close();
});

Deno.test("stop/pause/resume: 409 when not running; rerun replays and 404s on unknown", async () => {
  const c = ctx();
  const h = createHandler(c);
  seedSession(c);

  const run = await (await h(req("POST", "/workflows", {
    sessionId: "S",
    script: META + "return 'v1'",
  }))).json() as WorkflowRun;
  await settled(c, run.id);

  // Not running any more: pause/resume 409; stop is idempotent on a finished run.
  assertEquals((await h(req("POST", `/workflows/${run.id}/pause`))).status, 409);
  assertEquals((await h(req("POST", `/workflows/${run.id}/resume`))).status, 409);
  assertEquals((await h(req("POST", `/workflows/${run.id}/stop`))).status, 200);
  assertEquals(c.db.getWorkflow(run.id)!.status, "done"); // stop didn't clobber the outcome

  // Rerun with an edited script — a fresh run linked back to the source.
  const rerun = await h(req("POST", `/workflows/${run.id}/rerun`, {
    script: META + "return 'v2'",
  }));
  assertEquals(rerun.status, 201);
  const second = await rerun.json() as WorkflowRun;
  assertEquals(second.resumeOf, run.id);
  assertEquals((await settled(c, second.id)).result, "v2");

  assertEquals((await h(req("POST", "/workflows/nope/rerun", {}))).status, 404);
  assertEquals((await h(req("POST", "/workflows/nope/stop"))).status, 404);
  c.db.close();
});

Deno.test("rerun with no body reuses the (possibly edited) script mirror", async () => {
  const c = ctx();
  const h = createHandler(c);
  seedSession(c);
  const run = await (await h(req("POST", "/workflows", {
    sessionId: "S",
    script: META + "return 'original'",
  }))).json() as WorkflowRun;
  await settled(c, run.id);

  // Edit the mirror file, then rerun without a body — the edit takes effect.
  const file = (await (await h(req("GET", `/workflows/${run.id}`))).json() as {
    scriptFile: string;
  }).scriptFile;
  await Deno.writeTextFile(file, META + "return 'edited on disk'");
  const second = await (await h(req("POST", `/workflows/${run.id}/rerun`, {}))).json() as
    WorkflowRun;
  assertEquals((await settled(c, second.id)).result, "edited on disk");
  c.db.close();
});
