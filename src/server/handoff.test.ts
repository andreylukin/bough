import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { type AppCtx, createHandler } from "./app.ts";
import type { BoughEvent, Message, Role, Session } from "../schema/parts.ts";
import type { LlmClient, LlmParams } from "../supervisor/llm.ts";

// A fake LLM that returns a fixed draft and records the prompt it was given.
function fakeLlm(draft: string): { client: LlmClient; prompts: string[] } {
  const prompts: string[] = [];
  return {
    prompts,
    client: {
      run(params: LlmParams) {
        const block = params.messages[0].content[0];
        prompts.push(block.type === "text" ? block.text : "");
        return Promise.resolve({
          content: [{ type: "text" as const, text: draft }],
          stopReason: "end_turn",
        });
      },
    },
  };
}

function ctx(llm?: LlmClient): AppCtx {
  return { db: new Db(":memory:"), bus: new Bus(), llm };
}

function seedMessage(
  db: Db,
  sessionId: string,
  id: string,
  role: Role,
  text: string,
  createdAt: number,
) {
  db.createMessage({
    id,
    sessionId,
    role,
    parts: [{ type: "text", text }],
    pending: false,
    createdAt,
  });
}

function seedThread(db: Db): Session {
  const s: Session = {
    id: "S",
    parentId: null,
    title: "big session",
    kind: "root",
    createdAt: 1,
    workspace: "/tmp/ws",
  };
  db.createSession(s);
  seedMessage(db, "S", "m1", "user", "hello", 10);
  seedMessage(db, "S", "m2", "supervisor", "did the thing in src/a.ts", 11);
  return s;
}

const post = (path: string, body: unknown) =>
  new Request("http://x" + path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
const get = (path: string) => new Request("http://x" + path);

Deno.test("handoff: drafts from the whole thread + goal onto a fresh root with the draft attached", async () => {
  const llm = fakeLlm("Now do Y. Context: X was done in src/a.ts.");
  const c = ctx(llm.client);
  seedThread(c.db);
  const h = createHandler(c);
  const events: BoughEvent[] = [];
  c.bus.subscribe((e) => events.push(e));

  const res = await h(post("/sessions/S/handoff", { goal: "do Y for teams too" }));
  assertEquals(res.status, 200);
  const { session } = await res.json() as { session: Session };

  // A standalone root on the same workspace, lineage back to the source.
  assertEquals(session.parentId, null);
  assertEquals(session.kind, "root");
  assertEquals(session.title, "handoff · big session");
  assertEquals(session.workspace, "/tmp/ws");
  assertEquals(session.originId, "S");
  assertEquals(session.originMessageId, "m2");
  assertEquals(session.draft, "Now do Y. Context: X was done in src/a.ts.");

  // The drafter saw the rendered thread and the goal.
  assertStringIncludes(llm.prompts[0], "did the thing in src/a.ts");
  assertStringIncludes(llm.prompts[0], "Goal for the new conversation: do Y for teams too");

  // No messages are seeded — the draft carries the context; the draft persists.
  const branch = await (await h(get(`/sessions/${session.id}`))).json() as {
    session: Session;
    thread: Message[];
  };
  assertEquals(branch.thread.length, 0);
  assertEquals(branch.session.draft, "Now do Y. Context: X was done in src/a.ts.");

  // Original session untouched; events announce the branch and its draft.
  const orig = await (await h(get("/sessions/S"))).json() as { thread: Message[] };
  assertEquals(orig.thread.map((m) => m.id), ["m1", "m2"]);
  assertEquals(events.filter((e) => e.type === "session.created").length, 1);
  assert(
    events.some((e) =>
      e.type === "session.updated" && e.sessionId === session.id &&
      (e.data as Session).draft === "Now do Y. Context: X was done in src/a.ts."
    ),
  );
  c.db.close();
});

Deno.test("handoff: posting the first message consumes the draft", async () => {
  const llm = fakeLlm("draft prompt");
  const c = ctx(llm.client);
  seedThread(c.db);
  const h = createHandler(c);

  const { session } = await (await h(post("/sessions/S/handoff", { goal: "g" })))
    .json() as { session: Session };
  assertEquals(session.draft, "draft prompt");

  const events: BoughEvent[] = [];
  c.bus.subscribe((e) => events.push(e));
  const res = await h(post(`/sessions/${session.id}/messages`, { text: "edited draft prompt" }));
  assertEquals(res.status, 202);

  const after = await (await h(get(`/sessions/${session.id}`))).json() as { session: Session };
  assertEquals(after.session.draft ?? null, null);
  // The clear is announced so live views drop the stale draft.
  assert(
    events.some((e) =>
      e.type === "session.updated" && e.sessionId === session.id &&
      (e.data as Session).draft == null
    ),
  );
  c.db.close();
});

Deno.test("handoff: titles don't stack; goal drives the drafter, not the title", async () => {
  const llm = fakeLlm("d");
  const c = ctx(llm.client);
  seedThread(c.db);
  const h = createHandler(c);

  const once = await (await h(post("/sessions/S/handoff", { goal: "g1" })))
    .json() as { session: Session };
  // Hand off from a handoff (after it has a message) — the prefix must not stack.
  seedMessage(c.db, once.session.id, "h1", "user", "sent", 30);
  const twice = await (await h(post(`/sessions/${once.session.id}/handoff`, { goal: "g2" })))
    .json() as { session: Session };
  assertEquals(twice.session.title, "handoff · big session");
  c.db.close();
});

Deno.test("handoff: empty thread, unknown session, and bad bodies error cleanly", async () => {
  const llm = fakeLlm("d");
  const c = ctx(llm.client);
  c.db.createSession({ id: "E", parentId: null, title: "empty", kind: "root", createdAt: 1 });
  const h = createHandler(c);

  assertEquals((await h(post("/sessions/E/handoff", { goal: "g" }))).status, 400);
  assertEquals((await h(post("/sessions/missing/handoff", { goal: "g" }))).status, 404);
  assertEquals((await h(post("/sessions/E/handoff", { goal: "" }))).status, 400);
  assertEquals((await h(post("/sessions/E/handoff", { nope: 1 }))).status, 400);
  c.db.close();
});
