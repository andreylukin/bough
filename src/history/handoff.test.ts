/**
 * Handoff, with the three claims that make it a distinct operation rather than a
 * differently-worded compaction:
 *
 *   1. **The source is byte-identical afterwards** and NOTHING is copied — the new root
 *      has no messages at all. The distilled context lives entirely in its `draft`.
 *   2. **The draft is a draft.** It is persisted on the session, announced, and cleared
 *      by the first POSTED message rather than by anything this module does — asserted
 *      end to end through the real route table, because "posting the message clears it"
 *      is the half of the contract that lives in `server/sessions.ts`.
 *   3. **A failed or empty draft writes nothing.** The LLM call completes before a row
 *      exists, so the user is never left with an empty root they did not ask for.
 *
 * Offline: an in-memory database, a real bus, and a scripted fake `LlmClient` that
 * records the prompts it was given. `node:assert/strict` — jsr.io is unreachable here
 * (plan §7).
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { HandoffError } from "../errors.ts";
import type { BoughEvent, BoughEventOf } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx, LlmClient, LlmParams, LlmResult } from "../types.ts";
import { DEFAULT_MODEL } from "../turn/runner.ts";
import { createHandler, type Route, route } from "../server/app.ts";
import { handoff, type HandoffCtx, handoffH } from "./handoff.ts";

// ---- fixtures ---------------------------------------------------------------

interface FakeLlm extends LlmClient {
  prompts: string[];
  systems: string[];
  models: string[];
}

function fakeLlm(reply: (prompt: string) => string): FakeLlm {
  const prompts: string[] = [];
  const systems: string[] = [];
  const models: string[] = [];
  return {
    prompts,
    systems,
    models,
    run(params: LlmParams): Promise<LlmResult> {
      const block = params.messages[0].content[0];
      const prompt = block.type === "text" ? block.text : "";
      prompts.push(prompt);
      systems.push(params.system ?? "");
      models.push(params.model);
      return Promise.resolve({
        content: [{ type: "text", text: reply(prompt) }],
        stopReason: "end_turn",
      });
    },
  };
}

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  llm: FakeLlm;
  ctx: HandoffCtx & AppCtx;
}

function fixture(reply: (prompt: string) => string = () => "DRAFT"): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const llm = fakeLlm(reply);
  return { db, bus, events, llm, ctx: { db, bus, llm } };
}

function session(db: SqliteDb, over: Partial<Session> & { title: string }): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    ...over,
  });
}

let stamp = 1_700_000_000_000;

function message(
  db: SqliteDb,
  sessionId: string,
  role: Message["role"],
  parts: Part[],
): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role,
    parts,
    pending: false,
    createdAt: stamp++,
  });
}

function text(db: SqliteDb, sessionId: string, role: Message["role"], t: string): Message {
  return message(db, sessionId, role, [{ type: "text", text: t }]);
}

function snapshot(db: SqliteDb, sessionId: string): string {
  return JSON.stringify({
    session: db.getSession(sessionId),
    messages: db.messagesFor(sessionId),
  });
}

/** A source with an inherited ancestor turn — a handoff distils the VISIBLE thread. */
function scenario(f: Fixture): { parent: Session; source: Session; last: Message } {
  const parent = session(f.db, {
    title: "the migration",
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
    base: "deadbeef",
  });
  text(f.db, parent.id, "user", "migrate the journal to the new key");
  const source = session(f.db, {
    title: "fork · the migration",
    kind: "fork",
    parentId: parent.id,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
    base: "deadbeef",
  });
  text(f.db, source.id, "supervisor", "the key must hash the RESOLVED model");
  const last = text(f.db, source.id, "user", "ok — now do the relaunch path");
  return { parent, source, last };
}

// ---- the draft ---------------------------------------------------------------

Deno.test("handoff opens a root carrying the draft and seeds NO messages", async () => {
  const f = fixture(() => "Finish the relaunch path in workflow/relaunch.ts.");
  const { source, last } = scenario(f);

  const created = await handoff(f.ctx, source.id, { goal: "finish the relaunch path" });

  assert.equal(created.kind, "root");
  assert.equal(created.parentId, null);
  assert.equal(created.draft, "Finish the relaunch path in workflow/relaunch.ts.");
  // Nothing is copied: the distilled context is the draft and only the draft.
  assert.deepEqual(f.db.messagesFor(created.id), []);
  assert.deepEqual(f.db.threadFor(created.id), []);
  // What storage kept, not just what was returned.
  assert.equal(f.db.getSession(created.id)?.draft, created.draft);

  // The same checkout, with the sha the Changes rail measures from.
  assert.equal(created.workspace, "/tmp/checkout");
  assert.equal(created.base, "deadbeef");
  assert.equal(created.originDir, "/tmp/checkout");
  // Lineage back to the source, as of its last thread message.
  assert.equal(created.originId, source.id);
  assert.equal(created.originMessageId, last.id);
  // Titled off the BASE title: handing off a fork must not compound the prefix.
  assert.equal(created.title, "handoff · the migration");
  f.db.close();
});

Deno.test("the prompt carries the WHOLE visible thread and the stated goal", async () => {
  const f = fixture();
  const { source } = scenario(f);

  await handoff(f.ctx, source.id, { goal: "finish the relaunch path" });

  assert.equal(f.llm.prompts.length, 1);
  const prompt = f.llm.prompts[0];
  // The inherited ancestor turn is in it — a handoff distils what the user has been
  // looking at, which in a forked session is mostly inherited.
  assert.match(prompt, /migrate the journal to the new key/);
  assert.match(prompt, /the key must hash the RESOLVED model/);
  assert.match(prompt, /Goal for the new conversation: finish the relaunch path/);
  // The system prompt is the one that says "write the opening prompt", not "summarize".
  assert.match(f.llm.systems[0], /OPENING\s+PROMPT/);
  f.db.close();
});

Deno.test("the draft is trimmed, and an empty one is a 502 that writes nothing", async () => {
  const f = fixture(() => "  \n  Fix the ticker.  \n");
  const { source } = scenario(f);
  const created = await handoff(f.ctx, source.id, { goal: "fix the ticker" });
  assert.equal(created.draft, "Fix the ticker.");

  const g = fixture(() => "   \n ");
  const { source: blank } = scenario(g);
  const before = g.db.listSessions().length;
  await assert.rejects(
    () => handoff(g.ctx, blank.id, { goal: "anything" }),
    (e: unknown) => e instanceof HandoffError && (e as HandoffError).status === 502,
  );
  // The LLM call completes before the first write, so a failed draft leaves no empty
  // root behind for the user to find.
  assert.equal(g.db.listSessions().length, before);
  f.db.close();
  g.db.close();
});

// ---- the source is untouched --------------------------------------------------

Deno.test("handoff leaves the source AND its ancestor byte-identical", async () => {
  const f = fixture();
  const { parent, source } = scenario(f);
  const before = [snapshot(f.db, parent.id), snapshot(f.db, source.id)];

  await handoff(f.ctx, source.id, { goal: "carry on elsewhere" });

  assert.deepEqual([snapshot(f.db, parent.id), snapshot(f.db, source.id)], before);
  f.db.close();
});

// ---- model resolution ----------------------------------------------------------

Deno.test("the session's own pin decides the model, then the global default", async () => {
  const f = fixture();
  const { source } = scenario(f);

  // No pin, no ctx default → the built-in.
  await handoff(f.ctx, source.id, { goal: "a" });
  assert.equal(f.llm.models[0], DEFAULT_MODEL);

  // ctx default.
  await handoff({ ...f.ctx, model: "openai:gpt-5" }, source.id, { goal: "b" });
  assert.equal(f.llm.models[1], "openai:gpt-5");

  // A session pin wins over it — a model id is a provider routing decision, and this
  // user may hold only that provider's key.
  f.db.setSessionModel(source.id, "vendor/some-model");
  await handoff({ ...f.ctx, model: "openai:gpt-5" }, source.id, { goal: "c" });
  assert.equal(f.llm.models[2], "vendor/some-model");
  // …and the new root inherits the pin, for the same reason.
  const created = await handoff(f.ctx, source.id, { goal: "d" });
  assert.equal(created.model, "vendor/some-model");
  f.db.close();
});

// ---- refusals -------------------------------------------------------------------

Deno.test("an unknown session is a 404 and an empty thread is a 400", async () => {
  const f = fixture();
  const empty = session(f.db, { title: "brand new" });

  await assert.rejects(
    () => handoff(f.ctx, "no-such-session", { goal: "anything" }),
    (e: unknown) => e instanceof HandoffError && (e as HandoffError).status === 404,
  );
  await assert.rejects(
    () => handoff(f.ctx, empty.id, { goal: "anything" }),
    (e: unknown) =>
      e instanceof HandoffError && (e as HandoffError).status === 400 &&
      /empty thread/.test((e as Error).message),
  );
  // Neither bought an LLM call.
  assert.deepEqual(f.llm.prompts, []);
  f.db.close();
});

// ---- events ----------------------------------------------------------------------

Deno.test("the root is created, then updated with the draft", async () => {
  const f = fixture(() => "the draft text");
  const { source } = scenario(f);
  f.events.length = 0;

  const created = await handoff(f.ctx, source.id, { goal: "go" });

  assert.deepEqual(f.events.map((e) => e.type), ["session.created", "session.updated"]);
  assert.ok(f.events.every((e) => e.sessionId === created.id));
  // The update carries the draft, so a live tree view has it without a refetch.
  const updated = f.events[1] as BoughEventOf<"session.updated">;
  assert.equal(updated.data.draft, "the draft text");
  f.db.close();
});

// ---- the route, and the draft's other half ----------------------------------------

const TABLE: Route[] = [route("POST", "/sessions/:id/handoff", handoffH)];

Deno.test("POST /sessions/:id/handoff answers 201 with the drafted root", async () => {
  const f = fixture(() => "Pick up the relaunch path.");
  const { source } = scenario(f);
  const call = createHandler(f.ctx, { routes: TABLE });

  const res = await call(
    new Request(`http://x/sessions/${source.id}/handoff`, {
      method: "POST",
      body: JSON.stringify({ goal: "finish the relaunch path" }),
    }),
  );

  assert.equal(res.status, 201);
  const body = await res.json() as { session: Session };
  assert.equal(body.session.draft, "Pick up the relaunch path.");
  assert.equal(body.session.kind, "root");
  f.db.close();
});

Deno.test("the route maps an unknown session to 404 and an empty goal to 400", async () => {
  const f = fixture();
  const { source } = scenario(f);
  const call = createHandler(f.ctx, { routes: TABLE });
  const post = (id: string, body: unknown) =>
    call(
      new Request(`http://x/sessions/${id}/handoff`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    );

  assert.equal((await post("no-such-session", { goal: "x" })).status, 404);

  const blank = await post(source.id, { goal: "" });
  assert.equal(blank.status, 400);
  assert.match((await blank.json()).error, /invalid body/);
  f.db.close();
});

Deno.test("posting the first message clears the draft", async () => {
  const f = fixture(() => "Pick up the relaunch path.");
  const { source } = scenario(f);
  // The REAL route table: the half of this contract that lives in `server/sessions.ts`
  // is what makes the draft a draft rather than a seeded turn.
  const call = createHandler(f.ctx);

  const created = await (await call(
    new Request(`http://x/sessions/${source.id}/handoff`, {
      method: "POST",
      body: JSON.stringify({ goal: "finish the relaunch path" }),
    }),
  )).json() as { session: Session };
  assert.equal(f.db.getSession(created.session.id)?.draft, "Pick up the relaunch path.");

  // The user edits it and sends. Whatever they actually sent supersedes the draft.
  const posted = await call(
    new Request(`http://x/sessions/${created.session.id}/messages`, {
      method: "POST",
      body: JSON.stringify({ text: "Pick up the relaunch path, but start with the tests." }),
    }),
  );
  assert.equal(posted.status, 202);
  assert.equal(f.db.getSession(created.session.id)?.draft, null);
  f.db.close();
});
