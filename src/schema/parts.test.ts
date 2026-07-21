import { assertEquals } from "jsr:@std/assert@1";
import {
  AskQuestion,
  BoughEvent,
  Message,
  type Message as TMessage,
  NetRequest,
  Part,
  Session,
  type Session as TSession,
} from "./parts.ts";

// Round-trip the exact wire shapes clients pin. If the schema drifts, these fail.

Deno.test("Part union round-trips all five kinds", () => {
  const parts: unknown[] = [
    { type: "text", text: "hi" },
    { type: "reasoning", text: "thinking" },
    { type: "tool_call", id: "c1", name: "read", input: { path: "/x" } },
    { type: "tool_result", callId: "c1", output: "done", isError: false },
    {
      type: "image",
      path: "/home/u/.bough/attachments/abc.png",
      mediaType: "image/png",
      name: "shot.png",
      size: 12345,
    },
    {
      type: "ask",
      id: "q1",
      question: "Which env?",
      options: ["dev", "prod"],
      status: "answered",
      answer: "prod",
    },
  ];
  for (const p of parts) assertEquals(Part.parse(p), p);
});

Deno.test("AskQuestion round-trips (with and without options/answer)", () => {
  const pending: unknown = {
    id: "q1",
    sessionId: "s1",
    messageId: "m1",
    question: "Which env?",
    options: ["dev", "prod"],
    status: "pending",
    ts: 1,
  };
  assertEquals(AskQuestion.parse(pending), pending);
  const declined: unknown = {
    id: "q2",
    sessionId: "s1",
    messageId: "m1",
    question: "Proceed?",
    status: "declined",
    ts: 2,
  };
  assertEquals(AskQuestion.parse(declined), declined);
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
  const note: TMessage = {
    id: "m2",
    sessionId: "s1",
    role: "system",
    parts: [{ type: "text", text: "[subagent finished] …" }],
    pending: false,
    createdAt: 124,
  };
  assertEquals(Message.parse(note), note);
});

Deno.test("Session round-trips with nullable parent", () => {
  const s: TSession = { id: "s1", parentId: null, title: "root", kind: "root", createdAt: 1 };
  assertEquals(Session.parse(s), s);
  const fork: TSession = { id: "s2", parentId: "s1", title: "fork", kind: "fork", createdAt: 2 };
  assertEquals(Session.parse(fork), fork);
  const sub: TSession = {
    id: "s3",
    parentId: null,
    title: "subagent",
    kind: "subagent",
    createdAt: 3,
    originId: "s1",
    originMessageId: "m1",
    contextTokens: 12000,
    cachedTokens: 11000,
    lastLlmAt: 4,
  };
  assertEquals(Session.parse(sub), sub);
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
