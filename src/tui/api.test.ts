/**
 * Tests for the TUI's HTTP client.
 *
 * These drive the REAL route table (`server/app.ts`) over an in-memory database, by
 * injecting a `fetch` that calls `createHandler(ctx)` directly. No socket is bound,
 * no port is claimed, nothing touches the network or `~/.bough` — and yet a method
 * whose URL, verb or response shape does not match the server it talks to fails here
 * rather than in the TUI. That is the whole value of this file: a typed client is only
 * worth having if something proves the types are the server's.
 *
 * `node:assert/strict` — jsr.io is unreachable in this environment.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import process from "node:process";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { createHandler } from "../server/app.ts";
import type { AppCtx } from "../types.ts";
import { ApiError, createApi, defaultBase, OfflineError } from "./api.ts";

/**
 * A ctx over an in-memory database, and a `fetch` that dispatches through the router.
 * `db.close()` is the whole teardown — nothing else was created.
 */
function fixture() {
  const db = openDb(":memory:");
  const ctx: AppCtx = { db, bus: new Bus() };
  const handler = createHandler(ctx, { onUnexpectedError: () => {} });
  const fetchFn = (input: string | URL | Request, init?: RequestInit) =>
    handler(new Request(input as string | URL, init));
  return {
    ctx,
    api: createApi({ base: "http://127.0.0.1:4321", fetchFn }),
    close: () => db.close(),
  };
}

test("the base tracks BOUGH_PORT, because the rewrite runs beside the live install", () => {
  const previous = process.env["BOUGH_PORT"];
  try {
    process.env["BOUGH_PORT"] = "4399";
    assert.equal(defaultBase(), "http://127.0.0.1:4399");
    delete process.env["BOUGH_PORT"];
    assert.equal(defaultBase(), "http://127.0.0.1:4321");
  } finally {
    if (previous === undefined) delete process.env["BOUGH_PORT"];
    else process.env["BOUGH_PORT"] = previous;
  }
});

test("session round trip: create, list, fetch, post, draft", async () => {
  const { api, close } = fixture();
  try {
    const created = await api.createSession({ title: "first" });
    assert.equal(created.kind, "root");

    const listed = await api.listSessions();
    assert.deepEqual(listed.map((s) => s.id), [created.id]);
    assert.equal(listed[0].busy, false, "`busy` is derived server-side, not stored");

    const posted = await api.postMessage(created.id, { text: "hello" });
    assert.equal(posted.queued, false);
    assert.equal(posted.message.role, "user");

    const snapshot = await api.getSession(created.id);
    assert.equal(snapshot.session.id, created.id);
    assert.deepEqual(snapshot.thread.map((m) => m.id), [posted.message.id]);
    // Usage rides along on purpose: this endpoint is the reconnect path, and a client
    // that needed a second fetch to redraw the status bar would flicker on every drop.
    assert.equal(typeof snapshot.usage.inputTokens, "number");
    assert.equal(typeof snapshot.usage.tree.costUsd, "number");

    const draft = await api.putDraft(created.id, "half a thought");
    assert.deepEqual(draft, { ok: true, draft: "half a thought" });
  } finally {
    close();
  }
});

test("the drill-in query is how collapsed sessions are reached at all", async () => {
  const { ctx, api, close } = fixture();
  try {
    const root = await api.createSession({ title: "root" });
    // A subagent cannot be created over HTTP (`server/sessions.ts`), so it is written
    // the way `agent()` writes it: with the lineage edge that makes it reachable.
    ctx.db.createSession({
      id: "sub-1",
      title: "worker",
      kind: "subagent",
      createdAt: Date.now(),
      parentId: null,
      originId: root.id,
    });

    assert.deepEqual((await api.listSessions()).map((s) => s.id), [root.id]);
    assert.deepEqual((await api.listSessions(root.id)).map((s) => s.id), ["sub-1"]);
  } finally {
    close();
  }
});

test("a server error arrives as its own sentence, not as a status code", async () => {
  const { api, close } = fixture();
  try {
    await assert.rejects(
      () => api.getSession("nope"),
      (error: unknown) => {
        assert.ok(error instanceof ApiError);
        assert.equal(error.status, 404);
        // Error text is a product surface (spec §6): the message names the id.
        assert.match(error.message, /session nope not found/);
        return true;
      },
    );
  } finally {
    close();
  }
});

test("a dead server says so in one sentence, with the command that fixes it", async () => {
  const api = createApi({
    base: "http://127.0.0.1:4321",
    fetchFn: () => Promise.reject(new TypeError("error sending request: Connection refused")),
  });
  await assert.rejects(
    () => api.listSessions(),
    (error: unknown) => {
      assert.ok(error instanceof OfflineError);
      assert.match(error.message, /unreachable/);
      assert.match(error.message, /http:\/\/127\.0\.0\.1:4321/);
      // The REMEDY must come before the address. This line is rendered into a
      // one-row notice that truncates, and with the command last an 80-column
      // terminal clipped it to "bough st…" — the only part that mattered.
      assert.ok(
        error.message.indexOf("bough start") < error.message.indexOf("127.0.0.1"),
        `the command must precede the address: ${error.message}`,
      );
      assert.ok(error.message.length <= 80, `too long for one row: ${error.message}`);
      return true;
    },
  );
});

test("changes: a non-repository workspace is an answer, not an error", async () => {
  const { api, close } = fixture();
  try {
    const s = await api.createSession({});
    const changes = await api.getChanges(s.id);
    assert.equal(changes.available, false);
    assert.ok(changes.reason, "the rail says why rather than showing an empty diff (spec §13)");
  } finally {
    close();
  }
});

test("jobs, artifacts and questions answer for a session with nothing in them", async () => {
  const { api, close } = fixture();
  try {
    const s = await api.createSession({});
    assert.deepEqual((await api.listJobs(s.id)).jobs, []);
    assert.deepEqual((await api.listArtifacts(s.id)).artifacts, []);
    assert.deepEqual(await api.listQuestions(), []);
    assert.deepEqual((await api.listComments(s.id)).comments, []);
  } finally {
    close();
  }
});

test("workflows: the list is empty and a missing run 404s with its id", async () => {
  const { api, close } = fixture();
  try {
    assert.deepEqual((await api.listWorkflows()).workflows, []);
    await assert.rejects(
      () => api.getWorkflow("run-404"),
      (error: unknown) => {
        assert.ok(error instanceof ApiError);
        assert.match(error.message, /run-404/);
        return true;
      },
    );
    // Spec §8: replay is always reported — including that there is no such run, which
    // is a different fact from "replayed nothing".
    await assert.rejects(() => api.workflowReplay("run-404"), ApiError);
  } finally {
    close();
  }
});

test("search answers over an empty index rather than failing", async () => {
  const { api, close } = fixture();
  try {
    const result = await api.search("anything");
    assert.equal(result.count, 0);
    assert.deepEqual(result.hits, []);
    await assert.rejects(() => api.search(""), ApiError);
  } finally {
    close();
  }
});

test("schedules round trip through the client's verbs", async () => {
  const { api, close } = fixture();
  try {
    const created = await api.createSchedule({
      title: "nightly",
      prompt: "check the build",
      spec: "daily@09:00",
    });
    assert.equal(created.enabled, true);
    assert.deepEqual((await api.listSchedules()).map((s) => s.id), [created.id]);

    const patched = await api.patchSchedule(created.id, { enabled: false });
    assert.equal(patched.enabled, false);
    assert.deepEqual(await api.deleteSchedule(created.id), { ok: true, removed: created.id });
    assert.deepEqual(await api.listSchedules(), []);
  } finally {
    close();
  }
});

test("URLs are built in one place, and segments are encoded", () => {
  const api = createApi({ base: "http://127.0.0.1:4321" });
  assert.equal(api.eventsUrl(), "http://127.0.0.1:4321/events");
  assert.equal(api.eventsUrl("a b"), "http://127.0.0.1:4321/events?sessionId=a+b");
  // Path separators inside an artifact name survive; the segments around them do not
  // get to smuggle one in.
  assert.equal(
    api.artifactUrl("s 1", "assets/app js.html"),
    "http://127.0.0.1:4321/artifacts/s%201/assets/app%20js.html",
  );
});
