import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { createHandler, type AppCtx } from "./app.ts";
import type { BoughEvent, Message, Role, Session } from "../schema/parts.ts";
import type { LlmClient, LlmParams } from "../supervisor/llm.ts";

// A fake LLM that returns a fixed summary and records the prompt it was given.
function fakeLlm(summary: string): { client: LlmClient; prompts: string[] } {
  const prompts: string[] = [];
  return {
    prompts,
    client: {
      run(params: LlmParams) {
        const block = params.messages[0].content[0];
        prompts.push(block.type === "text" ? block.text : "");
        return Promise.resolve({ content: [{ type: "text" as const, text: summary }], stopReason: "end_turn" });
      },
    },
  };
}

function ctx(llm?: LlmClient): AppCtx {
  const bus = new Bus();
  return { db: new Db(":memory:"), bus, llm };
}

function seedMessage(db: Db, sessionId: string, id: string, role: Role, text: string, createdAt: number) {
  const m: Message = { id, sessionId, role, parts: [{ type: "text", text }], pending: false, createdAt };
  db.createMessage(m);
}

// A root session with 5 messages: hello / working / do X / did X / thanks.
function seedThread(db: Db): Session {
  const s: Session = { id: "S", parentId: null, title: "root", kind: "root", createdAt: 1 };
  db.createSession(s);
  seedMessage(db, "S", "m1", "user", "hello", 10);
  seedMessage(db, "S", "m2", "supervisor", "working on it", 11);
  seedMessage(db, "S", "m3", "user", "do X", 12);
  seedMessage(db, "S", "m4", "supervisor", "did X", 13);
  seedMessage(db, "S", "m5", "user", "thanks", 14);
  return s;
}

const post = (path: string, body: unknown) =>
  new Request("http://x" + path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
const get = (path: string) => new Request("http://x" + path);

Deno.test("compact: mid-thread span replaced by summary; original untouched", async () => {
  const fake = fakeLlm("SUMMARY of the middle");
  const c = ctx(fake.client);
  seedThread(c.db);
  const h = createHandler(c);

  const events: BoughEvent[] = [];
  c.bus.subscribe((e) => events.push(e));

  const res = await h(post("/sessions/S/compact", { fromMessageId: "m2", toMessageId: "m4" }));
  assertEquals(res.status, 200);
  const { session } = await res.json() as { session: Session };
  assertEquals(session.kind, "compaction");
  assertEquals(session.parentId, null); // sibling of the root target
  assertEquals(session.title, "compacted · 3 turns");
  // Lineage: compacted session + span-end message (POST response).
  assertEquals(session.originId, "S");
  assertEquals(session.originMessageId, "m4");

  // Only the span (m2..m4) was fed to the LLM — not the pre/post-span messages.
  assert(fake.prompts[0].includes("working on it"));
  assert(fake.prompts[0].includes("did X"));
  assert(!fake.prompts[0].includes("hello"));
  assert(!fake.prompts[0].includes("thanks"));

  // The compaction branch's thread: pre-span, summary, post-span.
  const branch = await (await h(get(`/sessions/${session.id}`))).json() as {
    session: Session;
    thread: Message[];
  };
  assertEquals(branch.session.originId, "S"); // lineage survives the DB round-trip
  assertEquals(branch.session.originMessageId, "m4");
  assertEquals(
    branch.thread.map((m) => [m.role, m.parts.map((p) => (p.type === "text" ? p.text : "")).join("")]),
    [["user", "hello"], ["supervisor", "SUMMARY of the middle"], ["user", "thanks"]],
  );

  // Original session is unchanged.
  const orig = await (await h(get("/sessions/S"))).json() as { thread: Message[] };
  assertEquals(orig.thread.map((m) => m.id), ["m1", "m2", "m3", "m4", "m5"]);

  // Events for the UI: one session.created (carrying lineage) + three message.started.
  const created = events.filter((e) => e.type === "session.created");
  assertEquals(created.length, 1);
  assertEquals((created[0].data as Session).originId, "S");
  assertEquals((created[0].data as Session).originMessageId, "m4");
  assertEquals(events.filter((e) => e.type === "message.started" && e.sessionId === session.id).length, 3);
  c.db.close();
});

Deno.test("compact: single-message span (from == to) is allowed", async () => {
  const c = ctx(fakeLlm("S").client);
  seedThread(c.db);
  const h = createHandler(c);
  const res = await h(post("/sessions/S/compact", { fromMessageId: "m3", toMessageId: "m3" }));
  assertEquals(res.status, 200);
  const { session } = await res.json() as { session: Session };
  assertEquals(session.title, "compacted · 1 turns");
  c.db.close();
});

Deno.test("compact: invalid spans and unknown ids error cleanly", async () => {
  const c = ctx(fakeLlm("S").client);
  seedThread(c.db);
  const h = createHandler(c);

  // from after to
  assertEquals((await h(post("/sessions/S/compact", { fromMessageId: "m4", toMessageId: "m2" }))).status, 400);
  // unknown message id
  assertEquals((await h(post("/sessions/S/compact", { fromMessageId: "m2", toMessageId: "zzz" }))).status, 400);
  // unknown session
  assertEquals((await h(post("/sessions/missing/compact", { fromMessageId: "m2", toMessageId: "m4" }))).status, 404);
  // malformed body
  assertEquals((await h(post("/sessions/S/compact", { nope: 1 }))).status, 400);
  c.db.close();
});

Deno.test("compact: a forked child span compacts as a sibling under the shared parent", async () => {
  const c = ctx(fakeLlm("CHILD-SUMMARY").client);
  // root R (msg r1) → child S (msg s1,s2). Compact S's own span [s1..s2].
  c.db.createSession({ id: "R", parentId: null, title: "root", kind: "root", createdAt: 1 });
  seedMessage(c.db, "R", "r1", "user", "root msg", 10);
  c.db.createSession({ id: "S", parentId: "R", title: "fork", kind: "fork", createdAt: 2 });
  seedMessage(c.db, "S", "s1", "user", "child msg 1", 20);
  seedMessage(c.db, "S", "s2", "supervisor", "child msg 2", 21);
  const h = createHandler(c);

  const { session } = await (await h(post("/sessions/S/compact", { fromMessageId: "s1", toMessageId: "s2" })))
    .json() as { session: Session };
  assertEquals(session.parentId, "R"); // sibling of S, under the shared parent R

  // Thread inherits R's message, then the summary (span fully replaced).
  const branch = await (await h(get(`/sessions/${session.id}`))).json() as { thread: Message[] };
  assertEquals(
    branch.thread.map((m) => m.parts.map((p) => (p.type === "text" ? p.text : "")).join("")),
    ["root msg", "CHILD-SUMMARY"],
  );
  c.db.close();
});
