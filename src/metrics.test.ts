import { assertEquals } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { sessionMetrics } from "./metrics.ts";

function seed(db: Db) {
  db.createSession({ id: "S", parentId: null, title: "s", kind: "root", createdAt: 0 });
  // Turn 1: user asks at 1000, first output at 1800, done at 5000, one tool call.
  db.createMessage({
    id: "u1",
    sessionId: "S",
    role: "user",
    parts: [{ type: "text", text: "do it" }],
    pending: false,
    createdAt: 1000,
  });
  db.createMessage({
    id: "s1",
    sessionId: "S",
    role: "supervisor",
    parts: [
      { type: "text", text: "on it" },
      { type: "tool_call", id: "c1", name: "bash", input: {} },
      { type: "tool_result", callId: "c1", output: "ok", isError: false },
    ],
    pending: false,
    createdAt: 1000,
  });
  db.createTurn({
    id: "t1",
    sessionId: "S",
    messageId: "s1",
    status: "done",
    step: "round:1",
    updatedAt: 5000,
    firstOutputAt: null,
  });
  db.setTurnFirstOutput("t1", 1800);
  db.setTurnFirstOutput("t1", 9999); // idempotent — first stamp wins
  // Turn 2: user asks at 6000, interrupted at 7000, nothing streamed.
  db.createMessage({
    id: "u2",
    sessionId: "S",
    role: "user",
    parts: [{ type: "text", text: "again" }],
    pending: false,
    createdAt: 6000,
  });
  db.createMessage({
    id: "s2",
    sessionId: "S",
    role: "supervisor",
    parts: [{ type: "text", text: "⏹ Stopped." }],
    pending: false,
    createdAt: 6000,
  });
  db.createTurn({
    id: "t2",
    sessionId: "S",
    messageId: "s2",
    status: "interrupted",
    step: "round:1",
    updatedAt: 7000,
    firstOutputAt: null,
  });
  // Net events: a resolved human hold + a still-parked hold count as approval
  // prompts; a plain policy allow does not.
  db.recordNetEvent("S", {
    id: "n1",
    host: "api.example.com",
    action: "fetch",
    verdict: "allowed",
    reason: "approved by human",
    ts: 2000,
  });
  db.recordNetEvent("S", {
    id: "n2",
    host: "cdn.example.com",
    action: "fetch",
    verdict: "allowed",
    reason: "host allowed by rule",
    ts: 2100,
  });
  db.recordNetEvent("S", {
    id: "n3",
    host: "held.example.com",
    action: "push",
    verdict: "pending",
    ts: 6500,
  });
}

Deno.test("sessionMetrics derives usability metrics from stored rows", () => {
  const db = new Db(":memory:");
  seed(db);
  const m = sessionMetrics(db, "S");
  assertEquals(m.userTurns, 2);
  assertEquals(m.assistantTurns, 2);
  assertEquals(m.toolCalls, 1);
  assertEquals(m.interrupted, 1);
  assertEquals(m.failed, 0);
  assertEquals(m.approvalPrompts, 2);
  assertEquals(m.firstOutput, { count: 1, medianMs: 800, maxMs: 800 });
  assertEquals(m.turnDuration, { count: 2, medianMs: 4000, maxMs: 4000 });
  assertEquals(m.wallClockMs, 5000);
  db.close();
});

Deno.test("sessionMetrics on an empty session is all zeros", () => {
  const db = new Db(":memory:");
  db.createSession({ id: "E", parentId: null, title: "e", kind: "root", createdAt: 0 });
  const m = sessionMetrics(db, "E");
  assertEquals(m.userTurns, 0);
  assertEquals(m.firstOutput, null);
  assertEquals(m.turnDuration, null);
  assertEquals(m.wallClockMs, 0);
  db.close();
});
