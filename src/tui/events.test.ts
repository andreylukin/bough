/**
 * Tests for the SSE subscription.
 *
 * Two halves. The framing rules are exercised on strings, because a frame split
 * across a chunk boundary and a heartbeat comment are the two things a hand-written
 * SSE parser gets wrong, and both fail silently — the symptom is "the backend stopped
 * emitting" while the backend is emitting perfectly.
 *
 * The rest runs the real `GET /events` handler through an injected `fetch`, so the
 * bytes under test are the bytes `server/events.ts` writes, with no socket bound. The
 * load-bearing assertions:
 *
 *   - **No `Last-Event-ID`, ever.** `seq` is a dedupe key, not a resume cursor
 *     (spec §3, plan §6.16). Sending one would ask for a guarantee the server cannot
 *     keep across a restart, and would paper over the re-fetch that is the actual
 *     repair. The request headers are asserted, not assumed.
 *   - **`onOpen({reconnect})` fires true only on a RE-connect.** That flag is what
 *     triggers the store's resync; if it fired on the first open the client would
 *     double-fetch at boot, and if it never fired the client would sit on stale state
 *     forever after a drop.
 *   - **A frame the client does not understand is skipped, not fatal.** With the
 *     known-type list imported from the frozen schema, an unknown name means a server
 *     ahead of this client, not a reason to tear down the stream.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { EVENT_TYPES } from "../schema/events.ts";
import type { BoughEvent } from "../schema/events.ts";
import { createHandler } from "../server/app.ts";
import type { AppCtx, Db } from "../types.ts";
import { connectEvents, KNOWN_EVENT_TYPES, parseFrames } from "./events.ts";

// ---- framing ----------------------------------------------------------------

test("parseFrames returns the unconsumed tail so a split frame survives", () => {
  const seen: [string, string][] = [];
  const emit = (type: string, data: string) => seen.push([type, data]);

  let tail = parseFrames('event: message.delta\ndata: {"a":1}\n\nevent: turn.fi', emit);
  assert.deepEqual(seen, [["message.delta", '{"a":1}']]);
  assert.equal(tail, "event: turn.fi");

  tail = parseFrames(tail + 'nished\ndata: {"b":2}\n\n', emit);
  assert.deepEqual(seen[1], ["turn.finished", '{"b":2}']);
  assert.equal(tail, "");
});

test("comment lines are skipped without disturbing the stream", () => {
  const seen: string[] = [];
  const tail = parseFrames(
    ': connected\n\n: ping\n\nevent: tool.log\ndata: {"line":"x"}\n\n',
    (type) => seen.push(type),
  );
  assert.deepEqual(seen, ["tool.log"]);
  assert.equal(tail, "");
});

test("the known-type list is the schema's, so it cannot drift", () => {
  assert.deepEqual([...KNOWN_EVENT_TYPES].sort(), [...EVENT_TYPES].sort());
});

// ---- against the real handler ----------------------------------------------

/** A ctx with a real bus and a database that throws if anything touches it. */
function fixture(): AppCtx {
  const unusable = new Proxy({}, {
    get(_t, prop) {
      throw new Error(`/events must not touch the database (read ${String(prop)})`);
    },
  }) as Db;
  return { db: unusable, bus: new Bus() };
}

/** Wait until `check()` holds, or fail. Beats sleeping a fixed amount and hoping. */
async function until(check: () => boolean, what: string, tries = 200): Promise<void> {
  for (let i = 0; i < tries; i++) {
    if (check()) return;
    await new Promise((r) => setTimeout(r, 1));
  }
  throw new Error(`timed out waiting for: ${what}`);
}

test("events flow, parsed, and the request never asks to resume", async () => {
  const ctx = fixture();
  const handler = createHandler(ctx, { onUnexpectedError: () => {} });
  const requests: Request[] = [];
  const received: BoughEvent[] = [];

  const stream = connectEvents({
    url: "http://127.0.0.1:4321/events",
    fetchFn: (input, init) => {
      const req = new Request(input as string | URL, init);
      requests.push(req);
      return handler(req);
    },
    onEvent: (event) => received.push(event),
    delay: () => Promise.resolve(),
  });

  await until(() => ctx.bus.size === 1, "the stream to subscribe");
  ctx.bus.publish({
    type: "message.delta",
    sessionId: "s1",
    data: { messageId: "m", delta: "hi" },
  });
  ctx.bus.publish({
    type: "turn.finished",
    sessionId: "s1",
    data: { turnId: "t", sessionId: "s1", status: "done" },
  });
  await until(() => received.length === 2, "both events to arrive");

  assert.equal(received[0].type, "message.delta");
  assert.equal(received[0].seq, 1);
  assert.equal(received[1].type, "turn.finished");
  assert.equal(requests[0].headers.get("last-event-id"), null, "seq is not a resume cursor");

  stream.close();
  await stream.done;
  await until(() => ctx.bus.size === 0, "the subscription to be released");
});

test("onOpen reports a RE-connect, which is what triggers the store's re-fetch", async () => {
  const opens: boolean[] = [];
  const closes: number[] = [];
  let live: ReadableStreamDefaultController<Uint8Array> | null = null;
  const encoder = new TextEncoder();
  let attempt = 0;

  const stream = connectEvents({
    url: "http://127.0.0.1:4321/events",
    // First dial: the server is not up yet. Then two good connections, the first of
    // which is dropped under the client.
    fetchFn: () => {
      attempt++;
      if (attempt === 1) return Promise.reject(new TypeError("Connection refused"));
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          live = controller;
          controller.enqueue(encoder.encode(": connected\n\n"));
        },
      });
      return Promise.resolve(
        new Response(body, { headers: { "content-type": "text/event-stream" } }),
      );
    },
    onEvent: () => {},
    onOpen: ({ reconnect }) => opens.push(reconnect),
    onClose: () => closes.push(1),
    delay: () => Promise.resolve(),
  });

  await until(() => opens.length === 1, "the first successful open");
  assert.deepEqual(opens, [false], "a failed dial is not an open: nothing was missed yet");

  live!.close(); // the server went away mid-stream
  await until(() => opens.length === 2, "the redial");
  assert.deepEqual(opens, [false, true], "the second open must announce itself as a reconnect");
  assert.equal(closes.length, 1);

  stream.close();
  live!.close();
  await stream.done;
  assert.equal(stream.connected, false);
});

test("an unknown or malformed frame is skipped, and the stream survives it", async () => {
  const encoder = new TextEncoder();
  const received: BoughEvent[] = [];
  const bad: string[] = [];
  let live: ReadableStreamDefaultController<Uint8Array> | null = null;

  const stream = connectEvents({
    url: "http://127.0.0.1:4321/events",
    fetchFn: () =>
      Promise.resolve(
        new Response(
          new ReadableStream<Uint8Array>({
            start(controller) {
              live = controller;
              // A server ahead of this client, a truncated payload, an envelope that
              // is not one — then a perfectly good event.
              controller.enqueue(encoder.encode("event: session.teleported\ndata: {}\n\n"));
              controller.enqueue(encoder.encode("event: tool.log\ndata: {not json\n\n"));
              controller.enqueue(encoder.encode('event: tool.log\ndata: {"seq":"one"}\n\n'));
              controller.enqueue(encoder.encode(
                'event: tool.log\ndata: {"type":"tool.log","seq":4,"ts":9,"data":{"messageId":"m","callId":"c","line":"ok"}}\n\n',
              ));
            },
          }),
          { headers: { "content-type": "text/event-stream" } },
        ),
      ),
    onEvent: (event) => received.push(event),
    onBadFrame: ({ type }) => bad.push(type),
    delay: () => Promise.resolve(),
  });

  await until(() => received.length === 1, "the good event to arrive");
  assert.deepEqual(bad, ["session.teleported", "tool.log", "tool.log"]);
  assert.equal(received[0].seq, 4);

  stream.close();
  live!.close(); // the injected fetch does not honour the abort signal; the real one does
  await stream.done;
});

test("?sessionId scopes the stream but never drops un-scoped events", async () => {
  const ctx = fixture();
  const handler = createHandler(ctx, { onUnexpectedError: () => {} });
  const received: BoughEvent[] = [];

  const stream = connectEvents({
    base: "http://127.0.0.1:4321",
    sessionId: "s1",
    fetchFn: (input, init) => handler(new Request(input as string | URL, init)),
    onEvent: (event) => received.push(event),
    delay: () => Promise.resolve(),
  });

  await until(() => ctx.bus.size === 1, "the stream to subscribe");
  ctx.bus.publish({
    type: "message.delta",
    sessionId: "s2",
    data: { messageId: "x", delta: "no" },
  });
  ctx.bus.publish({ type: "workflow.log", data: { runId: "r", line: "global" } });
  ctx.bus.publish({
    type: "message.delta",
    sessionId: "s1",
    data: { messageId: "y", delta: "yes" },
  });
  await until(() => received.length === 2, "the two deliverable events");

  assert.deepEqual(received.map((e) => e.type), ["workflow.log", "message.delta"]);

  stream.close();
  await stream.done;
  await until(() => ctx.bus.size === 0, "the subscription to be released");
});
