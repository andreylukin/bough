import { assertEquals } from "jsr:@std/assert@1";
import { BoughEvent, Message, NetRequest, Part, Session } from "./parts.ts";
import type { Message as TMessage, Session as TSession } from "./parts.ts";

// Round-trip the exact shapes web/src/types.ts pins. If the mirror drifts, these fail.

Deno.test("Part union round-trips all four kinds", () => {
  const parts: unknown[] = [
    { type: "text", text: "hi" },
    { type: "reasoning", text: "thinking" },
    { type: "tool_call", id: "c1", name: "read", input: { path: "/x" } },
    { type: "tool_result", callId: "c1", output: "done", isError: false },
  ];
  for (const p of parts) assertEquals(Part.parse(p), p);
});

Deno.test("Message round-trips", () => {
  const m: TMessage = {
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [{ type: "text", text: "hello" }],
    pending: true,
    createdAt: 123,
  };
  assertEquals(Message.parse(m), m);
});

Deno.test("Session round-trips with nullable parent", () => {
  const s: TSession = { id: "s1", parentId: null, title: "root", kind: "root", createdAt: 1 };
  assertEquals(Session.parse(s), s);
  const fork: TSession = { id: "s2", parentId: "s1", title: "fork", kind: "fork", createdAt: 2 };
  assertEquals(Session.parse(fork), fork);
});

Deno.test("BoughEvent + NetRequest round-trip", () => {
  const ev = { type: "net.request", sessionId: "s1", seq: 3, ts: 9, data: { any: true } };
  assertEquals(BoughEvent.parse(ev), ev);
  const r = { id: "r1", host: "api.github.com", action: "GET /repos", verdict: "allowed", ts: 5 };
  assertEquals(NetRequest.parse(r), r);
});

Deno.test("bad Part is rejected", () => {
  assertEquals(Part.safeParse({ type: "text" }).success, false);
  assertEquals(Part.safeParse({ type: "nope" }).success, false);
});
