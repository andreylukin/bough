import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { Bus } from "./bus.ts";
import type { AskQuestion } from "./schema/parts.ts";
import { answerAsk, declineAsk, expireAsks, getAsk, pendingAsks, raiseAsk } from "./asks.ts";

function harness() {
  const bus = new Bus();
  const events: AskQuestion[] = [];
  bus.subscribe((e) => {
    if (e.type === "ask.question") events.push(e.data as AskQuestion);
  });
  return { bus, events };
}

Deno.test("raise → answer: resolves with the answer; pending → answered events", async () => {
  const { bus, events } = harness();
  const { record, answer } = raiseAsk(bus, {
    sessionId: "s1",
    messageId: "m1",
    question: "Which env?",
    options: ["dev", "prod"],
  });

  // Raised: registered, listed, announced pending.
  assertEquals(getAsk(record.id)?.question, "Which env?");
  assertEquals(pendingAsks("s1").map((q) => q.id), [record.id]);
  assertEquals(events[0].status, "pending");
  assertEquals(events[0].options, ["dev", "prod"]);

  assertEquals(answerAsk(record.id, "prod"), true);
  assertEquals(await answer, "prod");
  // Settled: gone from the registry, final event on the SAME id.
  assertEquals(getAsk(record.id), undefined);
  assertEquals(pendingAsks().length, 0);
  assertEquals(events[1].id, record.id);
  assertEquals(events[1].status, "answered");
  assertEquals(events[1].answer, "prod");
  // A second settle is a no-op (already gone).
  assertEquals(answerAsk(record.id, "again"), false);
});

Deno.test("raise → decline: rejects with a catchable 'user declined' error", async () => {
  const { bus, events } = harness();
  const { record, answer } = raiseAsk(bus, {
    sessionId: "s1",
    messageId: "m1",
    question: "Proceed?",
  });
  assertEquals(declineAsk(record.id), true);
  const err = await assertRejects(() => answer, Error);
  assertStringIncludes(err.message, "user declined");
  assertStringIncludes(err.message, "Proceed?");
  assertEquals(events[1].status, "declined");
  assertEquals(pendingAsks().length, 0);
});

Deno.test("raise → interrupt (signal abort): rejects and clears the hold", async () => {
  const { bus, events } = harness();
  const controller = new AbortController();
  const { answer } = raiseAsk(
    bus,
    { sessionId: "s1", messageId: "m1", question: "Which?" },
    controller.signal,
  );
  controller.abort();
  const err = await assertRejects(() => answer, Error);
  assertStringIncludes(err.message, "interrupted");
  assertEquals(events[1].status, "interrupted");
  assertEquals(pendingAsks().length, 0);
});

Deno.test("an already-aborted signal rejects immediately", async () => {
  const { bus } = harness();
  const controller = new AbortController();
  controller.abort();
  const { answer } = raiseAsk(
    bus,
    { sessionId: "s1", messageId: "m1", question: "Which?" },
    controller.signal,
  );
  await assertRejects(() => answer, Error, "interrupted");
  assertEquals(pendingAsks().length, 0);
});

Deno.test("expireAsks clears one session's holds and leaves the other's", async () => {
  const { bus } = harness();
  const a = raiseAsk(bus, { sessionId: "sA", messageId: "m1", question: "a?" });
  const b = raiseAsk(bus, { sessionId: "sB", messageId: "m2", question: "b?" });
  assertEquals(expireAsks("sA"), 1);
  await assertRejects(() => a.answer, Error, "interrupted");
  assertEquals(pendingAsks().map((q) => q.id), [b.record.id]);
  // Sweep everything (also leaves the registry clean for other tests).
  assertEquals(expireAsks(), 1);
  await assertRejects(() => b.answer, Error, "interrupted");
});
