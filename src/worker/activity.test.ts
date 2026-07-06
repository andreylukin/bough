import { assertEquals } from "jsr:@std/assert@1";
import { Bus } from "../bus.ts";
import type { BoughEvent } from "../schema/parts.ts";
import { watchActivity } from "./activity.ts";

function partEvent(sessionId: string, name: string, input: unknown): BoughEvent {
  return {
    type: "message.part",
    sessionId,
    seq: 0,
    data: { messageId: "m1", part: { type: "tool_call", id: "t1", name, input } },
  } as BoughEvent;
}

Deno.test("watchActivity blurbs run_steps programs as session.activity", async () => {
  const bus = new Bus();
  const activity: BoughEvent[] = [];
  bus.subscribe((e) => {
    if (e.type === "session.activity") activity.push(e);
  });
  const seen: string[] = [];
  const stop = watchActivity(bus, (code) => {
    seen.push(code);
    return Promise.resolve("running the tests");
  });

  bus.publish(partEvent("s1", "run_steps", { code: "await bash('deno test')" }));
  await new Promise((r) => setTimeout(r, 0));

  assertEquals(seen, ["await bash('deno test')"]);
  assertEquals(activity.length, 1);
  assertEquals(activity[0].sessionId, "s1");
  assertEquals((activity[0].data as { text: string }).text, "running the tests");
  stop();
});

Deno.test("watchActivity ignores other tools and drops rounds while one is in flight", async () => {
  const bus = new Bus();
  const activity: BoughEvent[] = [];
  bus.subscribe((e) => {
    if (e.type === "session.activity") activity.push(e);
  });
  const resolvers: ((v: string | null) => void)[] = [];
  const stop = watchActivity(bus, () =>
    new Promise((r) => {
      resolvers.push(r);
    }));

  bus.publish(partEvent("s1", "other_tool", { code: "x" }));
  bus.publish(partEvent("s1", "run_steps", { code: "round 1" }));
  bus.publish(partEvent("s1", "run_steps", { code: "round 2 (dropped)" }));
  // A different session is not blocked by s1's in-flight blurb.
  bus.publish(partEvent("s2", "run_steps", { code: "round A" }));
  assertEquals(resolvers.length, 2); // s1 round 1 + s2 round A; s1 round 2 dropped

  resolvers[0]("doing things"); // s1
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(activity.length, 1); // s2's is still pending; s1 emitted once
  assertEquals(activity[0].sessionId, "s1");

  resolvers[1](null); // s2's blurb fails soft — no event
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(activity.length, 1);
  stop();
});

Deno.test("watchActivity blurbs failures never throw into the bus", async () => {
  const bus = new Bus();
  const stop = watchActivity(bus, () => Promise.reject(new Error("worker down")));
  bus.publish(partEvent("s1", "run_steps", { code: "x" }));
  await new Promise((r) => setTimeout(r, 0));
  stop();
});
