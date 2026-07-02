import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { createHandler, type AppCtx } from "./app.ts";
import type { Message, Session } from "../schema/parts.ts";

function ctx(): AppCtx {
  const bus = new Bus();
  return { db: new Db(":memory:"), bus };
}

const req = (method: string, path: string, body?: unknown) =>
  new Request("http://x" + path, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });

Deno.test("GET /config lists models; PATCH /config switches the active model", async () => {
  const c = ctx();
  const h = createHandler(c);

  const cfg = await (await h(req("GET", "/config"))).json() as { model: string; models: { id: string }[] };
  assertEquals(cfg.model, "claude-opus-4-8");
  assert(cfg.models.some((m) => m.id === "claude-opus-4-8"));

  const patched = await h(req("PATCH", "/config", { model: "claude-haiku-4-5" }));
  assertEquals((await patched.json() as { model: string }).model, "claude-haiku-4-5");
  assertEquals((await (await h(req("GET", "/config"))).json() as { model: string }).model, "claude-haiku-4-5");

  assertEquals((await h(req("PATCH", "/config", { model: "" }))).status, 400);
  // Restore so later tests see the default.
  await h(req("PATCH", "/config", { model: "claude-opus-4-8" }));
  c.db.close();
});

Deno.test("GET /sessions/:id includes token usage (zero before any turn)", async () => {
  const c = ctx();
  const h = createHandler(c);
  const s = await (await h(req("POST", "/sessions", { title: "u" }))).json() as Session;
  const got = await (await h(req("GET", `/sessions/${s.id}`))).json() as { usage: { contextTokens: number; outputTokens: number } };
  assertEquals(got.usage, { contextTokens: 0, outputTokens: 0 });
  c.db.close();
});

Deno.test("POST /sessions creates and GET /sessions lists it", async () => {
  const c = ctx();
  const h = createHandler(c);

  const created = await (await h(req("POST", "/sessions", { title: "hello" }))).json() as Session;
  assertEquals(created.title, "hello");
  assertEquals(created.kind, "root");
  assertEquals(created.parentId, null);

  const list = await (await h(req("GET", "/sessions"))).json() as Session[];
  assertEquals(list.map((s) => s.id), [created.id]);
  c.db.close();
});

Deno.test("POST /sessions with parentId defaults kind=fork; unknown parent → 400", async () => {
  const c = ctx();
  const h = createHandler(c);
  const root = await (await h(req("POST", "/sessions", { title: "r" }))).json() as Session;

  const fork = await (await h(req("POST", "/sessions", { title: "f", parentId: root.id }))).json() as Session;
  assertEquals(fork.kind, "fork");
  assertEquals(fork.parentId, root.id);

  const bad = await h(req("POST", "/sessions", { title: "f", parentId: "nope" }));
  assertEquals(bad.status, 400);
  c.db.close();
});

Deno.test("GET /sessions marks a session busy while a turn is pending", async () => {
  const c = ctx();
  const h = createHandler(c);
  const s = await (await h(req("POST", "/sessions", { title: "b" }))).json() as Session;

  const before = await (await h(req("GET", "/sessions"))).json() as (Session & { busy: boolean })[];
  assertEquals(before[0].busy, false);

  // postMessage persists a pending supervisor placeholder before the turn runs.
  await h(req("POST", `/sessions/${s.id}/messages`, { text: "go" }));
  const during = await (await h(req("GET", "/sessions"))).json() as (Session & { busy: boolean })[];
  assertEquals(during[0].busy, true);
  c.db.close();
});

Deno.test("GET /sessions/:id returns session + thread-through-parents; 404 unknown", async () => {
  const c = ctx();
  const h = createHandler(c);
  const root = await (await h(req("POST", "/sessions", { title: "r" }))).json() as Session;
  await h(req("POST", `/sessions/${root.id}/messages`, { text: "hi from user" }));
  const fork = await (await h(req("POST", "/sessions", { title: "f", parentId: root.id }))).json() as Session;

  const res = await h(req("GET", `/sessions/${fork.id}`));
  const { session, thread } = await res.json() as { session: Session; thread: Message[] };
  assertEquals(session.id, fork.id);
  // The user message on root is inherited by the fork's thread.
  assertEquals(thread[0].role, "user");
  assertEquals(thread[0].parts, [{ type: "text", text: "hi from user" }]);

  assertEquals((await h(req("GET", "/sessions/missing"))).status, 404);
  c.db.close();
});

Deno.test("POST message returns 202 and persists user + pending supervisor msgs", async () => {
  const c = ctx();
  const h = createHandler(c);
  const s = await (await h(req("POST", "/sessions", { title: "s" }))).json() as Session;

  const res = await h(req("POST", `/sessions/${s.id}/messages`, { text: "go" }));
  assertEquals(res.status, 202);

  const msgs = c.db.messagesFor(s.id);
  assertEquals(msgs.length, 2);
  assertEquals(msgs[0].role, "user");
  assertEquals(msgs[0].pending, false);
  assertEquals(msgs[1].role, "supervisor");
  assertEquals(msgs[1].pending, true); // stub turn placeholder
  c.db.close();
});

Deno.test("archive hides a session from the list but keeps it addressable", async () => {
  const c = ctx();
  const h = createHandler(c);
  const s = await (await h(req("POST", "/sessions", { title: "old noise" }))).json() as Session;
  assertEquals((await h(req("POST", `/sessions/${s.id}/archive`, {}))).status, 200);
  const list = await (await h(req("GET", "/sessions"))).json() as Session[];
  assertEquals(list.some((x) => x.id === s.id), false);
  // The thread is still there — forks/lineage keep resolving.
  assertEquals((await h(req("GET", `/sessions/${s.id}`))).status, 200);
  assertEquals((await h(req("POST", "/sessions/nope/archive", {}))).status, 404);
  c.db.close();
});

Deno.test("invalid body → 400", async () => {
  const c = ctx();
  const h = createHandler(c);
  // title is optional now (title worker), so a wrong TYPE is the invalid case.
  assertEquals((await h(req("POST", "/sessions", { title: 123 }))).status, 400);
  c.db.close();
});

Deno.test("OPTIONS preflight → 204 with CORS", async () => {
  const c = ctx();
  const h = createHandler(c);
  const res = await h(req("OPTIONS", "/sessions"));
  assertEquals(res.status, 204);
  assertEquals(res.headers.get("access-control-allow-origin"), "*");
  c.db.close();
});

// ---- SSE smoke: real server, events flow from POSTs to the /events stream ----

Deno.test("smoke: /events streams named events for a posted turn", async () => {
  const c = ctx();
  const h = createHandler(c);
  const server = Deno.serve({ port: 0, onListen() {} }, h);
  const { port } = server.addr as Deno.NetAddr;
  const origin = `http://127.0.0.1:${port}`;

  // Open the SSE stream first so we don't miss events.
  const evRes = await fetch(`${origin}/events`);
  const reader = evRes.body!.getReader();
  const dec = new TextDecoder();

  const seen: string[] = [];
  const collect = (async () => {
    let buf = "";
    while (seen.length < 3) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let i;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const frame = buf.slice(0, i);
        buf = buf.slice(i + 2);
        const m = frame.match(/^event: (.+)$/m);
        if (m) seen.push(m[1]);
      }
    }
  })();

  const s = await (await fetch(`${origin}/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ title: "smoke" }),
  })).json() as Session;

  await fetch(`${origin}/sessions/${s.id}/messages`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ text: "hello" }),
  });

  await collect;
  reader.cancel();
  await server.shutdown();
  c.db.close();

  // session.created, then message.started (user) + message.started (supervisor).
  assertEquals(seen[0], "session.created");
  assertEquals(seen.filter((t) => t === "message.started").length, 2);
});

// The web client streams via POST (quick tunnels buffer GET event-streams; see routes).
Deno.test("POST /events streams the same event feed", async () => {
  const c = ctx();
  const h = createHandler(c);
  const server = Deno.serve({ port: 0, onListen() {} }, h);
  const { port } = server.addr as Deno.NetAddr;
  const origin = `http://127.0.0.1:${port}`;

  const evRes = await fetch(`${origin}/events`, { method: "POST" });
  assertEquals(evRes.headers.get("content-type"), "text/event-stream");
  const reader = evRes.body!.getReader();
  const dec = new TextDecoder();

  const first = (async () => {
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return null;
      buf += dec.decode(value, { stream: true });
      const m = buf.match(/^event: (.+)$/m);
      if (m) return m[1];
    }
  })();

  await fetch(`${origin}/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ title: "post-events" }),
  }).then((r) => r.body?.cancel());

  assertEquals(await first, "session.created");
  reader.cancel();
  await server.shutdown();
  c.db.close();
});
