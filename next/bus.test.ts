/**
 * Tests for the event bus.
 *
 * Two properties carry real weight and everything here exists to pin one of them.
 *
 * The first is listener isolation (plan §6.6). A subscriber is a client's SSE
 * connection; one that dies mid-fan-out throws on enqueue, and if that propagated,
 * a single detached client would stop every other client's stream. So it is checked
 * in the position that actually breaks — a thrower *before* a healthy listener —
 * rather than last, where a broken implementation would pass by accident.
 *
 * The second is that `seq` is monotonic. It is the client's dedupe key (plan §6.16),
 * so a repeat or a gap-free-but-out-of-order stamp is a client that silently drops a
 * message. It is asserted across throwing listeners, across zero listeners, and per
 * instance.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy, so the jsr import declared in `deno.json` cannot
 * resolve. `node:assert` is built into the runtime and needs no fetch. (Same
 * constraint `paths.test.ts` documents.)
 */
import { deepStrictEqual, ok, strictEqual } from "node:assert";
import { Bus } from "./bus.ts";
import type { Bus as BusPort } from "./types.ts";
import type { BoughEvent, EventInput } from "./schema/events.ts";

/** A minimal well-formed event. The bus does not inspect payloads. */
function delta(text: string, sessionId = "s1"): EventInput {
  return { type: "message.delta", sessionId, data: { messageId: "m1", delta: text } };
}

/** A bus whose listener errors are collected instead of logged, so runs stay quiet. */
function quietBus(now?: () => number) {
  const errors: unknown[] = [];
  const bus = new Bus({ now, onListenerError: (err) => errors.push(err) });
  return { bus, errors };
}

// ---- invariant 6: a throwing listener must not break fan-out ----------------

Deno.test("a throwing listener does not prevent later listeners from receiving", () => {
  const { bus, errors } = quietBus();
  const seen: string[] = [];

  bus.subscribe(() => seen.push("first"));
  bus.subscribe(() => {
    throw new Error("this SSE connection is gone");
  });
  bus.subscribe(() => seen.push("third"));
  bus.subscribe(() => {
    throw new Error("so is this one");
  });
  bus.subscribe(() => seen.push("fifth"));

  bus.publish(delta("x"));

  // The two healthy listeners *after* the first thrower still ran, in order.
  deepStrictEqual(seen, ["first", "third", "fifth"]);
  strictEqual(errors.length, 2, "both throws were reported, not swallowed silently");
  strictEqual((errors[0] as Error).message, "this SSE connection is gone");
});

Deno.test("publish itself never throws for a listener's reason", () => {
  const { bus } = quietBus();
  bus.subscribe(() => {
    throw new Error("boom");
  });
  // Callers persist and then publish; an emit path that could throw would need an
  // error branch at every call site.
  const event = bus.publish(delta("x"));
  strictEqual(event.seq, 1);
});

Deno.test("a listener that throws a non-Error, or a reporter that throws, is still isolated", () => {
  const seen: string[] = [];
  const bus = new Bus({
    onListenerError: () => {
      throw new Error("the reporter is broken too");
    },
  });
  bus.subscribe(() => {
    throw "a string, not an Error";
  });
  bus.subscribe(() => seen.push("survivor"));

  bus.publish(delta("x"));
  deepStrictEqual(seen, ["survivor"]);
});

Deno.test("the default error reporter logs and does not rethrow", () => {
  const bus = new Bus();
  const logged: unknown[][] = [];
  const original = console.error;
  console.error = (...args: unknown[]) => void logged.push(args);
  try {
    const seen: string[] = [];
    bus.subscribe(() => {
      throw new Error("boom");
    });
    bus.subscribe(() => seen.push("survivor"));
    bus.publish(delta("x"));
    deepStrictEqual(seen, ["survivor"]);
    strictEqual(logged.length, 1);
    strictEqual(logged[0][0], "bus listener threw:");
  } finally {
    console.error = original;
  }
});

// ---- seq is monotonic ------------------------------------------------------

Deno.test("seq is monotonic: starts at 1 and increments by exactly one per publish", () => {
  const { bus } = quietBus();
  const seqs: number[] = [];
  bus.subscribe((e) => seqs.push(e.seq));

  for (let i = 0; i < 50; i++) bus.publish(delta(`d${i}`));

  strictEqual(seqs.length, 50);
  deepStrictEqual(seqs, Array.from({ length: 50 }, (_, i) => i + 1));
  for (let i = 1; i < seqs.length; i++) {
    ok(seqs[i] > seqs[i - 1], `seq must strictly increase (${seqs[i - 1]} -> ${seqs[i]})`);
  }
});

Deno.test("seq advances even with no listeners, and even when every listener throws", () => {
  const { bus, errors } = quietBus();

  // Nothing subscribed: the counter is a property of the bus, not of delivery.
  strictEqual(bus.publish(delta("a")).seq, 1);
  strictEqual(bus.publish(delta("b")).seq, 2);

  bus.subscribe(() => {
    throw new Error("x");
  });
  strictEqual(bus.publish(delta("c")).seq, 3);
  strictEqual(bus.publish(delta("d")).seq, 4);
  strictEqual(errors.length, 2);
});

Deno.test("every subscriber sees the same seq for one event, and no repeats across events", () => {
  const { bus } = quietBus();
  const a: number[] = [], b: number[] = [];
  bus.subscribe((e) => a.push(e.seq));
  bus.subscribe((e) => b.push(e.seq));

  bus.publish(delta("1"));
  bus.publish(delta("2"));
  bus.publish(delta("3"));

  deepStrictEqual(a, [1, 2, 3]);
  deepStrictEqual(b, a, "one seq per event, not one per delivery");
  strictEqual(new Set(a).size, a.length, "seq is a dedupe key — it must never repeat");
});

Deno.test("seq is per instance — there is no shared global counter", () => {
  // Two buses in one process must not interleave counters; this is also what makes
  // each test above independent.
  const one = new Bus(), two = new Bus();
  strictEqual(one.publish(delta("a")).seq, 1);
  strictEqual(one.publish(delta("b")).seq, 2);
  strictEqual(two.publish(delta("a")).seq, 1);
});

// ---- stamping --------------------------------------------------------------

Deno.test("publish stamps ts from the injected clock and returns the stamped event", () => {
  let t = 1000;
  const { bus } = quietBus(() => (t += 5));
  const received: BoughEvent[] = [];
  bus.subscribe((e) => received.push(e));

  const returned = bus.publish(delta("hello"));

  strictEqual(returned.ts, 1005);
  strictEqual(returned.seq, 1);
  strictEqual(returned.type, "message.delta");
  strictEqual(returned.sessionId, "s1");
  deepStrictEqual(returned.data, { messageId: "m1", delta: "hello" });
  strictEqual(received.length, 1);
  strictEqual(received[0], returned, "listeners get the object publish returns");

  strictEqual(bus.publish(delta("again")).ts, 1010);
});

Deno.test("publish does not mutate its input", () => {
  const { bus } = quietBus();
  const input = delta("x");
  bus.publish(input);
  ok(!("seq" in input), "the stamp goes on a fresh object");
  ok(!("ts" in input), "the stamp goes on a fresh object");
});

Deno.test("delivery is synchronous — no microtask hop", () => {
  const { bus } = quietBus();
  let delivered = false;
  bus.subscribe(() => {
    delivered = true;
  });
  bus.publish(delta("x"));
  // If this were deferred, two emits could reach a client out of seq order.
  ok(delivered, "the listener ran before publish returned");
});

// ---- subscribe / unsubscribe ----------------------------------------------

Deno.test("subscribe returns an unsubscribe thunk, and size tracks live listeners", () => {
  const { bus } = quietBus();
  const seen: string[] = [];
  strictEqual(bus.size, 0);

  const off = bus.subscribe((e) => seen.push(`a:${e.seq}`));
  bus.subscribe((e) => seen.push(`b:${e.seq}`));
  strictEqual(bus.size, 2);

  bus.publish(delta("1"));
  off();
  strictEqual(bus.size, 1);
  bus.publish(delta("2"));

  deepStrictEqual(seen, ["a:1", "b:1", "b:2"]);
});

Deno.test("unsubscribing twice is a no-op, and N connect/disconnect cycles leak nothing", () => {
  const { bus } = quietBus();
  const off = bus.subscribe(() => {});
  off();
  off();
  strictEqual(bus.size, 0);

  for (let i = 0; i < 100; i++) bus.subscribe(() => {})();
  strictEqual(bus.size, 0, "the SSE endpoint's cancel path depends on this");
});

Deno.test("a listener may unsubscribe itself mid-fan-out without affecting the rest", () => {
  const { bus } = quietBus();
  const seen: string[] = [];

  const off = bus.subscribe((e) => {
    seen.push(`once:${e.seq}`);
    off();
  });
  bus.subscribe((e) => seen.push(`always:${e.seq}`));

  bus.publish(delta("1"));
  bus.publish(delta("2"));

  deepStrictEqual(seen, ["once:1", "always:1", "always:2"]);
  strictEqual(bus.size, 1);
});

Deno.test("a listener unsubscribed by an earlier listener does not receive that event", () => {
  // Iteration is over the live set: an unsubscribe is a closed connection, so the
  // safe direction is to skip it rather than deliver and swallow the error.
  const { bus } = quietBus();
  const seen: string[] = [];
  let dropSecond = () => {};

  bus.subscribe(() => {
    seen.push("first");
    dropSecond();
  });
  dropSecond = bus.subscribe(() => seen.push("second"));
  bus.subscribe(() => seen.push("third"));

  bus.publish(delta("1"));
  deepStrictEqual(seen, ["first", "third"]);
  strictEqual(bus.size, 2);
});

// ---- the port --------------------------------------------------------------

Deno.test("Bus satisfies the injected port in types.ts", () => {
  // The whole tree depends on the port, not on this class. If the shapes drift,
  // this assignment stops compiling — which is the point of the test.
  const port: BusPort = new Bus();
  const stamped = port.publish(delta("x"));
  strictEqual(stamped.seq, 1);
  strictEqual(typeof stamped.ts, "number");
  strictEqual(port.size, 0);
  const off = port.subscribe(() => {});
  strictEqual(port.size, 1);
  off();
  strictEqual(port.size, 0);
});

Deno.test("an unknown event name is a compile error, not a runtime one", () => {
  const { bus } = quietBus();
  // The bus does not validate payloads — `EventInput` is a compile-time contract,
  // and the envelope is only parsed where a client reads it off a socket. So the
  // guard against an invented event name has to be the typechecker.
  // @ts-expect-error — the event-name set is closed in schema/events.ts.
  const stamped = bus.publish({ type: "message.invented", data: {} });
  strictEqual(stamped.seq, 1, "it still publishes at runtime; `deno check` is the gate");
});
