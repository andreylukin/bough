/**
 * Tests for the SSE endpoint.
 *
 * Everything here runs against a real `Bus` and a hand-built `Request`, with no
 * socket bound and nothing on the network (plan §7). The handler needs only
 * `ctx.bus`, which is the point of it doing no database work.
 *
 * The load-bearing ones, and why:
 *
 *   - **Framing.** Clients attach one listener per event name, so a frame that lost
 *     its `event:` line is silently ignored by the TUI's drain loop — a failure mode
 *     that looks like "the backend stopped emitting" and has burned this codebase
 *     before. The exact bytes are asserted, not a substring.
 *   - **No `id:` field.** `id:` is the SSE resume mechanism; emitting one would
 *     advertise replay-from-cursor, which this server cannot honour because `seq`
 *     resets on restart (spec §3, plan §6.16). Its absence is a contract, so it is
 *     asserted rather than assumed.
 *   - **The leak check.** A leaked subscriber is delivered to for the life of the
 *     process, on the synchronous emit path of every turn. N connect/disconnect
 *     cycles must return the subscriber count to zero — from the normal cancel path
 *     and from the client-vanished path alike.
 *   - **Filtering keeps global events.** A `?sessionId=` stream that dropped
 *     un-scoped events would lose exactly the announcements it has no other way to
 *     learn about, and with no replay it would never recover them.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable from this environment, and a test that cannot run offline does not
 * belong in `deno task test`.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import type { EventInput } from "../schema/events.ts";
import type { AppCtx, Db } from "../types.ts";
import { createHandler, routes } from "./app.ts";
import {
  CONNECTED_FRAME,
  createEventsHandler,
  events,
  frame,
  HEARTBEAT_FRAME,
  passesFilter,
  type TimerHandle,
  type Timers,
} from "./events.ts";

// ---- fixtures ---------------------------------------------------------------

/**
 * A ctx carrying a real bus and no database. `/events` touches neither the db nor
 * the LLM; handing it a stub is how that stays true — a future edit that reaches
 * for `ctx.db` fails here rather than quietly acquiring a dependency.
 */
function fixture(): AppCtx {
  const unusable = new Proxy({}, {
    get(_t, prop) {
      throw new Error(`/events must not touch the database (read ${String(prop)})`);
    },
  }) as Db;
  return { db: unusable, bus: new Bus() };
}

function get(path: string, signal?: AbortSignal): Request {
  return new Request(`http://127.0.0.1:4321${path}`, signal ? { signal } : undefined);
}

/** Frame-at-a-time reader over an SSE response body. */
class Sse {
  readonly #reader: ReadableStreamDefaultReader<Uint8Array>;
  readonly #decoder = new TextDecoder();
  #buffer = "";

  constructor(res: Response) {
    assert.ok(res.body, "SSE response must have a body");
    this.#reader = res.body.getReader();
  }

  /** The next complete frame, delimiter included. Rejects if the stream ends first. */
  async next(): Promise<string> {
    for (;;) {
      const end = this.#buffer.indexOf("\n\n");
      if (end >= 0) {
        const one = this.#buffer.slice(0, end + 2);
        this.#buffer = this.#buffer.slice(end + 2);
        return one;
      }
      const { value, done } = await this.#reader.read();
      if (done) throw new Error("stream ended before a frame arrived");
      this.#buffer += this.#decoder.decode(value, { stream: true });
    }
  }

  /** Resolves true once the stream reports end-of-stream. */
  async ended(): Promise<boolean> {
    const { done } = await this.#reader.read();
    return done;
  }

  cancel(): Promise<void> {
    return this.#reader.cancel();
  }
}

/** A fake clock for the heartbeat: nothing fires until `tick()` is called. */
function fakeTimers(): Timers & { tick(): void; live: number } {
  const registered = new Map<number, () => void>();
  let nextId = 1;
  return {
    setInterval(callback: () => void) {
      const id = nextId++;
      registered.set(id, callback);
      return id;
    },
    clearInterval(handle: TimerHandle) {
      registered.delete(handle as number);
    },
    tick() {
      for (const callback of [...registered.values()]) callback();
    },
    get live() {
      return registered.size;
    },
  };
}

const sample = (sessionId?: string): EventInput => ({
  type: "message.delta",
  ...(sessionId === undefined ? {} : { sessionId }),
  data: { messageId: "m1", delta: "hi" },
});

// ---- framing ----------------------------------------------------------------

Deno.test("opens with a comment frame and the SSE content type", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(get("/events"), ctx, {});

  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "text/event-stream");
  assert.match(res.headers.get("cache-control") ?? "", /no-cache/);

  const sse = new Sse(res);
  assert.equal(await sse.next(), CONNECTED_FRAME);
  await sse.cancel();
});

Deno.test("frames each event as `event: <type>` + one `data:` line", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(get("/events"), ctx, {});
  const sse = new Sse(res);
  await sse.next(); // the preamble

  const published = ctx.bus.publish(sample("s1"));
  const got = await sse.next();

  assert.equal(got, `event: message.delta\ndata: ${JSON.stringify(published)}\n\n`);

  // The payload is the whole stamped envelope, seq and ts included.
  const [head, body] = got.trimEnd().split("\n");
  assert.equal(head, "event: message.delta");
  const parsed = JSON.parse(body.slice("data: ".length));
  assert.equal(parsed.type, "message.delta");
  assert.equal(parsed.sessionId, "s1");
  assert.equal(parsed.seq, published.seq);
  assert.equal(parsed.ts, published.ts);
  assert.deepEqual(parsed.data, { messageId: "m1", delta: "hi" });

  await sse.cancel();
});

Deno.test("never emits an SSE `id:` field — seq is a dedupe key, not a resume cursor", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(get("/events"), ctx, {});
  const sse = new Sse(res);
  await sse.next();

  ctx.bus.publish(sample("s1"));
  const got = await sse.next();
  for (const line of got.split("\n")) {
    assert.ok(!line.startsWith("id:"), `frame carries a resume cursor: ${JSON.stringify(got)}`);
  }

  await sse.cancel();
});

Deno.test("a multi-line payload stays on one `data:` line", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(get("/events"), ctx, {});
  const sse = new Sse(res);
  await sse.next();

  ctx.bus.publish({
    type: "tool.log",
    sessionId: "s1",
    data: { messageId: "m1", callId: "c1", line: "line one\nline two\n\nline three" },
  });
  const got = await sse.next();

  const lines = got.trimEnd().split("\n");
  assert.equal(lines.length, 2, `frame split across lines: ${JSON.stringify(got)}`);
  const parsed = JSON.parse(lines[1].slice("data: ".length));
  assert.equal(parsed.data.line, "line one\nline two\n\nline three");

  await sse.cancel();
});

Deno.test("frames every declared event type", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(get("/events"), ctx, {});
  const sse = new Sse(res);
  await sse.next();

  for (const type of ["session.created", "ask.question", "workflow.log"] as const) {
    ctx.bus.publish({ type, sessionId: "s1", data: {} } as unknown as EventInput);
    assert.match(await sse.next(), new RegExp(`^event: ${type.replace(".", "\\.")}\\n`));
  }

  await sse.cancel();
});

Deno.test("an unencodable payload is skipped, not fatal to the connection", async () => {
  const ctx = fixture();
  const reported: { phase: string }[] = [];
  const res = await createEventsHandler({
    heartbeatMs: 0,
    onStreamError: (_e, info) => reported.push(info),
  })(get("/events"), ctx, {});
  const sse = new Sse(res);
  await sse.next();

  const cyclic: Record<string, unknown> = {};
  cyclic.self = cyclic;
  ctx.bus.publish({ type: "session.updated", sessionId: "s1", data: cyclic } as EventInput);
  ctx.bus.publish(sample("s1"));

  // The good event still arrives, so the stream survived the bad one.
  assert.match(await sse.next(), /^event: message\.delta\n/);
  assert.deepEqual(reported, [{ phase: "serialize" }]);
  assert.equal(ctx.bus.size, 1);

  await sse.cancel();
});

// ---- filtering --------------------------------------------------------------

Deno.test("passesFilter: no filter passes everything; a global event always passes", () => {
  assert.equal(passesFilter({ sessionId: "s1" }, null), true);
  assert.equal(passesFilter({ sessionId: "s2" }, null), true);
  assert.equal(passesFilter({}, null), true);

  assert.equal(passesFilter({ sessionId: "s1" }, "s1"), true);
  assert.equal(passesFilter({ sessionId: "s2" }, "s1"), false);
  // The rule the endpoint exists to protect: un-scoped events are never dropped.
  assert.equal(passesFilter({}, "s1"), true);
  assert.equal(passesFilter({ sessionId: undefined }, "s1"), true);
});

Deno.test("?sessionId= drops other sessions but keeps global events", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(
    get("/events?sessionId=s1"),
    ctx,
    {},
  );
  const sse = new Sse(res);
  await sse.next();

  ctx.bus.publish(sample("s2")); // other session — must not appear
  const globalEvent = ctx.bus.publish(sample(undefined)); // no session — must appear
  const mine = ctx.bus.publish(sample("s1"));

  const first = await sse.next();
  assert.equal(JSON.parse(first.split("\n")[1].slice(6)).seq, globalEvent.seq);
  const second = await sse.next();
  assert.equal(JSON.parse(second.split("\n")[1].slice(6)).seq, mine.seq);

  await sse.cancel();
});

Deno.test("an unfiltered stream receives every session", async () => {
  const ctx = fixture();
  const res = await createEventsHandler({ heartbeatMs: 0 })(get("/events"), ctx, {});
  const sse = new Sse(res);
  await sse.next();

  const a = ctx.bus.publish(sample("s1"));
  const b = ctx.bus.publish(sample("s2"));
  assert.equal(JSON.parse((await sse.next()).split("\n")[1].slice(6)).seq, a.seq);
  assert.equal(JSON.parse((await sse.next()).split("\n")[1].slice(6)).seq, b.seq);

  await sse.cancel();
});

// ---- heartbeat --------------------------------------------------------------

Deno.test("writes a comment heartbeat on each tick and clears it on disconnect", async () => {
  const ctx = fixture();
  const timers = fakeTimers();
  const res = await createEventsHandler({ heartbeatMs: 15_000, timers })(
    get("/events"),
    ctx,
    {},
  );
  const sse = new Sse(res);
  await sse.next();

  assert.equal(timers.live, 1, "the heartbeat interval must be registered");
  timers.tick();
  assert.equal(await sse.next(), HEARTBEAT_FRAME);
  timers.tick();
  assert.equal(await sse.next(), HEARTBEAT_FRAME);

  await sse.cancel();
  assert.equal(timers.live, 0, "the interval must be cleared on disconnect");
});

Deno.test("a heartbeat tick after teardown is inert", async () => {
  const ctx = fixture();
  const timers = fakeTimers();
  const res = await createEventsHandler({ heartbeatMs: 15_000, timers })(
    get("/events"),
    ctx,
    {},
  );
  const sse = new Sse(res);
  await sse.next();
  await sse.cancel();

  timers.tick(); // no registered callbacks remain; must not throw
  assert.equal(ctx.bus.size, 0);
});

// ---- teardown and the leak check --------------------------------------------

Deno.test("N connect/disconnect cycles leave no listener leak", async () => {
  const ctx = fixture();
  const handler = createEventsHandler({ heartbeatMs: 0 });
  assert.equal(ctx.bus.size, 0);

  for (let i = 0; i < 50; i++) {
    const res = await handler(get("/events"), ctx, {});
    const sse = new Sse(res);
    await sse.next();
    assert.equal(ctx.bus.size, 1, `cycle ${i}: exactly one subscriber while open`);

    ctx.bus.publish(sample("s1"));
    await sse.next();

    await sse.cancel();
    assert.equal(ctx.bus.size, 0, `cycle ${i}: the subscriber must be released`);
  }

  assert.equal(ctx.bus.size, 0);
  // Publishing with every client gone neither throws nor delivers anywhere.
  ctx.bus.publish(sample("s1"));
  assert.equal(ctx.bus.size, 0);
});

Deno.test("concurrent streams unsubscribe independently", async () => {
  const ctx = fixture();
  const handler = createEventsHandler({ heartbeatMs: 0 });

  const open: Sse[] = [];
  for (let i = 0; i < 5; i++) {
    const sse = new Sse(await handler(get("/events"), ctx, {}));
    await sse.next();
    open.push(sse);
  }
  assert.equal(ctx.bus.size, 5);

  await open[2].cancel();
  assert.equal(ctx.bus.size, 4);

  // The survivors still receive.
  ctx.bus.publish(sample("s1"));
  for (const [i, sse] of open.entries()) {
    if (i === 2) continue;
    assert.match(await sse.next(), /^event: message\.delta\n/);
  }

  for (const [i, sse] of open.entries()) if (i !== 2) await sse.cancel();
  assert.equal(ctx.bus.size, 0);
});

Deno.test("an aborted request releases its subscription and ends the stream", async () => {
  const ctx = fixture();
  const timers = fakeTimers();
  const controller = new AbortController();
  const res = await createEventsHandler({ heartbeatMs: 15_000, timers })(
    get("/events", controller.signal),
    ctx,
    {},
  );
  const sse = new Sse(res);
  await sse.next();
  assert.equal(ctx.bus.size, 1);

  controller.abort();

  assert.equal(ctx.bus.size, 0, "an abandoned connection must not stay subscribed");
  assert.equal(timers.live, 0);
  assert.equal(await sse.ended(), true);
});

Deno.test("a request aborted before the body starts subscribes to nothing", async () => {
  const ctx = fixture();
  const timers = fakeTimers();
  const controller = new AbortController();
  controller.abort();

  const res = await createEventsHandler({ heartbeatMs: 15_000, timers })(
    get("/events", controller.signal),
    ctx,
    {},
  );
  assert.equal(ctx.bus.size, 0);
  assert.equal(timers.live, 0);
  await res.body?.cancel();
});

Deno.test("teardown is idempotent across abort and cancel", async () => {
  const ctx = fixture();
  const controller = new AbortController();
  const res = await createEventsHandler({ heartbeatMs: 0 })(
    get("/events", controller.signal),
    ctx,
    {},
  );
  const sse = new Sse(res);
  await sse.next();

  controller.abort();
  await sse.cancel(); // the other trigger, after the first already ran
  assert.equal(ctx.bus.size, 0);
});

// ---- route wiring -----------------------------------------------------------

Deno.test("the real route table exposes GET /events exactly once", () => {
  const matching = routes.filter((r) =>
    r.method === "GET" && r.pattern.exec({ pathname: "/events" })
  );
  assert.equal(matching.length, 1);
});

Deno.test("dispatches through createHandler with the production table", async () => {
  const ctx = fixture();
  const call = createHandler(ctx);
  const res = await call(get("/events?sessionId=s1"));

  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "text/event-stream");

  const sse = new Sse(res);
  assert.equal(await sse.next(), CONNECTED_FRAME);
  const published = ctx.bus.publish(sample("s1"));
  assert.equal(await sse.next(), frame(published));

  await sse.cancel();
  assert.equal(ctx.bus.size, 0, "the production handler must release its subscription too");
});

Deno.test("the exported handler is the one the table uses", () => {
  const entry = routes.find((r) => r.method === "GET" && r.pattern.exec({ pathname: "/events" }));
  assert.equal(entry?.handler, events);
});
