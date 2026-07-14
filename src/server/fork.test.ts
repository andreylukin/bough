import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { fork } from "../fork.ts";
import { type AppCtx, createHandler } from "./app.ts";
import type { Message, Role, Session } from "../schema/parts.ts";
import type { LlmClient } from "../supervisor/llm.ts";

// Fake LLM: one text round, then it answers the harness's stop-nudge with a stop
// call so the turn ends (the compliant-model shape).
const fakeLlm = (reply: string): LlmClient => {
  let called = false;
  return {
    run: () => {
      if (called) {
        return Promise.resolve({
          content: [{ type: "tool_use" as const, id: "stop-1", name: "stop", input: {} }],
          stopReason: "tool_use",
        });
      }
      called = true;
      return Promise.resolve({
        content: [{ type: "text" as const, text: reply }],
        stopReason: "end_turn",
      });
    },
  };
};

function ctx(llm?: LlmClient): AppCtx {
  const bus = new Bus();
  return { db: new Db(":memory:"), bus, llm };
}

function seed(db: Db, sessionId: string, id: string, role: Role, text: string, createdAt: number) {
  db.createMessage({
    id,
    sessionId,
    role,
    parts: [{ type: "text", text }],
    pending: false,
    createdAt,
  });
}

// Root session S with 4 messages: a / b / c / d (user / sup / user / sup).
function seedThread(db: Db, workspace?: string): Session {
  const s: Session = {
    id: "S",
    parentId: null,
    title: "root",
    kind: "root",
    createdAt: 1,
    ...(workspace ? { workspace } : {}),
  };
  db.createSession(s);
  seed(db, "S", "m1", "user", "a", 10);
  seed(db, "S", "m2", "supervisor", "b", 11);
  seed(db, "S", "m3", "user", "c", 12);
  seed(db, "S", "m4", "supervisor", "d", 13);
  return s;
}

const post = (path: string, body: unknown) =>
  new Request("http://x" + path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
const get = (path: string) => new Request("http://x" + path);
const texts = (thread: Message[]) =>
  thread.map((m) => [m.role, m.parts.map((p) => (p.type === "text" ? p.text : "")).join("")]);

Deno.test("fork no-edit: prefix + fork-point copied, original untouched, workspace inherited", async () => {
  const c = ctx();
  seedThread(c.db, "/tmp/proj");
  const h = createHandler(c);

  const { session } = await (await h(post("/sessions/S/fork", { atMessageId: "m3" }))).json() as {
    session: Session;
  };
  assertEquals(session.kind, "fork");
  assertEquals(session.parentId, null); // sibling of the root target
  assertEquals(session.workspace, "/tmp/proj"); // inherited
  // Lineage: forked-from session + at-message.
  assertEquals(session.originId, "S");
  assertEquals(session.originMessageId, "m3");

  const branch = await (await h(get(`/sessions/${session.id}`))).json() as {
    session: Session;
    thread: Message[];
  };
  assertEquals(branch.session.originId, "S"); // survives the DB round-trip
  assertEquals(branch.session.originMessageId, "m3");
  assertEquals(texts(branch.thread), [["user", "a"], ["supervisor", "b"], ["user", "c"]]);

  const orig = await (await h(get("/sessions/S"))).json() as {
    session: Session;
    thread: Message[];
  };
  assertEquals(orig.thread.map((m) => m.id), ["m1", "m2", "m3", "m4"]);
  assertEquals(orig.session.originId, undefined); // a root session carries no lineage
  c.db.close();
});

Deno.test("fork with-edit: edited user message lands and a turn runs on the fork", async () => {
  const c = ctx(fakeLlm("FORK-REPLY"));
  seedThread(c.db); // no workspace → turn runs unsandboxed, no FS side effects
  // Call fork() directly so we can await the turn deterministically.
  const { session, done } = fork(c, "S", { atMessageId: "m3", editedText: "c-EDITED" });
  await done;

  const thread = c.db.threadFor(session.id);
  assertEquals(texts(thread), [
    ["user", "a"],
    ["supervisor", "b"],
    ["user", "c-EDITED"], // m3 replaced by the edit; original "c" gone
    ["supervisor", "FORK-REPLY"], // the fresh turn's reply
  ]);
  assertEquals(thread.at(-1)!.pending, false);
  // original untouched
  assertEquals(c.db.messagesFor("S").map((m) => m.id), ["m1", "m2", "m3", "m4"]);
  c.db.close();
});

Deno.test("fork atPart: cut after a failed tool result, explain, and a turn runs from there", async () => {
  const c = ctx(fakeLlm("TRYING-ANOTHER-WAY"));
  const s: Session = { id: "S", parentId: null, title: "root", kind: "root", createdAt: 1 };
  c.db.createSession(s);
  seed(c.db, "S", "m1", "user", "do the thing", 10);
  // A supervisor turn: plan, a failed tool exchange, then it barreled on (parts 0..3).
  c.db.createMessage({
    id: "m2",
    sessionId: "S",
    role: "supervisor",
    parts: [
      { type: "text", text: "I'll try approach A" },
      { type: "tool_call", id: "t1", name: "bash", input: { command: "approach-a" } },
      { type: "tool_result", callId: "t1", output: "boom: approach A failed", isError: true },
      { type: "text", text: "kept going anyway" },
    ],
    pending: false,
    createdAt: 11,
  });

  // Cut right after the failed tool result (part 2) and explain.
  const { session, done } = fork(c, "S", {
    atMessageId: "m2",
    atPart: 2,
    editedText: "don't try it that way — approach A can't work here, use B",
  });
  await done;

  const thread = c.db.threadFor(session.id);
  // Prefix, the truncated supervisor turn (cut point included, later parts gone),
  // the correction, and the fresh turn's reply.
  assertEquals(thread.map((m) => m.role), ["user", "supervisor", "user", "supervisor"]);
  assertEquals(thread[1].parts, [
    { type: "text", text: "I'll try approach A" },
    { type: "tool_call", id: "t1", name: "bash", input: { command: "approach-a" } },
    { type: "tool_result", callId: "t1", output: "boom: approach A failed", isError: true },
  ]);
  assertEquals(texts(thread).slice(2), [
    ["user", "don't try it that way — approach A can't work here, use B"],
    ["supervisor", "TRYING-ANOTHER-WAY"],
  ]);
  // Original untouched — m2 still has all 4 parts.
  assertEquals(c.db.getMessage("m2")!.parts.length, 4);
  c.db.close();
});

Deno.test("fork errors: ancestor msg, non-user edit target, unknown ids, unknown session", async () => {
  const c = ctx(fakeLlm("x"));
  // root R (r1) → child S (s1)
  c.db.createSession({ id: "R", parentId: null, title: "root", kind: "root", createdAt: 1 });
  seed(c.db, "R", "r1", "user", "root", 10);
  c.db.createSession({ id: "S", parentId: "R", title: "fork", kind: "fork", createdAt: 2 });
  seed(c.db, "S", "s1", "user", "child", 20);
  seed(c.db, "S", "s2", "supervisor", "reply", 21);
  const h = createHandler(c);

  // ancestor message (r1 belongs to R, not S's own)
  assertEquals((await h(post("/sessions/S/fork", { atMessageId: "r1" }))).status, 400);
  // editing a supervisor message (without an atPart cut)
  assertEquals(
    (await h(post("/sessions/S/fork", { atMessageId: "s2", editedText: "no" }))).status,
    400,
  );
  // …but WITH atPart the text is a new user message after the cut, not an edit — allowed
  assertEquals(
    (await h(post("/sessions/S/fork", { atMessageId: "s2", atPart: 0, editedText: "use B" })))
      .status,
    200,
  );
  // atPart beyond the message's parts
  assertEquals((await h(post("/sessions/S/fork", { atMessageId: "s2", atPart: 9 }))).status, 400);
  // unknown message id
  assertEquals((await h(post("/sessions/S/fork", { atMessageId: "zzz" }))).status, 400);
  // unknown session
  assertEquals((await h(post("/sessions/nope/fork", { atMessageId: "s1" }))).status, 404);
  // malformed body
  assertEquals((await h(post("/sessions/S/fork", { nope: 1 }))).status, 400);
  c.db.close();
});

Deno.test("fork no-edit from a child session branches as a sibling under the shared parent", async () => {
  const c = ctx();
  c.db.createSession({ id: "R", parentId: null, title: "root", kind: "root", createdAt: 1 });
  seed(c.db, "R", "r1", "user", "root msg", 10);
  c.db.createSession({ id: "S", parentId: "R", title: "child", kind: "fork", createdAt: 2 });
  seed(c.db, "S", "s1", "user", "s-one", 20);
  seed(c.db, "S", "s2", "supervisor", "s-two", 21);
  const h = createHandler(c);

  const { session } = await (await h(post("/sessions/S/fork", { atMessageId: "s2" }))).json() as {
    session: Session;
  };
  assertEquals(session.parentId, "R"); // sibling of S under the shared parent R
  const branch = await (await h(get(`/sessions/${session.id}`))).json() as { thread: Message[] };
  // R's message (inherited) + prefix (s1) + the fork-point (s2)
  assertEquals(texts(branch.thread), [["user", "root msg"], ["user", "s-one"], [
    "supervisor",
    "s-two",
  ]]);
  c.db.close();
});

Deno.test("fork title: named after the branch point; no prefix stacking", async () => {
  const c = ctx();
  seedThread(c.db);
  const h = createHandler(c);

  // The at-message's text names the fork.
  const r1 = await (await h(post("/sessions/S/fork", { atMessageId: "m3" }))).json() as {
    session: Session;
  };
  assertEquals(r1.session.title, "fork · c");

  // Forking the fork doesn't compound to "fork · fork · …".
  const own = await (await h(get(`/sessions/${r1.session.id}`))).json() as { thread: Message[] };
  const at = own.thread.find((m) => m.sessionId === r1.session.id)!;
  const r2 = await (await h(post(`/sessions/${r1.session.id}/fork`, { atMessageId: at.id })))
    .json() as { session: Session };
  assertEquals(r2.session.title.startsWith("fork · fork ·"), false);
  assertEquals(r2.session.title.startsWith("fork · "), true);
  c.db.close();
});
