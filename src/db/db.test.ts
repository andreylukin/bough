import { assertEquals } from "jsr:@std/assert@1";
import { Db } from "./db.ts";
import type { Message, Session } from "../schema/parts.ts";

function mkDb(): Db {
  return new Db(":memory:");
}

function session(id: string, parentId: string | null, kind: Session["kind"], createdAt: number): Session {
  return { id, parentId, title: id, kind, createdAt };
}

function msg(id: string, sessionId: string, createdAt: number, text: string): Message {
  return {
    id,
    sessionId,
    role: "user",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt,
  };
}

Deno.test("session CRUD + list order (newest first)", () => {
  const db = mkDb();
  db.createSession(session("a", null, "root", 1));
  db.createSession(session("b", null, "root", 2));
  assertEquals(db.getSession("a")?.title, "a");
  assertEquals(db.listSessions().map((s) => s.id), ["b", "a"]);
  db.close();
});

Deno.test("session lineage: origin fields round-trip; absent stays undefined", () => {
  const db = mkDb();
  db.createSession(session("root", null, "root", 1)); // no lineage
  db.createSession({
    id: "fork",
    parentId: null,
    title: "fork",
    kind: "fork",
    createdAt: 2,
    originId: "root",
    originMessageId: "m7",
  });
  assertEquals(db.getSession("root")?.originId, undefined);
  assertEquals(db.getSession("root")?.originMessageId, undefined);
  const f = db.getSession("fork");
  assertEquals(f?.originId, "root");
  assertEquals(f?.originMessageId, "m7");
  db.close();
});

Deno.test("message CRUD preserves parts + pending", () => {
  const db = mkDb();
  db.createSession(session("s", null, "root", 1));
  const m: Message = {
    id: "m1",
    sessionId: "s",
    role: "supervisor",
    parts: [{ type: "reasoning", text: "hmm" }, { type: "text", text: "hi" }],
    pending: true,
    createdAt: 5,
  };
  db.createMessage(m);
  assertEquals(db.messagesFor("s"), [m]);
  db.close();
});

Deno.test("threadFor assembles root→self across parents in order", () => {
  const db = mkDb();
  // root -> mid -> leaf
  db.createSession(session("root", null, "root", 1));
  db.createSession(session("mid", "root", "fork", 2));
  db.createSession(session("leaf", "mid", "fork", 3));

  db.createMessage(msg("r1", "root", 10, "r-one"));
  db.createMessage(msg("r2", "root", 11, "r-two"));
  db.createMessage(msg("m1", "mid", 20, "m-one"));
  db.createMessage(msg("l1", "leaf", 30, "l-one"));

  assertEquals(db.threadFor("leaf").map((m) => m.id), ["r1", "r2", "m1", "l1"]);
  assertEquals(db.threadFor("mid").map((m) => m.id), ["r1", "r2", "m1"]);
  assertEquals(db.threadFor("root").map((m) => m.id), ["r1", "r2"]);
  db.close();
});

Deno.test("ancestorChain is root-first and cycle-safe", () => {
  const db = mkDb();
  db.createSession(session("root", null, "root", 1));
  db.createSession(session("child", "root", "fork", 2));
  assertEquals(db.ancestorChain("child").map((s) => s.id), ["root", "child"]);
  assertEquals(db.ancestorChain("missing"), []);
  db.close();
});

Deno.test("recordNetEvent inserts without throwing (minimal columns)", () => {
  const db = mkDb();
  db.recordNetEvent(undefined, {
    id: "n1",
    host: "api.github.com",
    action: "GET /repos",
    verdict: "allowed",
    ts: 1,
  });
  db.close();
});
