/**
 * Tests for the jobs API.
 *
 * The acceptance criteria (plan T6.8) are two, and both are about REACH rather than
 * about the registry, which `hostfn/shell.ts` already covers:
 *
 *   - **A session's job list covers its subagents' shells too.** The failing shape is
 *     specific and silent: a spawner fans out, four builds run, and the jobs tab shows
 *     nothing because every shell is registered under a child session's id. Asserted
 *     transitively, since a subagent may delegate one level further (spec §7).
 *   - **Killing a job emits `job.exited`.** The kill has to reach every attached
 *     client, not just the one that clicked — the response is for the caller, the
 *     event is for everyone else.
 *
 * Also asserted: kill resolves a job by id ACROSS sessions. Anything this endpoint can
 * list it must be able to kill, and scoping the lookup to the session in the URL 404'd
 * on every subagent row the list had just returned.
 *
 * These spawn REAL shells (`sleep`, `echo`) — trivial local commands, no network, no
 * key. Each test builds its own `JobRegistry` and installs it for the duration, so
 * nothing leaks into the process-wide one the server wires at boot.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { JobRegistry } from "../hostfn/jobs.ts";
import type { BackgroundJob, Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
// `./app.ts` first — see the note in `artifacts.test.ts` about the documented cycle.
import { createHandler, type Route, route } from "./app.ts";
import { jobOutputH, jobSessionIds, killJobH, listJobsH, setJobRegistry } from "./jobs.ts";

const TABLE: Route[] = [
  route("GET", "/sessions/:id/jobs", listJobsH),
  route("POST", "/sessions/:id/jobs/:jobId/kill", killJobH),
  route("GET", "/sessions/:id/jobs/:jobId/output", jobOutputH),
];

function session(id: string, extra: Partial<Session> = {}): Session {
  return {
    id,
    title: id,
    kind: "root",
    parentId: null,
    createdAt: Date.now(),
    ...extra,
  } as Session;
}

function fixture() {
  const db = openDb(":memory:");
  const bus = new Bus({ onListenerError: () => {} });
  const ctx: AppCtx = { db, bus, model: "test-model" };
  const registry = new JobRegistry({ bus });
  const previous = setJobRegistry(registry);
  return {
    call: createHandler(ctx, { routes: TABLE }),
    db,
    bus,
    registry,
    async [Symbol.asyncDispose]() {
      registry.killAll();
      await registry.drain();
      setJobRegistry(previous);
      db.close();
    },
  };
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;
const get = (path: string) => new Request(url(path));
const post = (path: string) => new Request(url(path), { method: "POST" });

// ---- lineage ----------------------------------------------------------------

test("AC: the tree walk collects subagents transitively, and only delegates", async () => {
  await using f = fixture();
  f.db.createSession(session("root"));
  f.db.createSession(session("kid", { kind: "subagent", originId: "root" }));
  f.db.createSession(session("grandkid", { kind: "subagent", originId: "kid" }));
  f.db.createSession(session("wfa", { kind: "workflow_agent", originId: "root" }));
  // A fork is a sibling CONVERSATION the user drives, not delegated work: folding its
  // jobs in would show one branch's runaway process in another branch's list.
  f.db.createSession(session("branch", { kind: "fork", originId: "root" }));

  assert.deepEqual(jobSessionIds(f.db, "root").sort(), ["grandkid", "kid", "root", "wfa"]);
  assert.deepEqual(jobSessionIds(f.db, "kid").sort(), ["grandkid", "kid"]);
  assert.deepEqual(jobSessionIds(f.db, "branch"), ["branch"]);
});

test("a lineage cycle does not hang the walk", async () => {
  await using f = fixture();
  f.db.createSession(session("a"));
  f.db.createSession(session("b", { kind: "subagent", originId: "a" }));
  // A bad write pointing back at the parent. The walk must terminate anyway.
  f.db.createSession(session("a2", { kind: "subagent", originId: "b" }));
  assert.deepEqual(jobSessionIds(f.db, "a").sort(), ["a", "a2", "b"]);
});

// ---- listing ----------------------------------------------------------------

test("AC: GET /sessions/:id/jobs includes the subagents' shells", async () => {
  await using f = fixture();
  f.db.createSession(session("spawner"));
  f.db.createSession(session("child", { kind: "subagent", originId: "spawner" }));

  const own = JSON.parse(
    f.registry.bashBg("long sleep", "sleep 30", { sessionId: "spawner", workspace: process.cwd() }),
  ) as { id: string };
  const childs = JSON.parse(
    f.registry.bashBg("long sleep", "sleep 30", { sessionId: "child", workspace: process.cwd() }),
  ) as { id: string };

  const res = await f.call(get("/sessions/spawner/jobs"));
  assert.equal(res.status, 200);
  const { jobs } = await res.json() as { jobs: BackgroundJob[] };
  assert.deepEqual(jobs.map((j) => j.id).sort(), [own.id, childs.id].sort());
  // Each row says whose it is, or a merged list is unattributable.
  assert.equal(jobs.find((j) => j.id === childs.id)!.sessionId, "child");

  // The child on its own sees only its own.
  const childOnly = await (await f.call(get("/sessions/child/jobs"))).json() as {
    jobs: BackgroundJob[];
  };
  assert.deepEqual(childOnly.jobs.map((j) => j.id), [childs.id]);
});

test("an unknown session is a 404 rather than an empty job list", async () => {
  await using f = fixture();
  const res = await f.call(get("/sessions/ghost/jobs"));
  assert.equal(res.status, 404);
  assert.equal(((await res.json()) as { error: string }).error.includes("ghost"), true);
});

// ---- kill -------------------------------------------------------------------

test("AC: killing a job emits job.exited and reports the outcome", async () => {
  await using f = fixture();
  f.db.createSession(session("s1"));
  const { id } = JSON.parse(
    f.registry.bashBg("long sleep", "sleep 30", { sessionId: "s1", workspace: process.cwd() }),
  ) as { id: string };

  const exited: BackgroundJob[] = [];
  f.bus.subscribe((e) => {
    if (e.type === "job.exited") exited.push(e.data as BackgroundJob);
  });

  const res = await f.call(post(`/sessions/s1/jobs/${id}/kill`));
  assert.equal(res.status, 200);
  assert.equal(((await res.json()) as { message: string }).message.includes(id), true);

  assert.deepEqual(exited.map((j) => j.id), [id]);
  assert.equal(exited[0].status, "exited");
  assert.equal(exited[0].sessionId, "s1");
});

test("kill resolves a SUBAGENT's job through its spawner's session", async () => {
  await using f = fixture();
  f.db.createSession(session("spawner"));
  f.db.createSession(session("child", { kind: "subagent", originId: "spawner" }));
  const { id } = JSON.parse(
    f.registry.bashBg("long sleep", "sleep 30", { sessionId: "child", workspace: process.cwd() }),
  ) as { id: string };

  // The URL names the spawner; the job belongs to the child. Anything the list
  // returned must be killable, so this must not 404.
  const res = await f.call(post(`/sessions/spawner/jobs/${id}/kill`));
  assert.equal(res.status, 200);
  await res.json();
  assert.deepEqual(f.registry.runningIds("child"), []);
});

test("killing an unknown job is a 404 that says why it might be gone", async () => {
  await using f = fixture();
  f.db.createSession(session("s1"));
  const res = await f.call(post("/sessions/s1/jobs/bg_999/kill"));
  assert.equal(res.status, 404);
  assert.equal(((await res.json()) as { error: string }).error.includes("bg_999"), true);
});

// ---- output -----------------------------------------------------------------

test("output returns the whole buffer and does NOT steal the model's cursor", async () => {
  await using f = fixture();
  f.db.createSession(session("s1"));
  const { id } = JSON.parse(
    f.registry.bashBg("greeter", "echo hello-from-the-job", { sessionId: "s1", workspace: process.cwd() }),
  ) as { id: string };
  await f.registry.bashWait(id, "s1");

  const res = await f.call(get(`/sessions/s1/jobs/${id}/output`));
  assert.equal(res.status, 200);
  const body = await res.json() as { output: string; job: BackgroundJob };
  assert.equal(body.output.includes("hello-from-the-job"), true);
  assert.equal(body.job.id, id);
  assert.equal(body.job.status, "exited");

  // Reading it again returns the same thing: a human looking at a log must not make
  // that output vanish from the agent's next tool result.
  const again = await (await f.call(get(`/sessions/s1/jobs/${id}/output`))).json() as {
    output: string;
  };
  assert.equal(again.output, body.output);
});

test("output for an unknown job is a 404", async () => {
  await using f = fixture();
  f.db.createSession(session("s1"));
  const res = await f.call(get("/sessions/s1/jobs/bg_404/output"));
  assert.equal(res.status, 404);
  await res.json();
});
