import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { type AppCtx, createHandler } from "./app.ts";
import { activeModel, usableContextLimit } from "../turn.ts";
import type { Message, Session } from "../schema/parts.ts";

function ctx(): AppCtx {
  const bus = new Bus();
  // envDir: PATCH /config persists the default model to the launcher env file —
  // point it at a throwaway dir so tests never touch the real ~/.bough/env.
  return { db: new Db(":memory:"), bus, envDir: Deno.makeTempDirSync({ prefix: "app-env-" }) };
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

Deno.test("PATCH /config with sessionId pins that session; others keep theirs", async () => {
  const c = ctx();
  const h = createHandler(c);
  const events: unknown[] = [];
  c.bus.subscribe((e) => e.type === "session.updated" && events.push(e.data));
  c.db.createSession({ id: "A", parentId: null, title: "a", kind: "root", createdAt: 1 });
  c.db.createSession({ id: "B", parentId: null, title: "b", kind: "root", createdAt: 2 });

  const res = await h(
    req("PATCH", "/config", { model: "claude-haiku-4-5", sessionId: "A" }),
  );
  assertEquals(res.status, 200);
  // The open session is pinned, the sibling untouched, the global default moved.
  assertEquals(c.db.getSession("A")?.model, "claude-haiku-4-5");
  assertEquals(c.db.getSession("B")?.model, undefined);
  assertEquals(
    (await (await h(req("GET", "/config"))).json() as { model: string }).model,
    "claude-haiku-4-5",
  );
  // The pin is announced so open UIs refresh the session row.
  assertEquals((events.at(-1) as { model?: string })?.model, "claude-haiku-4-5");
  // …and the session row carries it over the wire.
  const got = await (await h(req("GET", "/sessions/A"))).json() as {
    session: { model?: string };
  };
  assertEquals(got.session.model, "claude-haiku-4-5");

  assertEquals(
    (await h(req("PATCH", "/config", { model: "claude-opus-4-8", sessionId: "zzz" }))).status,
    404,
  );
  // Restore the process-global default for later tests.
  await h(req("PATCH", "/config", { model: "claude-opus-4-8" }));
  c.db.close();
});

Deno.test("PATCH /config effort: validates, pins per session, 'default' clears", async () => {
  const c = ctx();
  const h = createHandler(c);
  c.db.createSession({ id: "A", parentId: null, title: "a", kind: "root", createdAt: 1 });

  // Advertised in GET /config; starts unset.
  const cfg = await (await h(req("GET", "/config"))).json() as {
    effort: string;
    efforts: string[];
  };
  assertEquals(cfg.effort, "");
  assert(cfg.efforts.includes("xhigh"));

  assertEquals((await h(req("PATCH", "/config", { effort: "extreme" }))).status, 400);

  // Pin: session + global default move together (same semantics as model).
  const res = await h(req("PATCH", "/config", { effort: "xhigh", sessionId: "A" }));
  assertEquals((await res.json() as { effort: string }).effort, "xhigh");
  assertEquals(c.db.getSession("A")?.effort, "xhigh");

  // "default" clears the global and the pin.
  await h(req("PATCH", "/config", { effort: "default", sessionId: "A" }));
  assertEquals(c.db.getSession("A")?.effort, undefined);
  assertEquals(
    (await (await h(req("GET", "/config"))).json() as { effort: string }).effort,
    "",
  );
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

Deno.test("GET /config exposes key booleans; PUT /config/keys validates", async () => {
  const c = ctx();
  const h = createHandler(c);

  const cfg = await (await h(req("GET", "/config"))).json() as { keys: Record<string, unknown> };
  // A boolean per provider — never a value.
  assertEquals(typeof cfg.keys.anthropic, "boolean");
  assertEquals(typeof cfg.keys.openrouter, "boolean");
  assertEquals(typeof cfg.keys.openai, "boolean");

  // Validation: unknown provider, empty key, newline key, and empty body all 400.
  assertEquals((await h(req("PUT", "/config/keys", { provider: "bogus", key: "x" }))).status, 400);
  assertEquals((await h(req("PUT", "/config/keys", { provider: "openai", key: "" }))).status, 400);
  assertEquals(
    (await h(req("PUT", "/config/keys", { provider: "openai", key: "a\nb" }))).status,
    400,
  );
  assertEquals((await h(req("PUT", "/config/keys", {}))).status, 400);
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
      cacheReadTotal: number;
      cacheWriteTotal: number;
      costUsd: number;
      contextLimit: number | null;
      lastLlmAt: number | null;
      tree: { inputTokens: number; outputTokens: number; costUsd: number; sessions: number };
    };
  };
  assertEquals(got.usage, {
    contextTokens: 0,
    outputTokens: 0,
    inputTokens: 0,
    cachedTokens: 0,
    cacheReadTotal: 0,
    cacheWriteTotal: 0,
    costUsd: 0,
    // The default model's usable prompt budget (catalog window − output reservation).
    contextLimit: usableContextLimit(activeModel()),
    lastLlmAt: null,
    tree: { inputTokens: 0, outputTokens: 0, costUsd: 0, sessions: 0 },
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

Deno.test("artifact comments: POST adds, GET lists, DELETE removes (404s guarded)", async () => {
  // Isolate the comments sidecar under a throwaway HOME so the real ~/.bough is untouched.
  const home = Deno.makeTempDirSync({ prefix: "cmt-home-" });
  const prevHome = Deno.env.get("HOME");
  Deno.env.set("HOME", home);
  try {
    const c = ctx();
    const h = createHandler(c);
    const s = await (await h(req("POST", "/sessions", { title: "art" }))).json() as Session;
    const anchor = { label: "Files", selector: "body>h2", xf: 0.5, yf: 0.2 };

    // Unknown session → 404 on add.
    assertEquals(
      (await h(req("POST", "/sessions/nope/comments", {
        artifact: "i.html",
        text: "x",
        anchor,
      }))).status,
      404,
    );

    const added = await (await h(req("POST", `/sessions/${s.id}/comments`, {
      artifact: "index.html",
      text: "this is stale",
      anchor,
    }))).json() as { id: string; sent: boolean };
    assertEquals(added.sent, false);

    const listed = await (await h(req("GET", `/sessions/${s.id}/comments`))).json() as {
      comments: { text: string }[];
    };
    assertEquals(listed.comments.map((x) => x.text), ["this is stale"]);

    assertEquals((await h(req("DELETE", `/sessions/${s.id}/comments/${added.id}`))).status, 200);
    assertEquals((await h(req("DELETE", `/sessions/${s.id}/comments/${added.id}`))).status, 404);
    assertEquals(
      ((await (await h(req("GET", `/sessions/${s.id}/comments`))).json()) as { comments: [] })
        .comments.length,
      0,
    );
    c.db.close();
  } finally {
    if (prevHome !== undefined) Deno.env.set("HOME", prevHome);
    Deno.removeSync(home, { recursive: true });
  }
});

Deno.test("POST /sessions with model pins the session (bough exec -m)", async () => {
  const c = ctx();
  const h = createHandler(c);
  const created = await (await h(
    req("POST", "/sessions", { title: "m", model: "claude-haiku-4-5" }),
  )).json() as Session;
  assertEquals(created.model, "claude-haiku-4-5");
  assertEquals(c.db.getSession(created.id)?.model, "claude-haiku-4-5");
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

Deno.test("schedules: CRUD round-trip, spec/workspace validation, 404s", async () => {
  const c = ctx();
  const h = createHandler(c);
  const ws = Deno.makeTempDirSync({ prefix: "sched-api-" });

  // Create — spec validated, next_run_at computed, enabled defaults on.
  const res = await h(
    req("POST", "/schedules", {
      title: "deploy check",
      prompt: "check it",
      spec: "every:30m",
      workspace: ws,
    }),
  );
  assertEquals(res.status, 201);
  const created = await res.json() as {
    id: string;
    enabled: boolean;
    nextRunAt: number;
    workspace: string;
  };
  assertEquals(created.enabled, true);
  assert(created.nextRunAt > Date.now());
  assertEquals(created.workspace, ws);

  // Bad spec / bad workspace → 400 with the parser's message.
  assertEquals(
    (await h(req("POST", "/schedules", { title: "x", prompt: "y", spec: "weekly" }))).status,
    400,
  );
  assertEquals(
    (await h(
      req("POST", "/schedules", {
        title: "x",
        prompt: "y",
        spec: "every:1h",
        workspace: "/nope/zzz",
      }),
    ))
      .status,
    400,
  );

  // List.
  const listed = await (await h(req("GET", "/schedules"))).json() as {
    schedules: { id: string }[];
  };
  assertEquals(listed.schedules.map((s) => s.id), [created.id]);

  // PATCH: disable, then edit the spec (recomputes next run); bad spec 400; 404 unknown.
  const off = await (await h(req("PATCH", `/schedules/${created.id}`, { enabled: false })))
    .json() as {
      enabled: boolean;
    };
  assertEquals(off.enabled, false);
  const respec = await (await h(req("PATCH", `/schedules/${created.id}`, { spec: "daily@09:00" })))
    .json() as {
      spec: string;
    };
  assertEquals(respec.spec, "daily@09:00");
  assertEquals((await h(req("PATCH", `/schedules/${created.id}`, { spec: "bogus" }))).status, 400);
  assertEquals((await h(req("PATCH", "/schedules/zzz", { enabled: true }))).status, 404);

  // DELETE: removes; unknown id 404s.
  assertEquals((await h(req("DELETE", `/schedules/${created.id}`))).status, 200);
  assertEquals(
    ((await (await h(req("GET", "/schedules"))).json()) as { schedules: unknown[] }).schedules
      .length,
    0,
  );
  assertEquals((await h(req("DELETE", `/schedules/${created.id}`))).status, 404);
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

Deno.test("no CORS: responses carry no allow-origin; a browser can't drive the loopback API", async () => {
  const c = ctx();
  const h = createHandler(c);
  // A normal API response opts into no cross-origin access.
  const get = await h(req("GET", "/sessions"));
  assertEquals(get.headers.get("access-control-allow-origin"), null);
  // No preflight handler either — OPTIONS isn't a routed method, so it 404s.
  const opt = await h(req("OPTIONS", "/sessions"));
  assertEquals(opt.status, 404);
  assertEquals(opt.headers.get("access-control-allow-origin"), null);
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
    firstOutputAt: null,
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
      registry: { servers: {} },
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
    assertEquals(Object.keys(reg.registry.servers).sort(), ["echo", "other"]);

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

Deno.test("GET /artifacts/:id/* serves a published artifact; /sessions/:id/artifacts lists them", async () => {
  const home = Deno.makeTempDirSync({ prefix: "app-artifacts-" });
  const prevHome = Deno.env.get("HOME");
  Deno.env.set("HOME", home);
  try {
    const { publishArtifact } = await import("./artifacts.ts");
    await publishArtifact("sessX", "index.html", "<!doctype html><title>demo</title>");
    await publishArtifact("sessX", "css/app.css", "body{color:red}");

    const c = ctx();
    const h = createHandler(c);

    const page = await h(req("GET", "/artifacts/sessX/index.html"));
    assertEquals(page.status, 200);
    assertEquals(page.headers.get("content-type"), "text/html; charset=utf-8");
    assert((await page.text()).includes("<title>demo</title>"));

    const css = await h(req("GET", "/artifacts/sessX/css/app.css"));
    assertEquals(css.status, 200);
    assertEquals(css.headers.get("content-type"), "text/css; charset=utf-8");

    const missing = await h(req("GET", "/artifacts/sessX/ghost.html"));
    assertEquals(missing.status, 404);

    const listed = await (await h(req("GET", "/sessions/sessX/artifacts"))).json() as {
      artifacts: { name: string }[];
    };
    assertEquals(listed.artifacts.map((a) => a.name).sort(), ["css/app.css", "index.html"]);

    c.db.close();
  } finally {
    if (prevHome) Deno.env.set("HOME", prevHome);
    else Deno.env.delete("HOME");
    await Deno.remove(home, { recursive: true });
  }
});

Deno.test("questions: GET lists pending asks; POST answers/declines; stale ids 404", async () => {
  const { raiseAsk } = await import("../asks.ts");
  const c = ctx();
  const h = createHandler(c);

  const q1 = raiseAsk(c.bus, {
    sessionId: "s1",
    messageId: "m1",
    question: "Which env?",
    options: ["dev", "prod"],
  });

  // A freshly-attached client rebuilds the hold from the GET.
  const listed = await (await h(req("GET", "/questions"))).json() as {
    id: string;
    status: string;
    options?: string[];
  }[];
  assertEquals(listed.length, 1);
  assertEquals(listed[0].id, q1.record.id);
  assertEquals(listed[0].status, "pending");
  assertEquals(listed[0].options, ["dev", "prod"]);
  // Session filter: another session sees nothing.
  assertEquals(
    await (await h(req("GET", "/questions?sessionId=zzz"))).json() as unknown[],
    [],
  );

  // Wrong session in the path → 404; empty body → 400.
  assertEquals(
    (await h(req("POST", `/sessions/zzz/questions/${q1.record.id}`, { answer: "dev" }))).status,
    404,
  );
  assertEquals(
    (await h(req("POST", `/sessions/s1/questions/${q1.record.id}`, {}))).status,
    400,
  );

  // Answer resolves the program's promise and clears the listing.
  const res = await h(req("POST", `/sessions/s1/questions/${q1.record.id}`, { answer: "prod" }));
  assertEquals(res.status, 200);
  assertEquals(await q1.answer, "prod");
  assertEquals((await (await h(req("GET", "/questions"))).json() as unknown[]).length, 0);
  // Settled → gone: a second answer 404s instead of double-settling.
  assertEquals(
    (await h(req("POST", `/sessions/s1/questions/${q1.record.id}`, { answer: "x" }))).status,
    404,
  );

  // Decline rejects the program's ask() with the catchable error.
  const q2 = raiseAsk(c.bus, { sessionId: "s1", messageId: "m1", question: "Proceed?" });
  assertEquals(
    (await h(req("POST", `/sessions/s1/questions/${q2.record.id}`, { decline: true }))).status,
    200,
  );
  await q2.answer.then(
    () => {
      throw new Error("decline should reject");
    },
    (err: Error) => assert(err.message.includes("user declined")),
  );
  c.db.close();
});
