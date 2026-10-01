import { afterEach, beforeEach, expect, test } from "bun:test";
import { subscribe, type Event as SessionEvent } from "../src/api";

// Browser primitives are shared across these simulated tabs. The lock
// stays held until its callback settles, including during SSE reconnects.
class Stream {
  static all: Stream[] = [];
  onmessage: ((m: { data: string }) => void) | null = null;
  closed = false;
  constructor(public url: string) { Stream.all.push(this); }
  close() { this.closed = true; }
  send(data: string) { if (!this.closed) this.onmessage?.({ data }); }
}
class Channel {
  static all: Channel[] = [];
  static sent: any[] = [];
  onmessage: ((m: { data: unknown }) => void) | null = null;
  closed = false;
  constructor(public name: string) { Channel.all.push(this); }
  close() { this.closed = true; }
  postMessage(data: unknown) {
    Channel.sent.push(structuredClone(data));
    for (const other of Channel.all) {
      if (other !== this && other.name === this.name && !other.closed) {
        const copy = structuredClone(data);
        queueMicrotask(() => { if (!other.closed) other.onmessage?.({ data: copy }); });
      }
    }
  }
}
class Locks {
  tails = new Map<string, Promise<unknown>>();
  request(name: string, { signal }: { signal: AbortSignal }, callback: () => Promise<void>) {
    const wait = this.tails.get(name) ?? Promise.resolve();
    const next = wait.then(() => {
      if (signal.aborted) throw new DOMException("Aborted", "AbortError");
      return callback();
    });
    this.tails.set(name, next.catch(() => {}));
    return next;
  }
}
const originals = new Map<string, PropertyDescriptor | undefined>();
const stops: (() => void)[] = [];
function global(name: string, value: unknown) {
  Object.defineProperty(globalThis, name, { configurable: true, value });
}
const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const live = () => Stream.all.filter((s) => !s.closed);
function listen(id = "same", listener?: (ev: SessionEvent) => void) {
  const events: SessionEvent[] = [];
  const lifecycle = new EventTarget();
  global("document", lifecycle);
  const stop = subscribe(id, listener ?? ((ev) => events.push(ev)));
  stops.push(stop);
  return { events, stop, lifecycle };
}
const event = { kind: "change", seq: 1 } as SessionEvent;

beforeEach(() => {
  for (const name of ["navigator", "BroadcastChannel", "EventSource", "document", "crypto"]) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
  Stream.all = []; Channel.all = []; Channel.sent = [];
  global("navigator", { locks: new Locks() });
  global("BroadcastChannel", Channel);
  global("EventSource", Stream);
});
afterEach(async () => {
  for (const stop of stops.splice(0)) stop();
  await tick();
  for (const [name, descriptor] of originals) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else Reflect.deleteProperty(globalThis, name);
  }
});

test("seven tabs, five on one session, use three streams and all receive events", async () => {
  const tabs = Array.from({ length: 5 }, () => listen());
  const other = listen("other");
  const third = listen("third");
  await tick();
  expect(live()).toHaveLength(3);
  live().find((s) => s.url === "/api/sessions/same/events")!.send(JSON.stringify(event));
  await tick();
  for (const tab of tabs) expect(tab.events).toEqual([event]);
  expect(other.events).toEqual([]);
  expect(third.events).toEqual([]);
});

test("closing the leader closes its stream before a follower takes over", async () => {
  const first = listen();
  const second = listen();
  const third = listen();
  await tick();
  const source = live()[0];
  first.stop();
  expect(source.closed).toBe(true);
  await tick();
  expect(live()).toHaveLength(1);
  expect(Stream.all).toHaveLength(2);
  live()[0].send(JSON.stringify(event));
  await tick();
  expect(first.events).toEqual([]);
  expect(second.events).toEqual([event]);
  expect(third.events).toEqual([event]);
});

test("leaving followers never acquire a stream, even when the leader leaves", async () => {
  const first = listen();
  const second = listen();
  const third = listen();
  await tick();
  second.stop();
  first.stop();
  await tick();
  expect(live()).toHaveLength(1);
  expect(Stream.all).toHaveLength(2);
  third.stop();
  await tick();
  expect(live()).toHaveLength(0);
  expect(Channel.all.every((c) => c.closed)).toBe(true);
});

test("unsubscribe before lock acquisition opens nothing", async () => {
  listen().stop();
  await tick();
  expect(Stream.all).toHaveLength(0);
  expect(Channel.all.every((c) => c.closed)).toBe(true);
});

test("bad frames and throwing listeners do not interrupt fanout", async () => {
  listen("same", () => { throw new Error("listener"); });
  const follower = listen();
  await tick();
  live()[0].send("not json");
  live()[0].send(JSON.stringify(event));
  await tick();
  expect(follower.events).toEqual([event]);
  expect(live()).toHaveLength(1);
});

test("a late follower replays recorded state before subsequent changes", async () => {
  listen();
  await tick();
  live()[0].send(JSON.stringify(event));
  const follower = listen();
  await tick();
  const next = { ...event, seq: 2 };
  live()[0].send(JSON.stringify(next));
  await tick();
  expect(follower.events).toEqual([event, next]);
  expect(Stream.all).toHaveLength(1);
});

for (const missing of ["locks", "BroadcastChannel", "navigator", "crypto", "randomUUID"]) {
  test(`direct stream fallback without ${missing}`, async () => {
    if (missing === "locks") global("navigator", {});
    else if (missing === "randomUUID") global("crypto", {});
    else global(missing, undefined);
    const tab = listen();
    expect(live()).toHaveLength(1);
    live()[0].send(JSON.stringify(event));
    expect(tab.events).toEqual([event]);
    tab.stop();
    expect(live()).toHaveLength(0);
  });
}
for (const failure of ["channel", "lock rejection", "lock throw"]) {
  test(`direct stream fallback on ${failure}`, async () => {
    if (failure === "channel") global("BroadcastChannel", class { constructor() { throw new Error("denied"); } });
    else global("navigator", { locks: { request() {
      if (failure === "lock throw") throw new Error("denied");
      return Promise.reject(new Error("denied"));
    } } });
    const tab = listen();
    await tick();
    expect(live()).toHaveLength(1);
    live()[0].send(JSON.stringify(event));
    expect(tab.events).toEqual([event]);
    tab.stop();
    expect(live()).toHaveLength(0);
    expect(Channel.all.every((c) => c.closed)).toBe(true);
  });
}

test("late replay restores a running call/activity without replaying deltas to existing tabs", async () => {
  const owner = listen();
  await tick();
  const activity = { ...event, kind: "activity", text: "Working" };
  const call = { ...event, seq: 2, kind: "call", extra: { id: "tool-1", phase: "start" } };
  const delta = { ...event, seq: 0, kind: "call-delta", text: "partial" };
  for (const ev of [activity, call, delta]) live()[0].send(JSON.stringify(ev));
  const follower = listen();
  // Race a live record with the queued hello. It must appear once,
  // ordered after the replay, rather than be lost or replayed twice.
  const next = { ...event, seq: 3 };
  live()[0].send(JSON.stringify(next));
  await tick();
  expect(follower.events).toEqual([activity, call, next]);
  expect(owner.events).toEqual([activity, call, delta, next]);
  live()[0].send(JSON.stringify(delta));
  await tick();
  expect(follower.events).toEqual([activity, call, next, delta]);
});

test("replay stays bounded and SSE reconnect duplicates do not evict earlier state", async () => {
  listen();
  await tick();
  for (let seq = 1; seq <= 502; seq++) live()[0].send(JSON.stringify({ ...event, seq }));
  for (let seq = 3; seq <= 502; seq++) live()[0].send(JSON.stringify({ ...event, seq }));
  const follower = listen();
  await tick();
  expect(follower.events).toHaveLength(500);
  expect(follower.events.map((ev) => ev.seq)).toEqual(Array.from({ length: 500 }, (_, i) => i + 3));
});

test("a restarted server sequence replaces the old replay", async () => {
  listen();
  await tick();
  live()[0].send(JSON.stringify({ ...event, seq: 5, at: "old" }));
  const restarted = { ...event, at: "new" };
  live()[0].send(JSON.stringify(restarted));
  const follower = listen();
  await tick();
  expect(follower.events).toEqual([restarted]);
});

test("freezing an owner hands off and resuming rejoins without another stream", async () => {
  const owner = listen();
  const follower = listen();
  await tick();
  owner.lifecycle.dispatchEvent(new Event("freeze"));
  await tick();
  expect(live()).toHaveLength(1);
  expect(Stream.all).toHaveLength(2);
  live()[0].send(JSON.stringify(event));
  await tick();
  expect(owner.events).toEqual([]);
  expect(follower.events).toEqual([event]);
  owner.lifecycle.dispatchEvent(new Event("resume"));
  await tick();
  expect(live()).toHaveLength(1);
  expect(owner.events).toEqual([event]);
  expect(follower.events).toEqual([event]);
});

test("a frozen follower cancels its wait and unsubscribe prevents resume", async () => {
  const owner = listen();
  const follower = listen();
  await tick();
  follower.lifecycle.dispatchEvent(new Event("freeze"));
  owner.stop();
  await tick();
  expect(live()).toHaveLength(0);
  follower.stop();
  follower.lifecycle.dispatchEvent(new Event("resume"));
  await tick();
  expect(Stream.all).toHaveLength(1);
  expect(Channel.all.every((c) => c.closed)).toBe(true);
});


test("a new leader rejects queued messages from the previous owner", async () => {
  const owner = listen();
  const follower = listen();
  await tick();
  const previous = Channel.sent.find((message) => message.kind === "ready").owner;
  owner.stop();
  await tick();
  Channel.all[1].onmessage?.({ data: { kind: "event", owner: previous, event } });
  expect(follower.events).toEqual([]);
  live()[0].send(JSON.stringify(event));
  expect(follower.events).toEqual([event]);
});
