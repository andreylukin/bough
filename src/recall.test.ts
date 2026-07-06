import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { embeddableText, recall } from "./recall.ts";
import type { Message, Part, Session } from "./schema/parts.ts";

/**
 * A deterministic fake embedder: maps text onto a 4-dim "topic" vector by keyword,
 * so cosine ranking is predictable without a model.
 */
const TOPICS = ["auth", "proxy", "tests", "deploy"];
function fakeEmbed(texts: string[]): Promise<number[][]> {
  return Promise.resolve(texts.map((t) => {
    const v = TOPICS.map((topic) => (t.toLowerCase().includes(topic) ? 1 : 0.01));
    return v;
  }));
}

function seed(db: Db, id: string, title: string, text: string): void {
  const session: Session = { id, title, kind: "root", createdAt: 1, parentId: null };
  db.createSession(session);
  const m: Message = {
    id: `m-${id}`,
    sessionId: id,
    role: "user",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: 1,
  };
  db.createMessage(m);
}

Deno.test("recall indexes lazily and ranks by topic similarity", async () => {
  const db = new Db(":memory:");
  seed(db, "s1", "fix login", "the auth token expires too early in the login flow");
  seed(db, "s2", "proxy work", "rewrite the proxy gate to hold writes");
  seed(db, "s3", "ci", "the tests are flaky on ci");

  const first = await recall(db, "how did we fix the auth expiry?", 2, fakeEmbed);
  assertEquals(first.indexed, 3); // lazy index caught all three on the first call
  assertEquals(first.hits[0].sessionId, "s1");
  assertEquals(first.hits[0].title, "fix login");
  assertStringIncludes(first.hits[0].snippet, "auth token expires");
  assertEquals(first.hits.length, 2);
  assertEquals(first.hits[0].score > first.hits[1].score, true);

  // Second call: nothing new to index.
  const second = await recall(db, "proxy gate holds", 1, fakeEmbed);
  assertEquals(second.indexed, 0);
  assertEquals(second.hits[0].sessionId, "s2");
});

Deno.test("messages with no text get marked and never re-queued", async () => {
  const db = new Db(":memory:");
  const session: Session = { id: "s1", title: "t", kind: "root", createdAt: 1, parentId: null };
  db.createSession(session);
  db.createMessage({
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [{ type: "tool_call", id: "c1", name: "run_steps", input: {} } as Part],
    pending: false,
    createdAt: 1,
  });

  const first = await recall(db, "anything", 5, fakeEmbed);
  assertEquals(first.indexed, 0); // textless — marked, not embedded
  assertEquals(first.hits.length, 0);
  assertEquals(db.messagesToEmbed(10).length, 0); // not re-queued
});

Deno.test("embeddableText keeps prose and drops tool plumbing", () => {
  const parts: Part[] = [
    { type: "text", text: "let me fix it" },
    { type: "tool_call", id: "c", name: "run_steps", input: { code: "secret plumbing" } } as Part,
    { type: "reasoning", text: "thinking about auth" } as Part,
  ];
  const text = embeddableText(parts);
  assertStringIncludes(text, "let me fix it");
  assertStringIncludes(text, "thinking about auth");
  assertEquals(text.includes("secret plumbing"), false);
});

Deno.test("pending messages are not indexed until they finish", async () => {
  const db = new Db(":memory:");
  seed(db, "s1", "done", "finished auth message");
  db.createMessage({
    id: "m-live",
    sessionId: "s1",
    role: "supervisor",
    parts: [{ type: "text", text: "streaming auth right now" }],
    pending: true,
    createdAt: 2,
  });
  const r = await recall(db, "auth", 5, fakeEmbed);
  assertEquals(r.indexed, 1); // only the finished message
  assertEquals(r.hits.length, 1);
  assertEquals(r.hits[0].messageId, "m-s1");
});
