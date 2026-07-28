/**
 * Tests for fork-at-message and edit-and-resend (plan T8.2).
 *
 * The acceptance criteria are the four cuts and the two 400s, and each is asserted on
 * what the BRANCH ends up holding rather than on how it got there — a fork is only
 * interesting as the transcript it produces. Alongside them, and asserted in every
 * mode, is the invariant the module exists to hold: **the source session is
 * byte-identical afterwards** (plan T8.2's shared AC). "Edit & resend" reads like a
 * mutation to the user, so the test snapshots the source's rows before the fork and
 * deep-compares them after.
 *
 * The two edit-and-resend tests run a REAL turn through `turn/runner.ts`, driven by a
 * scripted fake `LlmClient` and a fake program runner: nothing here touches the
 * network, binds a socket, or writes outside an in-memory database (plan §7). One of
 * them reads the provider payload the runner assembled, because a mid-message cut is
 * allowed to strand a `tool_call` and `turn/replay.ts` is what has to make that
 * replayable.
 *
 * Assertions come from `node:assert/strict` — jsr.io is unreachable here, so a test
 * that needs `@std/assert` cannot run offline.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { ForkError } from "../errors.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx, LlmClient, LlmMessage, LlmParams, LlmResult } from "../types.ts";
import { beginTurn, STOP } from "../turn/runner.ts";
import { TurnRegistry } from "../turn/queue.ts";
// `../server/app.ts` FIRST, and deliberately: `history/fork.ts` imports its `json`
// helper, which is the cycle `app.ts` documents as safe. It is safe only when `app.ts`
// is the module that starts evaluating — entering through a handler module instead
// leaves its exports in the temporal dead zone while `app.ts` builds the route table.
import { createHandler, type Route, route } from "../server/app.ts";
import { fork, forkSessionH, type ForkStarter } from "./fork.ts";

// ---- fixtures ---------------------------------------------------------------

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  ctx: AppCtx;
  /** Every fake round the LLM was asked for, for the replay assertions. */
  asked: LlmParams[];
}

/** A model that says one thing and stops, in the same response (spec §5). */
function oneRoundLlm(said: string, asked: LlmParams[]): LlmClient {
  return {
    run(params: LlmParams): Promise<LlmResult> {
      // Deep-copied at request time on purpose: the runner keeps ONE `messages` array
      // for the whole turn and pushes this round's answer onto it, so holding the
      // reference would let the assertion see blocks that were not sent.
      asked.push({ ...params, messages: JSON.parse(JSON.stringify(params.messages)) });
      return Promise.resolve({
        content: [
          { type: "text", text: said },
          { type: "tool_use", id: "stop-1", name: STOP, input: {} },
        ],
        stopReason: "tool_use",
      });
    },
  };
}

function fixture(said = "fresh answer"): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const asked: LlmParams[] = [];
  return { db, bus, events, asked, ctx: { db, bus, llm: oneRoundLlm(said, asked) } };
}

/** Turn deps that keep the runner entirely off the network and out of a worker. */
function turnDeps() {
  return {
    registry: new TurnRegistry(),
    assemble: () => ({ system: "SYSTEM", systemVolatile: "", sections: [] }),
    program: () => Promise.resolve({ ok: true as const, logs: [] }),
    outageDelayMs: 0,
    reportError: (err: unknown) => {
      throw err;
    },
  };
}

/** The starter the resend tests inject: a real turn whose promise they can await. */
const realTurn: ForkStarter = (ctx, session) => beginTurn(ctx, session.id, turnDeps()).done;

function session(db: SqliteDb, over: Partial<Session> & { title: string }): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    ...over,
  });
}

let stamp = 1_000;
function message(
  db: SqliteDb,
  sessionId: string,
  role: Message["role"],
  parts: Part[] | string,
): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role,
    parts: typeof parts === "string" ? [{ type: "text", text: parts }] : parts,
    pending: false,
    createdAt: stamp++,
  });
}

function textOf(m: Message): string {
  return m.parts.map((p) => ("text" in p ? p.text : `<${p.type}>`)).join("|");
}

function textsOf(messages: Message[]): string[] {
  return messages.map(textOf);
}

/**
 * A parent with shared history, and the session about to be forked:
 *
 *   parent  : "ancestor question" / "ancestor answer"
 *   target  : "first ask" / "first answer" / "second ask" / "second answer"
 *
 * The fork point in most tests is `own[2]` — "second ask", a user turn with a
 * supervisor answer after it, so a cut that failed to stop would be visible.
 */
function scenario(f: Fixture) {
  const parent = session(f.db, { title: "parent" });
  message(f.db, parent.id, "user", "ancestor question");
  message(f.db, parent.id, "supervisor", "ancestor answer");
  const target = session(f.db, {
    title: "rename the router",
    parentId: parent.id,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
    base: "abc123",
  });
  message(f.db, target.id, "user", "first ask");
  message(f.db, target.id, "supervisor", "first answer");
  message(f.db, target.id, "user", "second ask");
  message(f.db, target.id, "supervisor", "second answer");
  return { parent, target, own: f.db.messagesFor(target.id) };
}

/** Everything about the source that a fork must not disturb. */
function snapshot(db: SqliteDb, sessionId: string): string {
  return JSON.stringify({
    session: db.getSession(sessionId),
    messages: db.messagesFor(sessionId),
  });
}

// ---- mode 1: editedText — edit & resend --------------------------------------

test("editedText seeds the prefix, appends the replacement, and runs a real turn", async () => {
  const f = fixture("fresh answer");
  const { parent, target, own } = scenario(f);
  const before = snapshot(f.db, target.id);

  const result = fork(f.ctx, target.id, {
    atMessageId: own[2].id, // "second ask"
    editedText: "second ask, rephrased",
  }, { start: realTurn });

  assert.equal(result.turnStarted, true);
  const outcome = await result.done as { status: string };
  assert.equal(outcome.status, "done");

  // The at-message is REPLACED, not copied: the prefix, then the edit, then the
  // model's fresh answer. Nothing from "second ask" onward survives.
  assert.deepEqual(textsOf(f.db.messagesFor(result.session.id)), [
    "first ask",
    "first answer",
    "second ask, rephrased",
    "fresh answer",
  ]);
  assert.deepEqual(f.db.messagesFor(result.session.id).map((m) => m.role), [
    "user",
    "supervisor",
    "user",
    "supervisor",
  ]);

  // A SIBLING: parented at the target's parent, so the ancestors are inherited rather
  // than copied, and the branch's thread is the whole conversation.
  assert.equal(result.session.parentId, parent.id);
  assert.equal(result.session.kind, "fork");
  assert.deepEqual(textsOf(f.db.threadFor(result.session.id)), [
    "ancestor question",
    "ancestor answer",
    "first ask",
    "first answer",
    "second ask, rephrased",
    "fresh answer",
  ]);

  // Lineage, for the tree view: what it branched from, and where.
  assert.equal(result.session.originId, target.id);
  assert.equal(result.session.originMessageId, own[2].id);
  // The same checkout, worked in place — and the sha its change set is measured from.
  assert.equal(result.session.workspace, "/tmp/checkout");
  assert.equal(result.session.originDir, "/tmp/checkout");
  assert.equal(result.session.base, "abc123");
  // Titled after the branch point, off the source's BASE title if it has no text.
  assert.equal(result.session.title, "fork · second ask");

  // THE INVARIANT: the source is byte-identical.
  assert.equal(snapshot(f.db, target.id), before);
  f.db.close();
});

test("the resent turn replays the seeded prefix and nothing after the cut", async () => {
  const f = fixture("fresh answer");
  const { target, own } = scenario(f);

  const result = fork(f.ctx, target.id, {
    atMessageId: own[2].id,
    editedText: "second ask, rephrased",
  }, { start: realTurn });
  await result.done;

  // One round was asked for, and what it carried is the branch's thread: inherited
  // ancestors, the copied prefix, the edit — and no trace of the turn forked away
  // from, which is the whole point of the operation.
  assert.equal(f.asked.length, 1);
  const sent = f.asked[0].messages.flatMap((m: LlmMessage) =>
    m.content.map((b) => ("text" in b ? b.text : `<${b.type}>`))
  );
  assert.deepEqual(sent, [
    "ancestor question",
    "ancestor answer",
    "first ask",
    "first answer",
    "second ask, rephrased",
  ]);
  f.db.close();
});

test("editedText is trimmed, and an empty one is a 400 rather than a turn asked to answer nothing", () => {
  const f = fixture();
  const { target, own } = scenario(f);

  const result = fork(f.ctx, target.id, {
    atMessageId: own[2].id,
    editedText: "  padded  \n",
  });
  assert.deepEqual(textsOf(f.db.messagesFor(result.session.id)).at(-1), "padded");
  // No starter wired: the branch exists carrying the edit, and says so honestly.
  assert.equal(result.turnStarted, false);
  assert.equal(result.done, undefined);

  assert.throws(
    () => fork(f.ctx, target.id, { atMessageId: own[2].id, editedText: "   " }),
    (e: unknown) => e instanceof ForkError && e.status === 400 && /empty/.test(String(e)),
  );
  f.db.close();
});

// ---- mode 2: no editedText — a plain branch point -----------------------------

test("without editedText the at-message is copied too, and no turn runs", () => {
  const f = fixture();
  const { target, own } = scenario(f);
  const before = snapshot(f.db, target.id);

  const result = fork(f.ctx, target.id, { atMessageId: own[2].id });

  assert.equal(result.turnStarted, false);
  assert.equal(result.done, undefined);
  assert.deepEqual(textsOf(f.db.messagesFor(result.session.id)), [
    "first ask",
    "first answer",
    "second ask",
  ]);
  // Seeded history is complete on arrival — a pending copy would look like a turn
  // that never finished, with nothing left to close it.
  assert.deepEqual(f.db.messagesFor(result.session.id).map((m) => m.pending), [
    false,
    false,
    false,
  ]);
  // Copies, not moves: new ids, and the parts share no structure with the source.
  const copied = f.db.messagesFor(result.session.id);
  assert.equal(copied.some((m) => own.some((o) => o.id === m.id)), false);
  assert.equal(snapshot(f.db, target.id), before);

  // The whole branch is announced, session first: a `message.started` for a session
  // the client has never heard of is a message it has nowhere to put.
  assert.deepEqual(f.events.map((e) => e.type), [
    "session.created",
    "message.started",
    "message.started",
    "message.started",
  ]);
  f.db.close();
});

// ---- mode 3: exclusive — the cut lands strictly before ------------------------

test("exclusive skips the at-message: the branch ends strictly before it", () => {
  const f = fixture();
  const { target, own } = scenario(f);
  const before = snapshot(f.db, target.id);

  const result = fork(f.ctx, target.id, { atMessageId: own[2].id, exclusive: true });

  assert.deepEqual(textsOf(f.db.messagesFor(result.session.id)), [
    "first ask",
    "first answer",
  ]);
  assert.equal(result.turnStarted, false);
  // Still the at-message that lineage points at — that is where the cut was made,
  // whether or not the message itself came along.
  assert.equal(result.session.originMessageId, own[2].id);
  assert.equal(snapshot(f.db, target.id), before);
  f.db.close();
});

test("exclusive is a no-op where the at-message's fate is already decided", async () => {
  const f = fixture();
  const { target, own } = scenario(f);

  // With editedText the at-message is replaced…
  const edited = fork(f.ctx, target.id, {
    atMessageId: own[2].id,
    editedText: "rephrased",
    exclusive: true,
  }, { start: realTurn });
  await edited.done;
  assert.deepEqual(textsOf(f.db.messagesFor(edited.session.id)), [
    "first ask",
    "first answer",
    "rephrased",
    "fresh answer",
  ]);

  // …and with atPart it is truncated. Neither is contradicted by `exclusive`; both
  // have already said what becomes of it.
  const cut = fork(f.ctx, target.id, {
    atMessageId: own[2].id,
    atPart: 0,
    exclusive: true,
  });
  assert.deepEqual(textsOf(f.db.messagesFor(cut.session.id)), [
    "first ask",
    "first answer",
    "second ask",
  ]);
  f.db.close();
});

// ---- mode 4: atPart — the cut lands inside the at-message ----------------------

test("atPart copies the at-message truncated to parts[0..atPart]", () => {
  const f = fixture();
  const { target } = scenario(f);
  // A supervisor turn that narrated, ran two programs, and only then answered.
  const rich = message(f.db, target.id, "supervisor", [
    { type: "text", text: "looking" },
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "one()" } },
    { type: "tool_result", callId: "c1", output: "boom", isError: true },
    { type: "text", text: "that failed, trying again" },
    { type: "tool_call", id: "c2", name: "run_steps", input: { code: "two()" } },
  ]);
  const before = snapshot(f.db, target.id);

  // Cut just after the failed tool result — history up to the failure, nothing after.
  const result = fork(f.ctx, target.id, { atMessageId: rich.id, atPart: 2 });

  const own = f.db.messagesFor(result.session.id);
  assert.deepEqual(textsOf(own), [
    "first ask",
    "first answer",
    "second ask",
    "second answer",
    "looking|<tool_call>|<tool_result>",
  ]);
  assert.equal(own.at(-1)!.parts.length, 3);
  assert.equal(result.turnStarted, false);
  assert.equal(snapshot(f.db, target.id), before);

  // Out of range is a 400 naming the last usable cut point rather than a truncation
  // that silently keeps the whole message.
  assert.throws(
    () => fork(f.ctx, target.id, { atMessageId: rich.id, atPart: 5 }),
    (e: unknown) => e instanceof ForkError && e.status === 400 && /out of range/.test(String(e)),
  );
  // The boundary itself is legal: the last part is a cut point like any other.
  assert.equal(
    f.db.messagesFor(
      fork(f.ctx, target.id, { atMessageId: rich.id, atPart: 4 }).session.id,
    ).at(-1)!.parts.length,
    5,
  );
  f.db.close();
});

test("atPart with editedText appends a correction after the cut, whatever the at-message's role", async () => {
  const f = fixture("different approach it is");
  const { target } = scenario(f);
  const rich = message(f.db, target.id, "supervisor", [
    { type: "text", text: "looking" },
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "one()" } },
  ]);
  const before = snapshot(f.db, target.id);

  // The at-message is a SUPERVISOR message — legal here, because with `atPart` the
  // edit is a new message after the cut rather than a replacement for that one.
  const result = fork(f.ctx, target.id, {
    atMessageId: rich.id,
    atPart: 1,
    editedText: "don't try it that way",
  }, { start: realTurn });
  assert.equal(result.turnStarted, true);
  await result.done;

  assert.deepEqual(textsOf(f.db.messagesFor(result.session.id)), [
    "first ask",
    "first answer",
    "second ask",
    "second answer",
    "looking|<tool_call>",
    "don't try it that way",
    "different approach it is",
  ]);

  // The cut stranded a `tool_call` with no `tool_result`, which is exactly what
  // `atPart` is for — and every provider rejects a thread with the pair left open.
  // `turn/replay.ts` closes it with a synthetic result rather than pretending the
  // call succeeded, and that is what makes a mid-message fork replayable at all.
  const blocks = f.asked[0].messages.flatMap((m: LlmMessage) => m.content);
  const synthetic = blocks.find((b) => b.type === "tool_result");
  assert.ok(synthetic, "the stranded tool_call must be paired with a synthetic result");
  assert.match(
    (synthetic as { content: string }).content,
    /interrupted/,
    "the synthetic result must say the call never returned, not that it succeeded",
  );
  assert.equal(snapshot(f.db, target.id), before);
  f.db.close();
});

// ---- the two 400s --------------------------------------------------------------

test("400: editedText may not replace a supervisor turn", () => {
  const f = fixture();
  const { target, own } = scenario(f);
  const before = snapshot(f.db, target.id);
  const sessionsBefore = f.db.listSessions().length;

  assert.throws(
    () => fork(f.ctx, target.id, { atMessageId: own[1].id, editedText: "you said this" }),
    (e: unknown) =>
      e instanceof ForkError && e.status === 400 &&
      /editedText can only replace a user message/.test(String(e)),
  );

  // Refused BEFORE the branch was opened: a check that ran after `openBranch` would
  // leave an empty half-seeded session in the user's list on every bad request.
  assert.equal(f.db.listSessions().length, sessionsBefore);
  assert.equal(f.events.length, 0);
  assert.equal(snapshot(f.db, target.id), before);
  f.db.close();
});

test("400: a fork point in ancestor history names the ancestor to fork instead", () => {
  const f = fixture();
  const { parent, target } = scenario(f);
  const ancestorMessage = f.db.messagesFor(parent.id)[0];
  // The user can SEE this message in the target's transcript — the thread is
  // ancestors ++ own — which is why the error has to name the session that owns it.
  assert.ok(f.db.threadFor(target.id).some((m) => m.id === ancestorMessage.id));

  assert.throws(
    () => fork(f.ctx, target.id, { atMessageId: ancestorMessage.id }),
    (e: unknown) =>
      e instanceof ForkError && e.status === 400 &&
      String(e).includes(`fork ${parent.id} instead`),
  );
  assert.equal(f.events.length, 0);

  // An id from an unrelated session is refused too, and says something different.
  const other = session(f.db, { title: "unrelated" });
  const stranger = message(f.db, other.id, "user", "elsewhere");
  assert.throws(
    () => fork(f.ctx, target.id, { atMessageId: stranger.id }),
    (e: unknown) =>
      e instanceof ForkError && /not .*— fork a session at one of its own/.test(String(e)),
  );
  // And an id that exists nowhere.
  assert.throws(
    () => fork(f.ctx, target.id, { atMessageId: "no-such-message" }),
    (e: unknown) => e instanceof ForkError && e.status === 400,
  );
  f.db.close();
});

test("404: forking a session that does not exist", () => {
  const f = fixture();
  assert.throws(
    () => fork(f.ctx, "no-such-session", { atMessageId: "whatever" }),
    (e: unknown) => (e as { status?: number }).status === 404,
  );
  f.db.close();
});

// ---- inheritance and titling ---------------------------------------------------

test("the branch inherits the source's model and effort pins", () => {
  const f = fixture();
  const { target, own } = scenario(f);
  f.db.setSessionModel(target.id, "openai:gpt-5");
  f.db.setSessionEffort(target.id, "high");
  const pinned = f.db.getSession(target.id)!;
  f.events.length = 0;

  const result = fork(f.ctx, pinned.id, { atMessageId: own[2].id });

  // A resend is a controlled comparison — same history, one changed message. Falling
  // back to the global default would answer it on a different model silently.
  assert.equal(result.session.model, "openai:gpt-5");
  assert.equal(result.session.effort, "high");
  assert.deepEqual(f.db.getSession(result.session.id), result.session);
  // Announced, so a client that only follows events sees the pins too.
  assert.deepEqual(f.events.map((e) => e.type), [
    "session.created",
    "session.updated",
    "message.started",
    "message.started",
    "message.started",
  ]);
  f.db.close();
});

test("a fork of a fork does not compound its title, and a text-free fork point falls back", () => {
  const f = fixture();
  const { target, own } = scenario(f);

  const first = fork(f.ctx, target.id, { atMessageId: own[2].id });
  assert.equal(first.session.title, "fork · second ask");

  // Forking the fork: the excerpt is the branch point's text, not "fork · fork · …".
  const firstOwn = f.db.messagesFor(first.session.id);
  const second = fork(f.ctx, first.session.id, { atMessageId: firstOwn[0].id });
  assert.equal(second.session.title, "fork · first ask");

  // A fork point with no text at all falls back to the source's BASE title.
  const toolOnly = message(f.db, target.id, "supervisor", [
    { type: "tool_call", id: "c9", name: "run_steps", input: {} },
  ]);
  const third = fork(f.ctx, target.id, { atMessageId: toolOnly.id });
  assert.equal(third.session.title, "fork · rename the router");
  f.db.close();
});

test("forking the first message produces an empty-but-real branch", () => {
  const f = fixture();
  const { target, own } = scenario(f);

  const exclusive = fork(f.ctx, target.id, { atMessageId: own[0].id, exclusive: true });
  assert.deepEqual(f.db.messagesFor(exclusive.session.id), []);
  // The ancestors are still inherited: an empty branch is not an empty thread.
  assert.deepEqual(textsOf(f.db.threadFor(exclusive.session.id)), [
    "ancestor question",
    "ancestor answer",
  ]);
  f.db.close();
});

// ---- the route ------------------------------------------------------------------

const TABLE: Route[] = [route("POST", "/sessions/:id/fork", forkSessionH)];

test("POST /sessions/:id/fork answers 201 with the branch and its thread", async () => {
  const f = fixture();
  const { target, own } = scenario(f);
  const call = createHandler(f.ctx, { routes: TABLE });

  const res = await call(
    new Request(`http://x/sessions/${target.id}/fork`, {
      method: "POST",
      body: JSON.stringify({ atMessageId: own[2].id, exclusive: true }),
    }),
  );
  assert.equal(res.status, 201);
  const body = await res.json() as {
    session: Session;
    thread: Message[];
    turnStarted: boolean;
  };
  assert.equal(body.session.kind, "fork");
  assert.equal(body.turnStarted, false);
  // The thread rides along so the client can render the branch it is switching to
  // without an immediate second fetch.
  assert.deepEqual(textsOf(body.thread), [
    "ancestor question",
    "ancestor answer",
    "first ask",
    "first answer",
  ]);
  f.db.close();
});

test("the route maps a bad fork point to 400 and an unknown session to 404", async () => {
  const f = fixture();
  const { target, own } = scenario(f);
  const call = createHandler(f.ctx, { routes: TABLE });
  const post = (id: string, body: unknown) =>
    call(
      new Request(`http://x/sessions/${id}/fork`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    );

  const bad = await post(target.id, { atMessageId: own[1].id, editedText: "nope" });
  assert.equal(bad.status, 400);
  assert.match((await bad.json()).error, /can only replace a user message/);

  const missing = await post("no-such-session", { atMessageId: own[0].id });
  assert.equal(missing.status, 404);

  // A body the schema rejects is the router's 400, not the domain's.
  const malformed = await post(target.id, { atPart: 1 });
  assert.equal(malformed.status, 400);
  assert.match((await malformed.json()).error, /invalid body/);
  f.db.close();
});

test("the route starts the turn through the ctx seam boot wires", async () => {
  const f = fixture("answered on the branch");
  const { target, own } = scenario(f);
  // The seam `server/main.ts` fills — read off the ctx structurally, exactly as
  // `server/sessions.ts` and `schedules.ts` read it.
  const started: string[] = [];
  const ctx = {
    ...f.ctx,
    startTurn: ((c, s, m) => {
      started.push(`${s.id}:${textOf(m)}`);
      return beginTurn(c, s.id, turnDeps()).done;
    }) as ForkStarter,
  };
  const call = createHandler(ctx, { routes: TABLE });

  const res = await call(
    new Request(`http://x/sessions/${target.id}/fork`, {
      method: "POST",
      body: JSON.stringify({ atMessageId: own[2].id, editedText: "try again" }),
    }),
  );
  assert.equal(res.status, 201);
  const body = await res.json() as { session: Session; turnStarted: boolean };
  assert.equal(body.turnStarted, true);
  assert.deepEqual(started, [`${body.session.id}:try again`]);

  // The 201 does not wait for the turn — it reports over /events like any other.
  await new Promise((r) => setTimeout(r, 0));
  f.db.close();
});
