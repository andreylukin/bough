/**
 * Tests for the router.
 *
 * The whole point of `createHandler(ctx)` is that the HTTP layer is drivable
 * without a server: every test here fabricates an `AppCtx` over an in-memory
 * database, builds a `Request` by hand, and calls the returned function. Nothing
 * binds a socket, claims a port, touches `~/.bough`, or reaches the network
 * (plan §7).
 *
 * The three that are load-bearing, and why:
 *
 *   - **`HttpError` → response.** The ground rule is that domain modules throw and
 *     the router renders (plan §0). If this mapping regressed, every module below
 *     `server/` would have to grow its own catch block and its own `Response`
 *     construction — the exact coupling `errors.ts` exists to prevent.
 *   - **A non-`HttpError` becomes a 500, not a dropped connection.** A defect in a
 *     handler must surface as an answer the client can show, and be reported once.
 *   - **First match wins, and the real table has no duplicate `(method, pathname)`.**
 *     `routes` is appended to by every task that adds an endpoint. Two entries for
 *     the same method and path is a merge accident where the second is dead code,
 *     and it is invisible in review; this catches it mechanically.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * not reachable from this environment, and a test that cannot run offline does not
 * belong in `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { z } from "zod";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { ConflictError, HttpError, NotFoundError } from "../errors.ts";
import type { AppCtx } from "../types.ts";
import {
  createHandler,
  errorResponse,
  type Handler,
  json,
  parseBody,
  type Route,
  route,
  routes,
} from "./app.ts";

// ---- fixtures ---------------------------------------------------------------

/** A fabricated ctx: real bus, in-memory database, no LLM, no socket. */
function fixture(): { ctx: AppCtx; db: SqliteDb } {
  const db = openDb(":memory:");
  return { ctx: { db, bus: new Bus(), model: "test-model" }, db };
}

function get(path: string): Request {
  return new Request(`http://127.0.0.1:4321${path}`);
}

function post(path: string, body?: unknown): Request {
  return new Request(`http://127.0.0.1:4321${path}`, {
    method: "POST",
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

/** Runs `fn` against a handler built over a fabricated ctx and a fabricated table. */
async function withHandler(
  table: Route[],
  fn: (
    call: (req: Request) => Promise<Response>,
    ctx: AppCtx,
    reported: unknown[],
  ) => Promise<void>,
): Promise<void> {
  const { ctx, db } = fixture();
  const reported: unknown[] = [];
  const call = createHandler(ctx, {
    routes: table,
    onUnexpectedError: (e) => reported.push(e),
  });
  try {
    await fn(call, ctx, reported);
  } finally {
    db.close();
  }
}

const ok: Handler = () => json({ ok: true });

// ---- dispatch ---------------------------------------------------------------

test("dispatches a matching method + pathname to its handler", async () => {
  await withHandler([route("GET", "/sessions", ok)], async (call) => {
    const res = await call(get("/sessions"));
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("content-type"), "application/json; charset=utf-8");
    assert.deepEqual(await res.json(), { ok: true });
  });
});

test("hands the handler the exact ctx createHandler was built with", async () => {
  let seen: AppCtx | undefined;
  const capture: Handler = (_req, ctx) => {
    seen = ctx;
    return json({});
  };
  await withHandler([route("GET", "/x", capture)], async (call, ctx) => {
    await call(get("/x"));
    // Identity, not shape: the router must pass the object through untouched, or
    // a test's fake db/bus would not be the one the handler uses.
    assert.equal(seen, ctx);
    assert.equal(seen?.model, "test-model");
  });
});

test("extracts named groups as params", async () => {
  let params: Record<string, string> | undefined;
  const capture: Handler = (_req, _ctx, p) => {
    params = p;
    return json({});
  };
  const table = [route("GET", "/sessions/:id/jobs/:jobId", capture)];
  await withHandler(table, async (call) => {
    await call(get("/sessions/abc/jobs/bg_1"));
    assert.deepEqual(params, { id: "abc", jobId: "bg_1" });
  });
});

test("omits an optional group that did not match, rather than passing undefined", async () => {
  let params: Record<string, string> | undefined;
  const capture: Handler = (_req, _ctx, p) => {
    params = p;
    return json({});
  };
  // `:path*` is how the artifact route is written. With nothing after the id the
  // group does not participate, and `URLPattern` reports it as `undefined`; the
  // router drops it so the key is simply absent and a handler can write
  // `params.path ?? ""` without the type lying to it.
  await withHandler([route("GET", "/artifacts/:id/:path*", capture)], async (call) => {
    await call(get("/artifacts/s1"));
    assert.equal(Object.hasOwn(params ?? {}, "path"), false);
    assert.deepEqual(params, { id: "s1" });
    await call(get("/artifacts/s1/deep/page.html"));
    assert.deepEqual(params, { id: "s1", path: "deep/page.html" });
  });
});

test("matches on pathname only — a query string does not affect routing", async () => {
  await withHandler([route("GET", "/events", ok)], async (call) => {
    const res = await call(get("/events?sessionId=abc"));
    assert.equal(res.status, 200);
  });
});

test("first match wins, so appending never steals an existing route", async () => {
  const first: Handler = () => json({ which: "first" });
  const second: Handler = () => json({ which: "second" });
  // The second entry also matches /sessions/new. Appending it must not change
  // what /sessions/new resolves to — this is why the table is never reordered.
  const table = [
    route("GET", "/sessions/new", first),
    route("GET", "/sessions/:id", second),
  ];
  await withHandler(table, async (call) => {
    assert.deepEqual(await (await call(get("/sessions/new"))).json(), { which: "first" });
    assert.deepEqual(await (await call(get("/sessions/x1"))).json(), { which: "second" });
  });
});

test("awaits an async handler", async () => {
  const slow: Handler = async () => {
    await Promise.resolve();
    return json({ ok: true }, 202);
  };
  await withHandler([route("POST", "/sessions/:id/messages", slow)], async (call) => {
    const res = await call(post("/sessions/a/messages"));
    assert.equal(res.status, 202);
  });
});

// ---- the one try/catch ------------------------------------------------------

test("maps a thrown HttpError subclass to its status and message", async () => {
  const missing: Handler = () => {
    throw new NotFoundError("session not found");
  };
  await withHandler([route("GET", "/sessions/:id", missing)], async (call, _ctx, reported) => {
    const res = await call(get("/sessions/nope"));
    assert.equal(res.status, 404);
    assert.deepEqual(await res.json(), { error: "session not found" });
    // A domain error is an outcome, not a defect: nothing is reported.
    assert.deepEqual(reported, []);
  });
});

test("maps each HttpError status, including ones no generic catch could guess", async () => {
  const table = [
    route("POST", "/conflict", () => {
      throw new ConflictError("that subagent already finished");
    }),
    route("POST", "/teapot", () => {
      throw new HttpError(413, "context window exceeded: 200000 tokens");
    }),
  ];
  await withHandler(table, async (call) => {
    assert.equal((await call(post("/conflict"))).status, 409);
    const overflow = await call(post("/teapot"));
    assert.equal(overflow.status, 413);
    assert.deepEqual(await overflow.json(), { error: "context window exceeded: 200000 tokens" });
  });
});

test("maps an HttpError rejected from an async handler too", async () => {
  const rejects: Handler = async () => {
    await Promise.resolve();
    throw new NotFoundError("gone");
  };
  await withHandler([route("GET", "/x", rejects)], async (call) => {
    assert.equal((await call(get("/x"))).status, 404);
  });
});

test("turns an unexpected error into a reported 500, never a dropped request", async () => {
  const boom = new TypeError("cannot read properties of undefined");
  await withHandler([route("GET", "/x", () => {
    throw boom;
  })], async (call, _ctx, reported) => {
    const res = await call(get("/x"));
    assert.equal(res.status, 500);
    assert.deepEqual(await res.json(), { error: "cannot read properties of undefined" });
    // Reported exactly once: it is a defect and must be visible in the log.
    assert.deepEqual(reported, [boom]);
  });
});

test("survives a handler throwing a non-Error value", async () => {
  await withHandler([route("GET", "/x", () => {
    throw "just a string";
  })], async (call) => {
    const res = await call(get("/x"));
    assert.equal(res.status, 500);
    assert.deepEqual(await res.json(), { error: "just a string" });
  });
});

test("one failing request does not poison the next", async () => {
  const table = [
    route("GET", "/bad", () => {
      throw new Error("boom");
    }),
    route("GET", "/good", ok),
  ];
  await withHandler(table, async (call) => {
    assert.equal((await call(get("/bad"))).status, 500);
    assert.equal((await call(get("/good"))).status, 200);
  });
});

// ---- fallbacks --------------------------------------------------------------

test("GET / returns a plain-text pointer", async () => {
  await withHandler([], async (call) => {
    const res = await call(get("/"));
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("content-type"), "text/plain; charset=utf-8");
    const body = await res.text();
    assert.match(body, /bough server/);
    // The pointer says there is no web UI, because there is not one (spec §17).
    assert.match(body, /no web UI/);
  });
});

test("an unknown path is a 404 naming the method and path", async () => {
  await withHandler([route("GET", "/sessions", ok)], async (call) => {
    const res = await call(get("/nope"));
    assert.equal(res.status, 404);
    assert.deepEqual(await res.json(), { error: "no route for GET /nope" });
  });
});

test("a known path with the wrong method is a 405 that names the allowed ones", async () => {
  const table = [
    route("GET", "/sessions/:id", ok),
    route("POST", "/sessions/:id", ok),
  ];
  await withHandler(table, async (call) => {
    const res = await call(new Request("http://127.0.0.1:4321/sessions/a", { method: "DELETE" }));
    assert.equal(res.status, 405);
    assert.equal(res.headers.get("allow"), "GET, POST");
    assert.match((await res.json()).error, /DELETE not allowed on \/sessions\/a/);
  });
});

test("the root pointer wins over a 405 when some other method owns /", async () => {
  await withHandler([route("POST", "/", ok)], async (call) => {
    const res = await call(get("/"));
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("content-type"), "text/plain; charset=utf-8");
  });
});

// ---- body parsing -----------------------------------------------------------

const Body = z.object({ text: z.string().min(1) });

test("parseBody yields validated data to the handler", async () => {
  const handler: Handler = async (req) => json({ echo: (await parseBody(req, Body)).text });
  await withHandler([route("POST", "/m", handler)], async (call) => {
    const res = await call(post("/m", { text: "hello" }));
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), { echo: "hello" });
  });
});

test("an invalid body becomes a 400 through the router's one catch", async () => {
  const handler: Handler = async (req) => json(await parseBody(req, Body));
  await withHandler([route("POST", "/m", handler)], async (call, _ctx, reported) => {
    const res = await call(post("/m", { text: 42 }));
    assert.equal(res.status, 400);
    assert.match((await res.json()).error, /^invalid body: /);
    // A 400 is a domain outcome, not a defect — it must not be logged as one.
    assert.deepEqual(reported, []);
  });
});

test("an absent body falls back, and the fallback decides the 400", async () => {
  const strict: Handler = async (req) => json(await parseBody(req, Body));
  const lenient: Handler = async (req) =>
    json(await parseBody(req, z.object({ paths: z.array(z.string()).optional() }), {}));
  await withHandler([route("POST", "/strict", strict), route("POST", "/lenient", lenient)], async (
    call,
  ) => {
    // Default fallback is null: the schema rejects it.
    assert.equal((await call(post("/strict"))).status, 400);
    // An all-optional body passes `{}` so "no body" means "no options".
    const res = await call(post("/lenient"));
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), {});
  });
});

test("malformed JSON is a 400, not a 500", async () => {
  const handler: Handler = async (req) => json(await parseBody(req, Body));
  await withHandler([route("POST", "/m", handler)], async (call, _ctx, reported) => {
    const req = new Request("http://127.0.0.1:4321/m", { method: "POST", body: "{not json" });
    assert.equal((await call(req)).status, 400);
    assert.deepEqual(reported, []);
  });
});

// ---- helpers ----------------------------------------------------------------

test("json and errorResponse produce the shapes every client reads", async () => {
  const res = json({ a: 1 }, 201);
  assert.equal(res.status, 201);
  assert.deepEqual(await res.json(), { a: 1 });
  const err = errorResponse(429, "spawn cap: 8 per turn");
  assert.equal(err.status, 429);
  assert.deepEqual(await err.json(), { error: "spawn cap: 8 per turn" });
});

// ---- the real route table ---------------------------------------------------

test("the shared route table has no duplicate (method, pathname) entry", () => {
  const seen = new Set<string>();
  for (const entry of routes) {
    const key = `${entry.method} ${entry.pattern.pathname}`;
    assert.equal(seen.has(key), false, `duplicate route appended: ${key}`);
    seen.add(key);
  }
});

test("createHandler defaults to the shared route table", async () => {
  const { ctx, db } = fixture();
  try {
    const call = createHandler(ctx);
    // Whatever tasks have appended, the fallbacks are always reachable.
    assert.equal((await call(get("/"))).status, 200);
    assert.equal((await call(get("/__no_such_route__"))).status, 404);
  } finally {
    db.close();
  }
});
