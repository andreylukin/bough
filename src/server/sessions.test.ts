/**
 * Tests for session CRUD, thread assembly and message intake.
 *
 * The load-bearing one is derived visibility (spec §4): a `subagent` session is
 * absent from `GET /sessions` and present under `?originId=`. It is asserted in
 * both directions on purpose — "hidden" and "reachable" are one invariant, and a
 * filter that hid a session from every listing would pass a test that only checked
 * the first half while making delegated work unopenable.
 *
 * Everything runs against `createHandler(ctx)` over an in-memory database with no
 * socket bound and nothing on the network (plan §7). The one filesystem touch is
 * `Deno.makeTempDir` for the workspace-existence check — never `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * not reachable from this environment, and a test that cannot run offline does not
 * belong in `deno task test`.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { createHandler, type Route, route } from "./app.ts";
import {
  createSession,
  getSession,
  isCollapsed,
  listSessions,
  normalizeWorkspace,
  patchSession,
  postMessage,
  putDraft,
  type SessionListItem,
  type TurnStarter,
  type WithTurnStarter,
} from "./sessions.ts";

// ---- fixtures ---------------------------------------------------------------

/** The five entries this task appends, isolated from whatever else the table holds. */
const TABLE: Route[] = [
  route("GET", "/sessions", listSessions),
  route("POST", "/sessions", createSession),
  route("GET", "/sessions/:id", getSession),
  route("POST", "/sessions/:id/messages", postMessage),
  route("PUT", "/sessions/:id/draft", putDraft),
  route("PATCH", "/sessions/:id", patchSession),
];

interface Fixture {
  call: (req: Request) => Promise<Response>;
  ctx: AppCtx & WithTurnStarter;
  db: SqliteDb;
  events: BoughEvent[];
  started: { session: Session; message: Message }[];
}

/**
 * A fabricated ctx over an in-memory database, a real bus with a collector
 * subscribed, and a recording turn starter. The starter is the seam M2 fills; here
 * it only records, so a test can assert that a post into a busy session did NOT
 * start one.
 */
function fixture(opts: { startTurn?: TurnStarter } = {}): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const started: { session: Session; message: Message }[] = [];
  const ctx: AppCtx & WithTurnStarter = {
    db,
    bus,
    model: "test-model",
    startTurn: opts.startTurn ?? ((_ctx, session, message) => {
      started.push({ session, message });
    }),
  };
  return { call: createHandler(ctx, { routes: TABLE }), ctx, db, events, started };
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;
const get = (path: string) => new Request(url(path));
const post = (path: string, body?: unknown) =>
  new Request(url(path), {
    method: "POST",
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
const put = (path: string, body: unknown) =>
  new Request(url(path), { method: "PUT", body: JSON.stringify(body) });

/** Create a session over HTTP and return the stored row. */
async function newSession(
  f: Fixture,
  body: Record<string, unknown> = {},
): Promise<Session> {
  const res = await f.call(post("/sessions", body));
  assert.equal(res.status, 201, await res.clone().text());
  return await res.json() as Session;
}

/** Insert a delegated session directly — `agent()`/`spawn()` (M4) own this path. */
function seedDelegated(
  db: SqliteDb,
  kind: "subagent" | "workflow_agent",
  origin: Session,
  title: string,
): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    title,
    kind,
    createdAt: Date.now(),
    // A subagent's thread is task-only: no parent, so no inherited context (spec §7).
    parentId: null,
    originId: origin.id,
  });
}

// ---- derived visibility -----------------------------------------------------

Deno.test("a subagent session is absent from GET /sessions and present under its origin", async () => {
  const f = fixture();
  try {
    const root = await newSession(f, { title: "root" });
    const child = seedDelegated(f.db, "subagent", root, "review handlers");

    const top = await (await f.call(get("/sessions"))).json() as SessionListItem[];
    assert.deepEqual(top.map((s) => s.id), [root.id]);
    assert.equal(top.some((s) => s.id === child.id), false);

    const drill = await (await f.call(get(`/sessions?originId=${root.id}`)))
      .json() as SessionListItem[];
    assert.deepEqual(drill.map((s) => s.id), [child.id]);
    assert.equal(drill[0].title, "review handlers");
    assert.equal(drill[0].kind, "subagent");
  } finally {
    f.db.close();
  }
});

Deno.test("a workflow_agent session collapses the same way a subagent does", async () => {
  const f = fixture();
  try {
    const root = await newSession(f);
    const agent = seedDelegated(f.db, "workflow_agent", root, "verify: title");
    const top = await (await f.call(get("/sessions"))).json() as SessionListItem[];
    assert.equal(top.some((s) => s.id === agent.id), false);
    const drill = await (await f.call(get(`/sessions?originId=${root.id}`))).json() as Session[];
    assert.deepEqual(drill.map((s) => s.id), [agent.id]);
  } finally {
    f.db.close();
  }
});

Deno.test("roots, forks and compactions are always listed", async () => {
  const f = fixture();
  try {
    const root = await newSession(f, { title: "root" });
    const fork = await newSession(f, { title: "fork", parentId: root.id });
    const compaction = await newSession(f, {
      title: "compaction",
      parentId: root.id,
      kind: "compaction",
    });
    assert.equal(fork.kind, "fork"); // derived from parentId
    const top = await (await f.call(get("/sessions"))).json() as Session[];
    assert.deepEqual(
      new Set(top.map((s) => s.id)),
      new Set([root.id, fork.id, compaction.id]),
    );
  } finally {
    f.db.close();
  }
});

Deno.test("the drill-in returns every branch of an origin, collapsed kinds and forks alike", async () => {
  const f = fixture();
  try {
    const root = await newSession(f);
    const sub = seedDelegated(f.db, "subagent", root, "a");
    // A fork of the same session shares the origin edge; splitting the two would
    // make the tree view ask twice for one node's children.
    const branch = f.db.createSession({
      id: crypto.randomUUID(),
      title: "branch",
      kind: "fork",
      createdAt: Date.now(),
      parentId: root.id,
      originId: root.id,
    });
    const drill = await (await f.call(get(`/sessions?originId=${root.id}`))).json() as Session[];
    assert.deepEqual(new Set(drill.map((s) => s.id)), new Set([sub.id, branch.id]));
  } finally {
    f.db.close();
  }
});

Deno.test("an unknown originId is a 404, not an empty list", async () => {
  const f = fixture();
  try {
    const res = await f.call(get("/sessions?originId=nope"));
    assert.equal(res.status, 404);
    assert.match((await res.json()).error, /session nope not found/);
  } finally {
    f.db.close();
  }
});

Deno.test("POST /sessions refuses to create a collapsed kind that no listing could reach", async () => {
  const f = fixture();
  try {
    for (const kind of ["subagent", "workflow_agent"]) {
      const res = await f.call(post("/sessions", { kind }));
      assert.equal(res.status, 400);
      assert.match((await res.json()).error, /agent\(\)\/spawn\(\)/);
    }
    // And nothing was persisted by the refused calls.
    assert.deepEqual(f.db.listSessions(), []);
  } finally {
    f.db.close();
  }
});

Deno.test("isCollapsed is the whole visibility rule — no stored flag exists", () => {
  const base = { id: "x", title: "t", createdAt: 0, parentId: null };
  assert.equal(isCollapsed({ ...base, kind: "subagent" }), true);
  assert.equal(isCollapsed({ ...base, kind: "workflow_agent" }), true);
  assert.equal(isCollapsed({ ...base, kind: "root" }), false);
  assert.equal(isCollapsed({ ...base, kind: "fork" }), false);
  assert.equal(isCollapsed({ ...base, kind: "compaction" }), false);
});

// ---- creation ---------------------------------------------------------------

Deno.test("POST /sessions announces the session it stored, byte for byte", async () => {
  const f = fixture();
  try {
    const session = await newSession(f, { title: "hello" });
    assert.equal(session.kind, "root");
    assert.deepEqual(f.db.getSession(session.id), session);
    const created = f.events.filter((e) => e.type === "session.created");
    assert.equal(created.length, 1);
    assert.deepEqual(created[0].data, session);
    assert.equal(created[0].sessionId, session.id);
  } finally {
    f.db.close();
  }
});

Deno.test("model and effort pins are stored before the announce, not after", async () => {
  const f = fixture();
  try {
    const session = await newSession(f, { model: "openai:gpt-5", effort: "high" });
    assert.equal(session.model, "openai:gpt-5");
    assert.equal(session.effort, "high");
    const created = f.events.find((e) => e.type === "session.created");
    // The event carried the pins too: a client that renders from the event and one
    // that renders from the response must not disagree.
    assert.deepEqual(created?.data, session);
  } finally {
    f.db.close();
  }
});

Deno.test("an unknown parent is a 400 naming it, and nothing is created", async () => {
  const f = fixture();
  try {
    const res = await f.call(post("/sessions", { parentId: "ghost" }));
    assert.equal(res.status, 400);
    assert.match((await res.json()).error, /parent ghost not found/);
    assert.deepEqual(f.db.listSessions(), []);
    assert.deepEqual(f.events, []);
  } finally {
    f.db.close();
  }
});

Deno.test("a workspace that exists is recorded, and originDir mirrors it", async () => {
  const f = fixture();
  const dir = await Deno.makeTempDir();
  try {
    const session = await newSession(f, { workspace: dir });
    assert.equal(session.workspace, dir);
    assert.equal(session.originDir, dir);
  } finally {
    f.db.close();
    await Deno.remove(dir);
  }
});

Deno.test("a workspace that does not exist is rejected at creation, not one turn later", async () => {
  const f = fixture();
  try {
    const res = await f.call(post("/sessions", { workspace: "/no/such/checkout" }));
    assert.equal(res.status, 400);
    assert.match((await res.json()).error, /workspace does not exist/);
    assert.deepEqual(f.db.listSessions(), []);
  } finally {
    f.db.close();
  }
});

Deno.test("a workspace that is a file, not a directory, says so", async () => {
  const f = fixture();
  const dir = await Deno.makeTempDir();
  const file = `${dir}/not-a-checkout`;
  await Deno.writeTextFile(file, "");
  try {
    const res = await f.call(post("/sessions", { workspace: file }));
    assert.equal(res.status, 400);
    assert.match((await res.json()).error, /not a directory/);
  } finally {
    f.db.close();
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("normalizeWorkspace expands ~ and absolutizes, with home as a parameter", () => {
  assert.equal(normalizeWorkspace("~", "/home/dev"), "/home/dev");
  assert.equal(normalizeWorkspace("~/src/bough", "/home/dev"), "/home/dev/src/bough");
  assert.equal(normalizeWorkspace("  /srv/x  ", "/home/dev"), "/srv/x");
  // `~name` is a login, not this user's home — it must not expand.
  assert.notEqual(normalizeWorkspace("~other/x", "/home/dev"), "/home/dev/other/x");
});

// ---- the session view -------------------------------------------------------

Deno.test("GET /sessions/:id returns {session, thread} with ancestors before own messages", async () => {
  const f = fixture();
  try {
    const root = await newSession(f, { title: "root" });
    await f.call(post(`/sessions/${root.id}/messages`, { text: "one" }));
    const child = await newSession(f, { parentId: root.id });
    await f.call(post(`/sessions/${child.id}/messages`, { text: "two" }));

    const body = await (await f.call(get(`/sessions/${child.id}`))).json() as {
      session: Session;
      thread: Message[];
      usage: { costUsd: number; tree: { costUsd: number } };
    };
    assert.equal(body.session.id, child.id);
    // Thread inheritance: the ancestor's message is present without being copied.
    assert.deepEqual(body.thread.map((m) => textOf(m)), ["one", "two"]);
    assert.deepEqual(body.thread.map((m) => m.sessionId), [root.id, child.id]);
    assert.equal(body.usage.costUsd, 0);
    assert.equal(body.usage.tree.costUsd, 0);
  } finally {
    f.db.close();
  }
});

Deno.test("GET /sessions/:id on an unknown id is a 404 naming it", async () => {
  const f = fixture();
  try {
    const res = await f.call(get("/sessions/ghost"));
    assert.equal(res.status, 404);
    assert.deepEqual(await res.json(), { error: "session ghost not found" });
  } finally {
    f.db.close();
  }
});

// ---- messages ---------------------------------------------------------------

function textOf(m: Message): string {
  return m.parts.filter((p) => p.type === "text").map((p) => p.text).join("");
}

Deno.test("POST messages persists, announces, and hands off to the turn runner", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    const res = await f.call(post(`/sessions/${session.id}/messages`, { text: "  ship it  " }));
    assert.equal(res.status, 202);
    const { message, queued } = await res.json() as { message: Message; queued: boolean };
    assert.equal(queued, false);
    assert.equal(message.role, "user");
    assert.equal(message.pending, false);
    assert.equal(textOf(message), "ship it");
    assert.deepEqual(f.db.messagesFor(session.id), [message]);

    const started = f.events.filter((e) => e.type === "message.started");
    assert.equal(started.length, 1);
    assert.deepEqual(started[0].data, message);

    // The turn runner receives the session and the exact stored message.
    assert.equal(f.started.length, 1);
    assert.deepEqual(f.started[0].message, message);
    assert.equal(f.started[0].session.id, session.id);
  } finally {
    f.db.close();
  }
});

Deno.test("a message posted while a turn runs is persisted and queued, not started", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    // A running turn is what makes the session busy (spec §5, one turn per session).
    const first = f.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: session.id,
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: Date.now(),
    });
    f.db.createTurn({
      id: crypto.randomUUID(),
      sessionId: session.id,
      messageId: first.id,
      status: "running",
      step: "model",
      createdAt: Date.now(),
      updatedAt: Date.now(),
    });

    const res = await f.call(post(`/sessions/${session.id}/messages`, { text: "also this" }));
    assert.equal(res.status, 202);
    const { message, queued } = await res.json() as { message: Message; queued: boolean };
    assert.equal(queued, true);
    // Persisted and announced — it is never dropped; only the START is deferred.
    assert.equal(f.db.getMessage(message.id)?.id, message.id);
    assert.equal(f.events.some((e) => e.type === "message.started"), true);
    assert.deepEqual(f.started, []);
  } finally {
    f.db.close();
  }
});

Deno.test("image attachments become parts carrying a path, never bytes", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    const image = {
      path: "/home/dev/.bough/attachments/a.png",
      mediaType: "image/png",
      name: "a.png",
      size: 1234,
    };
    const res = await f.call(
      post(`/sessions/${session.id}/messages`, { text: "look", images: [image] }),
    );
    const { message } = await res.json() as { message: Message };
    assert.deepEqual(message.parts, [
      { type: "text", text: "look" },
      { type: "image", ...image },
    ]);
  } finally {
    f.db.close();
  }
});

Deno.test("an image-only message is allowed; an entirely empty one is a 400", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    const image = { path: "/a.png", mediaType: "image/png", name: "a.png", size: 1 };
    const ok = await f.call(
      post(`/sessions/${session.id}/messages`, { text: "", images: [image] }),
    );
    assert.equal(ok.status, 202);
    const { message } = await ok.json() as { message: Message };
    assert.deepEqual(message.parts.map((p) => p.type), ["image"]);

    const empty = await f.call(post(`/sessions/${session.id}/messages`, { text: "   " }));
    assert.equal(empty.status, 400);
    assert.match((await empty.json()).error, /empty message/);
    assert.equal(f.db.messagesFor(session.id).length, 1);
  } finally {
    f.db.close();
  }
});

Deno.test("posting into an unknown session is a 404 and starts nothing", async () => {
  const f = fixture();
  try {
    const res = await f.call(post("/sessions/ghost/messages", { text: "hi" }));
    assert.equal(res.status, 404);
    assert.deepEqual(f.started, []);
  } finally {
    f.db.close();
  }
});

Deno.test("the first post consumes the handoff draft and announces the clear", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    f.db.setSessionDraft(session.id, "a prefilled opening prompt");
    await f.call(post(`/sessions/${session.id}/messages`, { text: "my own words" }));
    assert.equal(f.db.getSession(session.id)?.draft ?? null, null);
    const updated = f.events.filter((e) => e.type === "session.updated");
    assert.equal(updated.length, 1);
    assert.equal((updated[0].data as Session).draft ?? null, null);
  } finally {
    f.db.close();
  }
});

Deno.test("a turn starter that throws is contained — the post still answers 202", async () => {
  const errors: unknown[] = [];
  const original = console.error;
  console.error = (...args: unknown[]) => errors.push(args);
  const f = fixture({
    startTurn: () => {
      throw new Error("no llm configured");
    },
  });
  try {
    const session = await newSession(f);
    const res = await f.call(post(`/sessions/${session.id}/messages`, { text: "go" }));
    assert.equal(res.status, 202);
    assert.equal(errors.length, 1);
    // The message survived the failed start: the transcript is the source of truth.
    assert.equal(f.db.messagesFor(session.id).length, 1);
  } finally {
    console.error = original;
    f.db.close();
  }
});

Deno.test("a rejecting turn starter does not reject the request", async () => {
  const errors: unknown[] = [];
  const original = console.error;
  console.error = (...args: unknown[]) => errors.push(args);
  const f = fixture({ startTurn: () => Promise.reject(new Error("provider down")) });
  try {
    const session = await newSession(f);
    assert.equal(
      (await f.call(post(`/sessions/${session.id}/messages`, { text: "go" }))).status,
      202,
    );
    // The rejection is handled on its own tick, not awaited by the handler.
    await new Promise((r) => setTimeout(r, 0));
    assert.equal(errors.length, 1);
  } finally {
    console.error = original;
    f.db.close();
  }
});

Deno.test("no turn starter wired: the message still lands (M1 has no runner yet)", async () => {
  const db = openDb(":memory:");
  const ctx: AppCtx = { db, bus: new Bus() };
  const call = createHandler(ctx, { routes: TABLE });
  try {
    const created = await call(post("/sessions", {}));
    const session = await created.json() as Session;
    const res = await call(post(`/sessions/${session.id}/messages`, { text: "hi" }));
    assert.equal(res.status, 202);
    assert.equal(db.messagesFor(session.id).length, 1);
  } finally {
    db.close();
  }
});

Deno.test("a posted message is keyword-searchable immediately", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    await f.call(post(`/sessions/${session.id}/messages`, { text: "reticulating splines" }));
    const hits = f.db.searchMessages("splines");
    assert.equal(hits.length, 1);
    assert.equal(hits[0].sessionId, session.id);
  } finally {
    f.db.close();
  }
});

// ---- listing decorations ----------------------------------------------------

Deno.test("listing carries busy and lastTurnStatus derived from turns, not columns", async () => {
  const f = fixture();
  try {
    const idle = await newSession(f, { title: "idle" });
    const busy = await newSession(f, { title: "busy" });
    const m = f.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: busy.id,
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: Date.now(),
    });
    f.db.createTurn({
      id: crypto.randomUUID(),
      sessionId: busy.id,
      messageId: m.id,
      status: "running",
      step: "model",
      createdAt: Date.now(),
      updatedAt: Date.now(),
    });

    const rows = await (await f.call(get("/sessions"))).json() as SessionListItem[];
    const byId = new Map(rows.map((r) => [r.id, r]));
    assert.equal(byId.get(busy.id)?.busy, true);
    assert.equal(byId.get(busy.id)?.lastTurnStatus, "running");
    assert.equal(byId.get(idle.id)?.busy, false);
    assert.equal(Object.hasOwn(byId.get(idle.id)!, "lastTurnStatus"), false);
    // Cost is omitted rather than zero, so an untouched row stays small.
    assert.equal(Object.hasOwn(byId.get(idle.id)!, "costUsd"), false);
  } finally {
    f.db.close();
  }
});

// ---- draft ------------------------------------------------------------------

Deno.test("PUT draft stores the text and, deliberately, emits no event", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    const before = f.events.length;
    const res = await f.call(put(`/sessions/${session.id}/draft`, { draft: "half-typed" }));
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), { ok: true, draft: "half-typed" });
    assert.equal(f.db.getSession(session.id)?.draft, "half-typed");
    // The writer is the client switching away; echoing session.updated back at it
    // would race the prefill it is about to render.
    assert.equal(f.events.length, before);
  } finally {
    f.db.close();
  }
});

Deno.test("PUT draft with null clears it", async () => {
  const f = fixture();
  try {
    const session = await newSession(f);
    await f.call(put(`/sessions/${session.id}/draft`, { draft: "x" }));
    await f.call(put(`/sessions/${session.id}/draft`, { draft: null }));
    assert.equal(f.db.getSession(session.id)?.draft ?? null, null);
  } finally {
    f.db.close();
  }
});

Deno.test("PUT draft rejects a missing session and a wrong-shaped body", async () => {
  const f = fixture();
  try {
    assert.equal((await f.call(put("/sessions/ghost/draft", { draft: "x" }))).status, 404);
    const session = await newSession(f);
    const bad = await f.call(put(`/sessions/${session.id}/draft`, { draft: 42 }));
    assert.equal(bad.status, 400);
    assert.match((await bad.json()).error, /^invalid body: /);
  } finally {
    f.db.close();
  }
});

const patch = (path: string, body: unknown) =>
  new Request(url(path), { method: "PATCH", body: JSON.stringify(body) });

Deno.test("PATCH /sessions/:id pins a model, and an explicit null clears it", async () => {
  const f = fixture();
  const session = await newSession(f, { title: "pin me" });

  const pinned = await (await f.call(patch(`/sessions/${session.id}`, {
    model: "openai:gpt-x",
  }))).json();
  assert.equal(pinned.model, "openai:gpt-x");

  // An ABSENT field leaves the pin alone. This is the case a naive implementation gets
  // wrong by collapsing undefined into null and silently unpinning.
  const untouched = await (await f.call(patch(`/sessions/${session.id}`, {
    effort: "high",
  }))).json();
  assert.equal(untouched.model, "openai:gpt-x");
  assert.equal(untouched.effort, "high");

  // An EXPLICIT null clears it — the session falls back to the global default.
  const cleared = await (await f.call(patch(`/sessions/${session.id}`, {
    model: null,
  }))).json();
  assert.equal(cleared.model ?? null, null);
  assert.equal(cleared.effort, "high", "clearing one override must not clear the other");

  f.db.close();
});

Deno.test("PATCH /sessions/:id rejects an unknown effort and a missing session", async () => {
  const f = fixture();
  const session = await newSession(f, {});
  assert.equal((await f.call(patch(`/sessions/${session.id}`, { effort: "turbo" }))).status, 400);
  assert.equal((await f.call(patch("/sessions/nope", { model: "x" }))).status, 404);
  f.db.close();
});
