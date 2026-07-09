import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { type AppCtx, createHandler } from "./app.ts";
import { BUILTIN_SERVERS } from "../mcp/config.ts";
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

  const cfg = await (await h(req("GET", "/config"))).json() as {
    model: string;
    models: { id: string }[];
  };
  assertEquals(cfg.model, "claude-opus-4-8");
  assert(cfg.models.some((m) => m.id === "claude-opus-4-8"));

  const patched = await h(req("PATCH", "/config", { model: "claude-haiku-4-5" }));
  assertEquals((await patched.json() as { model: string }).model, "claude-haiku-4-5");
  assertEquals(
    (await (await h(req("GET", "/config"))).json() as { model: string }).model,
    "claude-haiku-4-5",
  );

  assertEquals((await h(req("PATCH", "/config", { model: "" }))).status, 400);
  // Restore so later tests see the default.
  await h(req("PATCH", "/config", { model: "claude-opus-4-8" }));
  c.db.close();
});

Deno.test("GET /config lists workers; PATCH /config switches the worker", async () => {
  const c = ctx();
  const h = createHandler(c);

  const cfg = await (await h(req("GET", "/config"))).json() as {
    worker: string;
    workerOptions: { id: string }[];
  };
  assertEquals(cfg.worker, "local");
  assert(cfg.workerOptions.some((w) => w.id === "local"));
  assert(cfg.workerOptions.some((w) => w.id === "claude-haiku-4-5"));

  const patched = await h(req("PATCH", "/config", { worker: "claude-haiku-4-5" }));
  assertEquals((await patched.json() as { worker: string }).worker, "claude-haiku-4-5");
  assertEquals(
    (await (await h(req("GET", "/config"))).json() as { worker: string }).worker,
    "claude-haiku-4-5",
  );

  // Local-only pins the worker: switching to a frontier model is rejected and the
  // effective worker reads as local even with a frontier choice stored.
  Deno.env.set("BOUGH_WORKER_LOCAL_ONLY", "1");
  try {
    assertEquals((await h(req("PATCH", "/config", { worker: "claude-haiku-4-5" }))).status, 400);
    assertEquals(
      (await (await h(req("GET", "/config"))).json() as { worker: string }).worker,
      "local",
    );
  } finally {
    Deno.env.delete("BOUGH_WORKER_LOCAL_ONLY");
  }

  assertEquals((await h(req("PATCH", "/config", {}))).status, 400);
  // Restore so later tests see the default.
  await h(req("PATCH", "/config", { worker: "local" }));
  c.db.close();
});

Deno.test("GET /sessions/:id includes token usage (zero before any turn)", async () => {
  const c = ctx();
  const h = createHandler(c);
  const s = await (await h(req("POST", "/sessions", { title: "u" }))).json() as Session;
  const got = await (await h(req("GET", `/sessions/${s.id}`))).json() as {
    usage: {
      contextTokens: number;
      outputTokens: number;
      inputTokens: number;
      cachedTokens: number;
      lastLlmAt: number | null;
      tree: { inputTokens: number; outputTokens: number; sessions: number };
    };
  };
  assertEquals(got.usage, {
    contextTokens: 0,
    outputTokens: 0,
    inputTokens: 0,
    cachedTokens: 0,
    lastLlmAt: null,
    tree: { inputTokens: 0, outputTokens: 0, sessions: 0 },
  });
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

  const fork = await (await h(req("POST", "/sessions", { title: "f", parentId: root.id })))
    .json() as Session;
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
  const fork = await (await h(req("POST", "/sessions", { title: "f", parentId: root.id })))
    .json() as Session;

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

Deno.test("POST /sessions/:id/adopt guards: 404 unknown, 400 non-subagent / no workspace", async () => {
  const c = ctx();
  const h = createHandler(c);

  assertEquals((await h(req("POST", "/sessions/nope/adopt"))).status, 404);

  // A root session is not adoptable.
  const root = await (await h(req("POST", "/sessions", { title: "r" }))).json() as Session;
  assertEquals((await h(req("POST", `/sessions/${root.id}/adopt`))).status, 400);

  // A subagent with no branched workspace (chat-only spawn) is 400 with a message.
  c.db.createSession({
    id: "sub1",
    parentId: null,
    title: "sub",
    kind: "subagent",
    createdAt: 1,
    originId: root.id,
    originMessageId: "m1",
  });
  const res = await h(req("POST", "/sessions/sub1/adopt"));
  assertEquals(res.status, 400);
  assert(((await res.json()) as { error: string }).error.includes("no branched workspace"));
  c.db.close();
});

Deno.test("GET /sessions carries lastTurnStatus once a session has run a turn", async () => {
  const c = ctx();
  const h = createHandler(c);
  const s = await (await h(req("POST", "/sessions", { title: "t" }))).json() as Session;

  let list = await (await h(req("GET", "/sessions")))
    .json() as (Session & { lastTurnStatus?: string })[];
  assertEquals(list.find((x) => x.id === s.id)?.lastTurnStatus, undefined);

  c.db.createMessage({
    id: "m1",
    sessionId: s.id,
    role: "supervisor",
    parts: [],
    pending: false,
    createdAt: 2,
  });
  c.db.createTurn({
    id: "t1",
    sessionId: s.id,
    messageId: "m1",
    status: "error",
    step: "x",
    updatedAt: 3,
  });
  list = await (await h(req("GET", "/sessions")))
    .json() as (Session & { lastTurnStatus?: string })[];
  assertEquals(list.find((x) => x.id === s.id)?.lastTurnStatus, "error");
  c.db.close();
});

Deno.test("theme: PUT round-trips, 400 on bad color, DELETE reverts to default", async () => {
  const dir = Deno.makeTempDirSync();
  const c = { ...ctx(), themeDir: dir };
  const h = createHandler(c);

  // Nothing saved yet: default palette, contract exposed.
  const empty = await (await h(req("GET", "/theme"))).json() as {
    theme: unknown;
    tokens: string[];
    defaults: Record<string, string>;
  };
  assertEquals(empty.theme, null);
  assert(empty.tokens.includes("green"));
  assertEquals(empty.defaults.bg, "#0e1013");

  // Save a partial theme and read it back.
  const theme = { name: "Rosé Pine", colors: { bg: "#191724", green: "#9ccfd8" } };
  const put = await h(req("PUT", "/theme", theme));
  assertEquals(put.status, 200);
  const got = await (await h(req("GET", "/theme"))).json() as { theme: typeof theme };
  assertEquals(got.theme, theme);

  // Non-hex color and unknown token shape are rejected.
  assertEquals((await h(req("PUT", "/theme", { name: "x", colors: { bg: "red" } }))).status, 400);
  assertEquals((await h(req("PUT", "/theme", { colors: {} }))).status, 400); // name required

  // DELETE reverts to the default palette.
  assertEquals((await h(req("DELETE", "/theme"))).status, 200);
  const cleared = await (await h(req("GET", "/theme"))).json() as { theme: unknown };
  assertEquals(cleared.theme, null);
  c.db.close();
});

Deno.test("mcp: registry round-trips, enable/disable manage activations, guards hold", async () => {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-app-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  const c = ctx();
  const h = createHandler(c);
  try {
    // no user registry: builtins only, nothing active, nothing connected
    const empty = await (await h(req("GET", "/mcp/servers"))).json() as {
      registry: { servers: Record<string, unknown> };
      auth: Record<string, unknown>;
      active: string[];
      connections: unknown[];
    };
    assertEquals(empty, {
      registry: JSON.parse(JSON.stringify({ servers: BUILTIN_SERVERS })),
      auth: {},
      active: [],
      connections: [],
    });

    // PUT validates: bad shape 400, good shape persists
    assertEquals((await h(req("PUT", "/mcp/servers", { servers: { bad: {} } }))).status, 400);
    const put = await h(req("PUT", "/mcp/servers", {
      servers: { echo: { command: "deno", args: ["run", "srv.ts"] } },
    }));
    assertEquals(put.status, 200);

    // enable requires a registered name; unknown session 404s
    assertEquals((await h(req("POST", "/mcp/servers/ghost/enable"))).status, 400);
    assertEquals((await h(req("POST", "/mcp/servers/echo/enable?session=nope"))).status, 404);

    const s = await (await h(req("POST", "/sessions", { title: "m" }))).json() as Session;
    await h(req("POST", `/mcp/servers/echo/enable?session=${s.id}`));
    const active = await (await h(req("GET", `/mcp/servers?session=${s.id}`))).json() as {
      active: string[];
    };
    assertEquals(active.active, ["echo"]);
    // scoped to that session; disable clears it
    const other = await (await h(req("GET", "/mcp/servers"))).json() as { active: string[] };
    assertEquals(other.active, []);
    await h(req("POST", `/mcp/servers/echo/disable?session=${s.id}`));
    const after = await (await h(req("GET", `/mcp/servers?session=${s.id}`))).json() as {
      active: string[];
    };
    assertEquals(after.active, []);

    // restart needs a session and a live connection
    assertEquals((await h(req("POST", "/mcp/servers/echo/restart"))).status, 400);
    assertEquals((await h(req("POST", `/mcp/servers/echo/restart?session=${s.id}`))).status, 400);
  } finally {
    Deno.env.delete("BOUGH_MCP_DIR");
    c.db.close();
  }
});

Deno.test("mcp: per-server PUT/DELETE, connect-now proves a server runs", async () => {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-app2-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  const c = ctx();
  const h = createHandler(c);
  const fixture = new URL("../mcp/testdata/echo_server.ts", import.meta.url).pathname;
  try {
    // per-server PUT validates and leaves siblings alone
    assertEquals((await h(req("PUT", "/mcp/servers/bad name", { command: "x" }))).status, 400);
    assertEquals((await h(req("PUT", "/mcp/servers/other", {}))).status, 400);
    await h(req("PUT", "/mcp/servers/other", { command: "sleep", args: ["1"] }));
    const put = await h(req("PUT", "/mcp/servers/echo", {
      command: Deno.execPath(),
      args: ["run", "--quiet", "--no-config", fixture],
    }));
    assertEquals(put.status, 200);
    const reg = await (await h(req("GET", "/mcp/servers"))).json() as {
      registry: { servers: Record<string, unknown> };
    };
    assertEquals(
      Object.keys(reg.registry.servers).sort(),
      ["echo", "other", ...Object.keys(BUILTIN_SERVERS)].sort(),
    );

    // connect guards: session required/known, name registered
    const s = await (await h(req("POST", "/sessions", { title: "m" }))).json() as Session;
    assertEquals((await h(req("POST", "/mcp/servers/echo/connect"))).status, 400);
    assertEquals((await h(req("POST", "/mcp/servers/echo/connect?session=nope"))).status, 404);
    assertEquals(
      (await h(req("POST", `/mcp/servers/ghost/connect?session=${s.id}`))).status,
      400,
    );

    if ((await Deno.permissions.query({ name: "run" })).state === "granted") {
      // connect-now spawns the server and reports its catalog — the proof step
      const conn = await (await h(req("POST", `/mcp/servers/echo/connect?session=${s.id}`)))
        .json() as { connected: boolean; tools: { name: string }[] };
      assertEquals(conn.connected, true);
      assertEquals(conn.tools.map((t) => t.name), ["echo", "scream", "boom"]);

      // whole-registry PUT with echo unchanged keeps its connection alive
      const full = await (await h(req("GET", "/mcp/servers"))).json() as {
        registry: { servers: Record<string, unknown> };
      };
      await h(req("PUT", "/mcp/servers", full.registry));
      const kept = await (await h(req("GET", `/mcp/servers?session=${s.id}`))).json() as {
        connections: { server: string; alive: boolean }[];
      };
      assertEquals(kept.connections.map((x) => [x.server, x.alive]), [["echo", true]]);

      // per-server PUT of a CHANGED entry drops its connection
      await h(req("PUT", "/mcp/servers/echo", {
        command: Deno.execPath(),
        args: ["run", "--quiet", "--no-config", fixture],
        env: { CHANGED: "1" },
      }));
      const dropped = await (await h(req("GET", `/mcp/servers?session=${s.id}`))).json() as {
        connections: unknown[];
      };
      assertEquals(dropped.connections, []);
    }

    // DELETE unregisters; repeat 404s
    assertEquals((await h(req("DELETE", "/mcp/servers/echo"))).status, 200);
    assertEquals((await h(req("DELETE", "/mcp/servers/echo"))).status, 404);
  } finally {
    const { mcpManager } = await import("../mcp/manager.ts");
    await mcpManager().dropAll();
    Deno.env.delete("BOUGH_MCP_DIR");
    c.db.close();
  }
});

Deno.test("mcp oauth: auth endpoint guards; callback validates state", async () => {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-oauth-app-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  const c = ctx();
  const h = createHandler(c);
  try {
    await h(req("PUT", "/mcp/servers", {
      servers: {
        stdio: { command: "deno" },
        remote: { url: "https://example.invalid/mcp" },
      },
    }));
    // auth is for remote servers only; unknown name 400s
    assertEquals((await h(req("POST", "/mcp/servers/ghost/auth"))).status, 400);
    assertEquals((await h(req("POST", "/mcp/servers/stdio/auth"))).status, 400);
    // remote servers surface their auth state on GET
    const got = await (await h(req("GET", "/mcp/servers"))).json() as {
      auth: Record<string, { authorized: boolean }>;
    };
    assertEquals(got.auth, { remote: { authorized: false } });
    // callback rejects a flow bough never started, as HTML for the human
    const cb = await h(req("GET", "/mcp/oauth/callback?code=x&state=remote.forged"));
    assertEquals(cb.status, 400);
    assert((await cb.text()).includes("state mismatch"));
    assertEquals((await h(req("GET", "/mcp/oauth/callback"))).status, 400);
    // logout is idempotent
    assertEquals((await h(req("DELETE", "/mcp/servers/remote/auth"))).status, 200);
  } finally {
    Deno.env.delete("BOUGH_MCP_DIR");
    c.db.close();
  }
});
