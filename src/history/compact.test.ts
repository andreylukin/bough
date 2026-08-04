/**
 * Compaction, with the two claims the AC names as the load-bearing tests:
 *
 *   1. A NON-CONTIGUOUS selection produces one summary per maximal run, with the
 *      unselected messages copied verbatim BETWEEN them, in order.
 *   2. The source session is byte-unchanged — the row and every message it owns are
 *      JSON-identical before and after, because compaction branches and never rewrites
 *      (spec §14).
 *
 * Claim 2 is asserted by snapshotting the source (session row + every message) to JSON
 * before the call and comparing the JSON after, rather than by spot-checking a field: a
 * spot check passes against an implementation that helpfully "tidies" a part it copied,
 * which is exactly the class of bug "never mutates" exists to forbid.
 *
 * Everything runs offline: an in-memory database, a real bus, and a scripted fake
 * `LlmClient` that records the prompts it was given. Assertions come from
 * `node:assert/strict` — jsr.io is unreachable here, so `@std/assert` cannot resolve
 * (plan §7: every test hermetic and offline).
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { CompactError } from "../errors.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx, LlmClient, LlmParams, LlmResult } from "../types.ts";
import { createHandler } from "../server/app.ts";
import { compact, type CompactCtx, renderSpan, runsOf } from "./compact.ts";

// ---- fixtures ---------------------------------------------------------------

interface FakeLlm extends LlmClient {
  /** Every prompt this client was asked to complete, in call order. */
  prompts: string[];
  systems: string[];
  models: string[];
}

/** A summarizer that answers with a stable, identifiable string per call. */
function fakeLlm(reply: (prompt: string, call: number) => string): FakeLlm {
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
        content: [{ type: "text", text: reply(prompt, prompts.length - 1) }],
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
  ctx: CompactCtx & AppCtx;
}

function fixture(
  reply: (prompt: string, call: number) => string = (_p, i) => `SUMMARY-${i}`,
): Fixture {
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

/** A session with `texts.length` messages, alternating user/supervisor. */
function conversation(db: SqliteDb, texts: string[], over: Partial<Session> = {}): {
  source: Session;
  messages: Message[];
} {
  const source = session(db, { title: "the work", ...over });
  const messages = texts.map((t, i) =>
    message(db, source.id, i % 2 === 0 ? "user" : "supervisor", [{ type: "text", text: t }])
  );
  return { source, messages };
}

/** The whole of a session as storage holds it — the byte-unchanged snapshot. */
function snapshot(db: SqliteDb, sessionId: string): string {
  return JSON.stringify({
    session: db.getSession(sessionId),
    messages: db.messagesFor(sessionId),
  });
}

function textOf(m: Message): string {
  return m.parts.map((p) => ("text" in p ? p.text : `<${p.type}>`)).join("|");
}

function textsOf(messages: Message[]): string[] {
  return messages.map(textOf);
}

/**
 * The `CompactError` a call refused with.
 *
 * `assert.rejects` resolves to `undefined`, so it can assert that something threw but
 * never what status it carried — and the status IS the contract here (400 for a
 * selection this operation cannot express, 404 for an unknown session, 502 for a
 * summarizer that produced nothing).
 */
async function refusal(fn: () => Promise<unknown>): Promise<CompactError> {
  try {
    await fn();
  } catch (e) {
    assert.ok(e instanceof CompactError, `expected a CompactError, got ${e}`);
    return e;
  }
  throw new assert.AssertionError({ message: "expected a refusal, got a compaction" });
}

/** Picks by index into a message list, whole-message. */
function picks(messages: Message[], ...indexes: number[]) {
  return indexes.map((i) => ({ messageId: messages[i].id }));
}

// ---- the AC -----------------------------------------------------------------

test("a non-contiguous selection collapses each run to one summary, keeping the messages between them", async () => {
  const f = fixture();
  // 0 1 2 3 4 5 6, selecting {1,2} and {5}: two runs, so two summaries, with 3 and 4
  // copied verbatim between them and 0 and 6 copied around them.
  const { source, messages } = conversation(f.db, ["m0", "m1", "m2", "m3", "m4", "m5", "m6"]);

  const branch = await compact(f.ctx, source.id, { picks: picks(messages, 1, 2, 5) });

  assert.deepEqual(textsOf(f.db.messagesFor(branch.id)), [
    "m0",
    "SUMMARY-0",
    "m3",
    "m4",
    "SUMMARY-1",
    "m6",
  ]);
  // Exactly two summarizer calls — one per run, not one per picked message.
  assert.equal(f.llm.prompts.length, 2);
  // …and each saw only its own run.
  assert.match(f.llm.prompts[0], /m1/);
  assert.match(f.llm.prompts[0], /m2/);
  assert.doesNotMatch(f.llm.prompts[0], /m5/);
  assert.match(f.llm.prompts[1], /m5/);
  assert.doesNotMatch(f.llm.prompts[1], /m1/);
  // The copies are copies, not moves: new ids, same text, same roles.
  const seeded = f.db.messagesFor(branch.id);
  const sourceIds = new Set(messages.map((m) => m.id));
  assert.equal(seeded.some((m) => sourceIds.has(m.id)), false);
  // m0(user) · summary(supervisor) · m3(supervisor) · m4(user) · summary(supervisor) ·
  // m6(user) — a copy keeps its role, and a summary is always the supervisor's.
  assert.deepEqual(seeded.map((m) => m.role), [
    "user",
    "supervisor",
    "supervisor",
    "user",
    "supervisor",
    "user",
  ]);
});

test("the compacted session is byte-unchanged", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c", "d", "e"]);
  const before = snapshot(f.db, source.id);

  await compact(f.ctx, source.id, { picks: picks(messages, 1, 3) });

  assert.equal(snapshot(f.db, source.id), before);
});

// ---- selection semantics ----------------------------------------------------

test("a contiguous selection is one summary in place", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c", "d"]);

  const branch = await compact(f.ctx, source.id, { picks: picks(messages, 1, 2) });

  assert.deepEqual(textsOf(f.db.messagesFor(branch.id)), ["a", "SUMMARY-0", "d"]);
  assert.equal(f.llm.prompts.length, 1);
});

test("picks are ordered and de-duplicated, whatever order the client sent them in", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c", "d", "e"]);

  // Sent backwards, with one duplicate — a user shift-clicking upward.
  const branch = await compact(f.ctx, source.id, {
    picks: [...picks(messages, 3, 1, 3)],
  });

  assert.deepEqual(textsOf(f.db.messagesFor(branch.id)), [
    "a",
    "SUMMARY-0",
    "c",
    "SUMMARY-1",
    "e",
  ]);
  assert.match(f.llm.prompts[0], /b/);
  assert.match(f.llm.prompts[1], /d/);
});

test("runsOf groups only ADJACENT indexes", () => {
  const view = (i: number) => ({ id: `m${i}`, parts: [] } as unknown as Message);
  const runs = runsOf([0, 1, 2, 5, 7, 8].map((idx) => ({ idx, view: view(idx) })));
  assert.deepEqual(runs.map((r) => [r.start, r.end]), [[0, 2], [5, 5], [7, 8]]);
});

test("a part pick narrows what the summarizer sees; the whole message is still replaced", async () => {
  const f = fixture();
  const source = session(f.db, { title: "parts" });
  message(f.db, source.id, "user", [{ type: "text", text: "keep-me" }]);
  const target = message(f.db, source.id, "supervisor", [
    { type: "text", text: "prose-part" },
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "noisy-tool-input" } },
  ]);

  const branch = await compact(f.ctx, source.id, {
    picks: [{ messageId: target.id, parts: [0] }],
  });

  assert.match(f.llm.prompts[0], /prose-part/);
  assert.doesNotMatch(f.llm.prompts[0], /noisy-tool-input/);
  // The message is wholly replaced — the unpicked tool call does not survive beside it.
  assert.deepEqual(textsOf(f.db.messagesFor(branch.id)), ["keep-me", "SUMMARY-0"]);
});

test("instructions steer the summary prompt", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b"]);

  await compact(f.ctx, source.id, {
    picks: picks(messages, 0),
    instructions: "keep the file paths",
  });

  assert.match(f.llm.prompts[0], /Additional instructions: keep the file paths/);
});

// ---- the branch -------------------------------------------------------------

test("the branch is a SIBLING, so shared ancestors come through the parent chain", async () => {
  const f = fixture();
  const root = session(f.db, { title: "root" });
  message(f.db, root.id, "user", [{ type: "text", text: "ancestor-1" }]);
  message(f.db, root.id, "supervisor", [{ type: "text", text: "ancestor-2" }]);
  const child = session(f.db, { title: "child", parentId: root.id, kind: "fork" });
  const own = ["own-a", "own-b", "own-c"].map((t) =>
    message(f.db, child.id, "user", [{ type: "text", text: t }])
  );

  const branch = await compact(f.ctx, child.id, { picks: picks(own, 1) });

  assert.equal(branch.parentId, root.id, "parented at the TARGET's parent");
  assert.equal(branch.kind, "compaction");
  assert.equal(branch.originId, child.id);
  assert.equal(branch.originMessageId, own[1].id);
  // The ancestor's messages were never copied…
  assert.deepEqual(textsOf(f.db.messagesFor(branch.id)), ["own-a", "SUMMARY-0", "own-c"]);
  // …and still appear in the thread, before the branch's own.
  assert.deepEqual(textsOf(f.db.threadFor(branch.id)), [
    "ancestor-1",
    "ancestor-2",
    "own-a",
    "SUMMARY-0",
    "own-c",
  ]);
});

test("the branch inherits the checkout, the base sha, and the session's pins", async () => {
  const f = fixture();
  const source = session(f.db, {
    title: "pinned",
    workspace: "/tmp/checkout",
    base: "abc123",
    originDir: "/tmp/checkout",
    model: "openai:gpt-5",
    effort: "high",
  });
  const own = [message(f.db, source.id, "user", [{ type: "text", text: "x" }])];

  const branch = await compact(f.ctx, source.id, { picks: picks(own, 0) });

  const runtime = f.db.getSessionRuntime(branch.id);
  assert.equal(runtime.workspace, "/tmp/checkout");
  assert.equal(runtime.base, "abc123", "the Changes rail needs the sha its diff is measured from");
  assert.equal(branch.originDir, "/tmp/checkout");
  assert.equal(branch.model, "openai:gpt-5", "a pinned provider must survive the branch");
  assert.equal(branch.effort, "high");
});

test("the summarizer runs on the session's pinned model, not the global default", async () => {
  const f = fixture();
  f.ctx.model = "claude-opus-4-8";
  const source = session(f.db, { title: "pinned", model: "openai:gpt-5" });
  const own = [message(f.db, source.id, "user", [{ type: "text", text: "x" }])];

  await compact(f.ctx, source.id, { picks: picks(own, 0) });

  assert.deepEqual(f.llm.models, ["openai:gpt-5"]);
});

test("the branch is announced before the messages seeded into it", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c"]);

  const branch = await compact(f.ctx, source.id, { picks: picks(messages, 1) });

  const mine = f.events.filter((e) => e.sessionId === branch.id);
  assert.equal(mine[0].type, "session.created");
  assert.deepEqual(mine.slice(1).map((e) => e.type), [
    "message.started",
    "message.started",
    "message.started",
  ]);
});

// ---- refusals ---------------------------------------------------------------

test("a selection reaching into ancestor history is a 400 naming the ancestor", async () => {
  const f = fixture();
  const root = session(f.db, { title: "root" });
  const ancestorMessage = message(f.db, root.id, "user", [{ type: "text", text: "ancestor" }]);
  const child = session(f.db, { title: "child", parentId: root.id, kind: "fork" });
  message(f.db, child.id, "user", [{ type: "text", text: "own" }]);

  const before = f.db.listSessions().length;
  const err = await refusal(() =>
    compact(f.ctx, child.id, { picks: [{ messageId: ancestorMessage.id }] })
  );

  assert.equal(err.status, 400);
  assert.match(err.message, new RegExp(root.id), "names the ancestor session to compact instead");
  assert.equal(f.db.listSessions().length, before, "nothing was branched");
  assert.equal(f.llm.prompts.length, 0, "and nothing was paid for");
});

test("an unknown message, an out-of-range part, an empty session and an unknown session all refuse", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b"]);

  const unknown = await refusal(() =>
    compact(f.ctx, source.id, { picks: [{ messageId: "nope" }] })
  );
  assert.equal(unknown.status, 400);

  const range = await refusal(() =>
    compact(f.ctx, source.id, { picks: [{ messageId: messages[0].id, parts: [7] }] })
  );
  assert.equal(range.status, 400);
  assert.match(range.message, /part index/);

  const empty = session(f.db, { title: "no messages" });
  const none = await refusal(() =>
    compact(f.ctx, empty.id, { picks: [{ messageId: messages[0].id }] })
  );
  assert.equal(none.status, 400);

  const missing = await refusal(() =>
    compact(f.ctx, "no-such-session", { picks: [{ messageId: messages[0].id }] })
  );
  assert.equal(missing.status, 404);
});

test("a failed summarizer leaves no half-seeded branch", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c", "d", "e"]);
  const before = snapshot(f.db, source.id);
  const sessionsBefore = f.db.listSessions().length;
  let call = 0;
  f.ctx.llm = {
    run(): Promise<LlmResult> {
      // The FIRST run succeeds and the second fails — the case where a naive
      // implementation has already written half a transcript.
      if (call++ === 0) {
        return Promise.resolve({
          content: [{ type: "text", text: "ok" }],
          stopReason: "end_turn",
        });
      }
      return Promise.reject(new Error("provider exploded"));
    },
  };

  await assert.rejects(
    () => compact(f.ctx, source.id, { picks: picks(messages, 1, 3) }),
    /provider exploded/,
  );

  assert.equal(f.db.listSessions().length, sessionsBefore, "no branch was created");
  assert.equal(snapshot(f.db, source.id), before);
});

test("an empty summary is a 502, not a message that silently lost the span", async () => {
  const f = fixture(() => "   ");
  const { source, messages } = conversation(f.db, ["a", "b"]);

  const err = await refusal(() => compact(f.ctx, source.id, { picks: picks(messages, 0) }));

  assert.equal(err.status, 502);
  assert.equal(f.db.listSessions().length, 1, "nothing was branched");
});

// ---- the title --------------------------------------------------------------

test("the deterministic title counts picked messages and never compounds", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c", "d"]);

  const one = await compact(f.ctx, source.id, { picks: picks(messages, 1) });
  assert.equal(one.title, "compacted · 1 turn");

  const two = await compact(f.ctx, source.id, { picks: picks(messages, 1, 2) });
  assert.equal(two.title, "compacted · 2 turns");

  // Compacting a compaction does not stack prefixes.
  const own = f.db.messagesFor(two.id);
  const again = await compact(f.ctx, two.id, { picks: [{ messageId: own[0].id }] });
  assert.equal(again.title, "compacted · 1 turn");
});

test("the cheap tier renames the branch from its first summary, and a failure keeps the placeholder", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c"]);
  const seen: string[] = [];
  f.ctx.cheap = {
    title: (text: string) => {
      seen.push(text);
      return Promise.resolve("token refresh race");
    },
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  };

  const branch = await compact(f.ctx, source.id, { picks: picks(messages, 1) });
  assert.equal(branch.title, "compacted · 1 turn", "the response never waits for a rename");
  await flush();

  assert.deepEqual(seen, ["SUMMARY-0"]);
  assert.equal(f.db.getSession(branch.id)?.title, "token refresh race · compacted 1");
  assert.equal(
    f.events.some((e) => e.type === "session.updated" && e.sessionId === branch.id),
    true,
  );

  // A cheap tier that rejects is silent: the compaction stands, the title stays.
  f.ctx.cheap = {
    title: () => Promise.reject(new Error("no key")),
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  };
  const second = await compact(f.ctx, source.id, { picks: picks(messages, 2) });
  await flush();
  assert.equal(f.db.getSession(second.id)?.title, "compacted · 1 turn");
});

/** Let the fire-and-forget rename settle. */
function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// ---- rendering --------------------------------------------------------------

test("renderSpan renders every part kind, and clips oversized tool output", () => {
  const m: Message = {
    id: "m",
    sessionId: "s",
    role: "supervisor",
    pending: false,
    createdAt: 0,
    parts: [
      { type: "text", text: "said" },
      { type: "reasoning", text: "thought" },
      { type: "tool_call", id: "c", name: "run_steps", input: { code: "x" } },
      { type: "tool_result", callId: "c", output: "y".repeat(5000), isError: false },
      { type: "image", path: "/a.png", mediaType: "image/png", name: "a.png", size: 1 },
      {
        type: "ask",
        id: "q",
        question: "which?",
        status: "answered",
        answer: "the second one",
      },
    ],
  };

  const lines = renderSpan([m]).split("\n");
  assert.deepEqual(lines.length, 6);
  assert.match(lines[0], /^supervisor: said$/);
  assert.match(lines[2], /\[tool run_steps\]/);
  assert.ok(lines[3].length < 2100, "a 5000-char tool result is clipped");
  assert.match(lines[3], /…$/);
  assert.match(lines[4], /\[image a\.png\]/);
  assert.match(lines[5], /ask: which\? → user answered: the second one/);

  // A message with no parts still contributes a line, so the roles stay legible.
  assert.equal(renderSpan([{ ...m, parts: [] }]), "supervisor:");
});

// ---- the route --------------------------------------------------------------

test("POST /sessions/:id/compact is reachable and answers 201 with the branch and its thread", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c"]);
  const handler = createHandler(f.ctx as AppCtx);

  const res = await handler(
    new Request(`http://localhost/sessions/${source.id}/compact`, {
      method: "POST",
      body: JSON.stringify({ picks: picks(messages, 1) }),
    }),
  );

  assert.equal(res.status, 201);
  const body = await res.json() as { session: Session; thread: Message[] };
  assert.equal(body.session.kind, "compaction");
  assert.deepEqual(textsOf(body.thread), ["a", "SUMMARY-0", "c"]);
});

test("the route maps domain refusals to their statuses", async () => {
  const f = fixture();
  const handler = createHandler(f.ctx as AppCtx);

  const missing = await handler(
    new Request("http://localhost/sessions/nope/compact", {
      method: "POST",
      body: JSON.stringify({ picks: [{ messageId: "m" }] }),
    }),
  );
  assert.equal(missing.status, 404);

  // An empty selection is the schema's 400, decided at the router edge.
  const { source } = conversation(f.db, ["a"]);
  const bad = await handler(
    new Request(`http://localhost/sessions/${source.id}/compact`, {
      method: "POST",
      body: JSON.stringify({ picks: [] }),
    }),
  );
  assert.equal(bad.status, 400);
});

// ---- the scout --------------------------------------------------------------
//
// `history/explore.ts` owns what the scout is pointed at and how its loop runs; what
// belongs here is only the contract between them: notes reach the summarizer, and a
// compaction happens either way. The `explore` seam is injected, so these run with no
// shell and no second provider key.

test("scout notes reach the summarizer, per run, with the prompt that ranks them", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["m0", "m1", "m2", "m3", "m4", "m5"]);
  // A scout only runs where there is a checkout to read, so the session needs one.
  f.db.setSessionWorkspace(source.id, "/w");
  const spans: string[][] = [];
  f.ctx.explore = (span) => {
    spans.push(span.map((m) => (m.parts[0]?.type === "text" ? m.parts[0].text : "")));
    return Promise.resolve(`NOTES-${spans.length - 1}`);
  };

  await compact(f.ctx, source.id, { picks: picks(messages, 1, 4) });

  // One scout per RUN, each seeing only its own run — the same scoping the summaries get.
  assert.deepEqual(spans, [["m1"], ["m4"]]);
  assert.match(f.llm.prompts[0], /NOTES-0/);
  assert.doesNotMatch(f.llm.prompts[0], /NOTES-1/);
  assert.match(f.llm.prompts[1], /NOTES-1/);
  // And the summarizer is told what to do when the notes and the transcript disagree.
  assert.match(f.llm.systems[0], /the notes are right/);
});

test("no notes leaves the summarizer exactly as it was before the scout existed", async () => {
  const f = fixture();
  const { source, messages } = conversation(f.db, ["a", "b", "c"]);
  f.db.setSessionWorkspace(source.id, "/w");
  f.ctx.explore = () => Promise.resolve(null);

  const branch = await compact(f.ctx, source.id, { picks: picks(messages, 1) });

  assert.deepEqual(textsOf(f.db.messagesFor(branch.id)), ["a", "SUMMARY-0", "c"]);
  assert.doesNotMatch(f.llm.systems[0], /the notes are right/);
  assert.doesNotMatch(f.llm.prompts[0], /Scout notes/);
});
