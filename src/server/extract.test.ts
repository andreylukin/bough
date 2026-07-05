import { assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { type AppCtx, createHandler } from "./app.ts";
import type { BoughEvent, Message, Role, Session } from "../schema/parts.ts";

function ctx(): AppCtx {
  return { db: new Db(":memory:"), bus: new Bus() };
}

function seedMessage(db: Db, sessionId: string, id: string, role: Role, text: string, createdAt: number) {
  const m: Message = { id, sessionId, role, parts: [{ type: "text", text }], pending: false, createdAt };
  db.createMessage(m);
}

// A root session with a workspace and 5 messages.
function seedThread(db: Db): Session {
  const s: Session = { id: "S", parentId: null, title: "big session", kind: "root", createdAt: 1, workspace: "/tmp/ws" };
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

Deno.test("extract: picked nodes copy into a fresh root conversation, thread order, original untouched", async () => {
  const c = ctx();
  seedThread(c.db);
  const h = createHandler(c);
  const events: BoughEvent[] = [];
  c.bus.subscribe((e) => events.push(e));

  // Selection order deliberately scrambled — the copies land in thread order.
  const res = await h(post("/sessions/S/extract", { picks: [{ messageId: "m4" }, { messageId: "m1" }, { messageId: "m3" }] }));
  assertEquals(res.status, 200);
  const { session } = await res.json() as { session: Session };
  assertEquals(session.parentId, null); // a standalone conversation, not a sibling branch
  assertEquals(session.kind, "root");
  assertEquals(session.title, "extract · big session");
  assertEquals(session.workspace, "/tmp/ws"); // work continues in the same repo
  assertEquals(session.originId, "S"); // lineage for the map
  assertEquals(session.originMessageId, "m4"); // last picked node in thread order

  const branch = await (await h(get(`/sessions/${session.id}`))).json() as { thread: Message[] };
  assertEquals(
    branch.thread.map((m) => [m.role, m.parts.map((p) => (p.type === "text" ? p.text : "")).join("")]),
    [["user", "hello"], ["user", "do X"], ["supervisor", "did X"]],
  );

  // Original session is unchanged.
  const orig = await (await h(get("/sessions/S"))).json() as { thread: Message[] };
  assertEquals(orig.thread.map((m) => m.id), ["m1", "m2", "m3", "m4", "m5"]);

  // Events: one session.created + one message.started per copy.
  assertEquals(events.filter((e) => e.type === "session.created").length, 1);
  assertEquals(events.filter((e) => e.type === "message.started" && e.sessionId === session.id).length, 3);
  c.db.close();
});

Deno.test("extract: inherited ancestor turns are fair game (unlike compact/fork)", async () => {
  const c = ctx();
  // root R (msg r1) → child S (msg s1). Extract r1 + s1 from S's thread.
  c.db.createSession({ id: "R", parentId: null, title: "root", kind: "root", createdAt: 1 });
  seedMessage(c.db, "R", "r1", "user", "root msg", 10);
  c.db.createSession({ id: "S", parentId: "R", title: "fork", kind: "fork", createdAt: 2 });
  seedMessage(c.db, "S", "s1", "supervisor", "child msg", 20);
  const h = createHandler(c);

  const res = await h(post("/sessions/S/extract", { picks: [{ messageId: "r1" }, { messageId: "s1" }] }));
  assertEquals(res.status, 200);
  const { session } = await res.json() as { session: Session };
  const branch = await (await h(get(`/sessions/${session.id}`))).json() as { thread: Message[] };
  assertEquals(
    branch.thread.map((m) => m.parts.map((p) => (p.type === "text" ? p.text : "")).join("")),
    ["root msg", "child msg"],
  );
  c.db.close();
});

Deno.test("extract: a partial pick copies only the chosen sections of a turn", async () => {
  const c = ctx();
  const s: Session = { id: "S", parentId: null, title: "big session", kind: "root", createdAt: 1 };
  c.db.createSession(s);
  seedMessage(c.db, "S", "m1", "user", "hello", 10);
  // A supervisor turn: prose, a tool exchange, closing prose (part indexes 0..3).
  c.db.createMessage({
    id: "m2",
    sessionId: "S",
    role: "supervisor",
    parts: [
      { type: "text", text: "plan" },
      { type: "tool_call", id: "t1", name: "bash", input: { command: "ls" } },
      { type: "tool_result", callId: "t1", output: "big output", isError: false },
      { type: "text", text: "conclusion" },
    ],
    pending: false,
    createdAt: 11,
  });
  const h = createHandler(c);

  // Everything except the tool call/result — duplicate picks for one message merge.
  const res = await h(post("/sessions/S/extract", {
    picks: [{ messageId: "m1" }, { messageId: "m2", parts: [0] }, { messageId: "m2", parts: [3] }],
  }));
  assertEquals(res.status, 200);
  const { session } = await res.json() as { session: Session };
  const branch = await (await h(get(`/sessions/${session.id}`))).json() as { thread: Message[] };
  assertEquals(
    branch.thread.map((m) => m.parts.map((p) => (p.type === "text" ? p.text : `[${p.type}]`)).join("|")),
    ["hello", "plan|conclusion"],
  );

  // An out-of-range part index errors cleanly.
  assertEquals((await h(post("/sessions/S/extract", { picks: [{ messageId: "m2", parts: [4] }] }))).status, 400);
  c.db.close();
});

Deno.test("extract: unknown ids and sessions error cleanly", async () => {
  const c = ctx();
  seedThread(c.db);
  const h = createHandler(c);

  assertEquals((await h(post("/sessions/S/extract", { picks: [{ messageId: "m1" }, { messageId: "zzz" }] }))).status, 400);
  assertEquals((await h(post("/sessions/S/extract", { picks: [] }))).status, 400);
  assertEquals((await h(post("/sessions/missing/extract", { picks: [{ messageId: "m1" }] }))).status, 404);
  assertEquals((await h(post("/sessions/S/extract", { nope: 1 }))).status, 400);
  c.db.close();
});
