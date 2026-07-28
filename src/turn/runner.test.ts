/**
 * The turn loop, driven end to end with a scripted fake `LlmClient` and a fake
 * program runner. No worker is spawned, no socket is bound, nothing is on the
 * network (plan §7).
 *
 * The load-bearing assertion is the last one in the first test: **a reasoning part
 * persisted by one turn never reaches the provider in a later turn** (plan §6.4).
 * It is asserted against a real transcript the runner itself wrote — not a hand-made
 * fixture — because that is the only version of the claim that stays true when
 * someone changes what gets persisted.
 *
 * The in-turn echo is asserted in the opposite direction in the same test, and the
 * pair is the whole rule: within one turn a reasoning block goes back verbatim
 * (providers reject a tool call whose signed thinking was altered); across turns
 * nothing does.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable here, and a test that cannot run offline does not belong in
 * `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import type { PromptInput } from "../prompt/assemble.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Part, Session, Usage } from "../schema/parts.ts";
import type { AppCtx, LlmBlock, LlmClient, LlmMessage, LlmParams, LlmResult } from "../types.ts";
import {
  beginTurn,
  createTurnStarter,
  DEFAULT_MODEL,
  MAX_STOP_NUDGES,
  type ProgramRun,
  RUN_STEPS,
  STOP,
  TOOLS,
  type TurnDeps,
} from "./runner.ts";
import { TurnRegistry } from "./queue.ts";

// ---- fixtures ---------------------------------------------------------------

/** One scripted round: what the fake model answers, or what it throws. */
interface ScriptedRound {
  content?: LlmBlock[];
  /** Streamed as `message.delta` before the round resolves. */
  deltas?: string[];
  usage?: Usage;
  stopReason?: string;
  throws?: () => unknown;
}

interface FakeLlm {
  client: LlmClient;
  /** Deep snapshots — the runner mutates its `messages` array between rounds. */
  calls: LlmParams[];
}

function scriptedLlm(rounds: ScriptedRound[]): FakeLlm {
  const calls: LlmParams[] = [];
  let i = 0;
  const client: LlmClient = {
    run(params, onText): Promise<LlmResult> {
      calls.push(structuredClone(params));
      const round = rounds[i++];
      if (!round) {
        throw new Error(`the fake model ran out of script after ${i - 1} round(s)`);
      }
      if (round.throws) return Promise.reject(round.throws());
      for (const d of round.deltas ?? []) onText(d);
      return Promise.resolve({
        content: round.content ?? [],
        stopReason: round.stopReason ?? "end_turn",
        ...(round.usage ? { usage: round.usage } : {}),
      });
    },
  };
  return { client, calls };
}

const text = (t: string): LlmBlock => ({ type: "text", text: t });
const reasoning = (t: string, meta?: unknown): LlmBlock => ({ type: "reasoning", text: t, meta });
const runSteps = (id: string, code: string, done?: boolean): LlmBlock => ({
  type: "tool_use",
  id,
  name: RUN_STEPS,
  input: done === undefined ? { code } : { code, done },
});
const stop = (id = "stop-1"): LlmBlock => ({ type: "tool_use", id, name: STOP, input: {} });

interface Fixture {
  db: SqliteDb;
  ctx: AppCtx;
  bus: Bus;
  events: BoughEvent[];
  session: Session;
  registry: TurnRegistry;
  /** Every program the runner asked to execute. */
  programs: ProgramRun[];
  /** Raw errors the runner reported to the server log. */
  reported: unknown[];
}

function fixture(opts: {
  llm: LlmClient;
  program?: (run: ProgramRun) => ProgramResult | Promise<ProgramResult>;
  kind?: Session["kind"];
  model?: string;
} = { llm: scriptedLlm([]).client }): Fixture & { deps: Record<string, unknown> } {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const session: Session = db.createSession({
    id: crypto.randomUUID(),
    title: "test session",
    kind: opts.kind ?? "root",
    createdAt: 1_000,
    parentId: null,
  });
  const ctx: AppCtx = { db, bus, llm: opts.llm, model: opts.model ?? "claude-opus-4-8" };
  const registry = new TurnRegistry();
  const programs: ProgramRun[] = [];
  const reported: unknown[] = [];
  const deps = {
    registry,
    // Collected rather than logged: an intentional failure should not print a
    // stack, and the reporting itself is worth asserting.
    reportError: (err: unknown) => reported.push(err),
    // A stub prompt: what assembly produces is `prompt/assemble.test.ts`'s subject,
    // and reading twenty markdown files here would only couple this test to their text.
    assemble: () => ({ system: "SYSTEM", systemVolatile: "", sections: [] }),
    program: async (run: ProgramRun): Promise<ProgramResult> => {
      programs.push(run);
      return await (opts.program?.(run) ?? { ok: true, logs: [] });
    },
    outageDelayMs: 0,
  };
  return { db, ctx, bus, events, session, registry, programs, reported, deps };
}

function userMessage(db: SqliteDb, sessionId: string, body: string, at = 2_000): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role: "user",
    parts: [{ type: "text", text: body }],
    pending: false,
    createdAt: at,
  });
}

/** Every content block of every message in a payload, flattened. */
function allBlocks(messages: LlmMessage[]): { type: string; [k: string]: unknown }[] {
  return messages.flatMap((m) => m.content as unknown as { type: string }[]);
}

function partsOf(db: SqliteDb, messageId: string): Part[] {
  return db.getMessage(messageId)!.parts;
}

function eventTypes(events: BoughEvent[]): string[] {
  return events.map((e) => e.type);
}

// ---- the multi-round turn ---------------------------------------------------

test("a multi-round turn runs the program, ends on stop, and never replays reasoning", async () => {
  const llm = scriptedLlm([
    // Round 1: thinks, narrates, runs a program.
    {
      content: [
        reasoning("weighing two approaches", { signature: "sig-1" }),
        text("Looking at the file now."),
        runSteps("call-1", "console.log(await bash('ls'))"),
      ],
      deltas: ["Looking at ", "the file now."],
      usage: { inputTokens: 1_000, outputTokens: 40, cacheReadTokens: 200 },
    },
    // Round 2: reports and stops, in the same response (spec §5).
    {
      content: [text("Listed the directory: three files."), stop()],
      usage: { inputTokens: 1_200, outputTokens: 20 },
    },
  ]);
  const f = fixture({
    llm: llm.client,
    program: () => ({ ok: true, logs: ["a.ts", "b.ts", "c.ts"] }),
  });

  // A previous turn's transcript, including a reasoning part. This is what must
  // never come back.
  const prior = f.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: f.session.id,
    role: "supervisor",
    parts: [
      { type: "reasoning", text: "PRIOR-THINKING-DO-NOT-REPLAY" },
      { type: "text", text: "Earlier answer." },
      { type: "tool_call", id: "old-1", name: RUN_STEPS, input: { code: "1" } },
      { type: "tool_result", callId: "old-1", output: "1", isError: false },
    ],
    pending: false,
    createdAt: 1_500,
  });
  userMessage(f.db, f.session.id, "list the files", 2_000);

  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  const outcome = await done;

  // ── the loop ran to a clean end ──
  assert.equal(outcome.status, "done");
  assert.equal(llm.calls.length, 2, "two rounds");
  assert.equal(f.programs.length, 1, "one program");
  assert.equal(f.programs[0].code, "console.log(await bash('ls'))");
  assert.equal(f.programs[0].callId, "call-1");

  // ── the transcript ──
  const parts = partsOf(f.db, message.id);
  assert.deepEqual(parts.map((p) => p.type), [
    "reasoning",
    "text",
    "tool_call",
    "tool_result",
    "text",
  ]);
  const result = parts[3] as Extract<Part, { type: "tool_result" }>;
  assert.equal(result.output, "a.ts\nb.ts\nc.ts");
  assert.equal(result.isError, false);
  assert.equal(result.interrupted, undefined);
  assert.equal(f.db.getMessage(message.id)!.pending, false, "the message is closed");
  // `stop` is loop control: it is never persisted, so it can never replay.
  assert.ok(!parts.some((p) => p.type === "tool_call" && p.name === STOP));

  // ── round 1's payload: the prior turn's reasoning is gone ──
  const round1 = allBlocks(llm.calls[0].messages);
  assert.equal(
    round1.filter((b) => b.type === "reasoning").length,
    0,
    "a persisted reasoning part must not reach the provider",
  );
  assert.ok(
    !JSON.stringify(llm.calls[0]).includes("PRIOR-THINKING-DO-NOT-REPLAY"),
    "not as a reasoning block and not smuggled in as text either",
  );
  // The rest of the prior turn DID replay — the drop is surgical, not a discard.
  assert.ok(round1.some((b) => b.type === "text" && b.text === "Earlier answer."));
  assert.ok(round1.some((b) => b.type === "tool_use" && b.id === "old-1"));
  assert.ok(round1.some((b) => b.type === "tool_result" && b.toolUseId === "old-1"));
  assert.ok(round1.some((b) => b.type === "text" && b.text === "list the files"));

  // ── round 2's payload: the CURRENT turn's reasoning IS echoed, with its meta ──
  // Different rule, same file: a provider that signs thinking rejects a tool call
  // whose thinking was altered, so within a turn the block travels back verbatim.
  const echoed = allBlocks(llm.calls[1].messages).filter((b) => b.type === "reasoning");
  assert.equal(echoed.length, 1);
  assert.deepEqual(echoed[0].meta, { signature: "sig-1" });
  // ...and the pending message itself is not in its own history.
  assert.ok(!JSON.stringify(llm.calls[0].messages).includes(message.id));

  // ── the tools the model saw ──
  assert.deepEqual(llm.calls[0].tools.map((t) => t.name), [RUN_STEPS, STOP]);
  assert.deepEqual(llm.calls[0].tools, TOOLS, "byte-stable across rounds and sessions");

  // ── usage ──
  const turn = f.db.turnForMessage(message.id)!;
  assert.equal(turn.status, "done");
  assert.equal(turn.usage?.inputTokens, 2_200);
  assert.equal(turn.usage?.outputTokens, 60);
  const sessionUsage = f.db.sessionUsage(f.session.id);
  assert.equal(sessionUsage.inputTokens, 2_200);
  assert.equal(sessionUsage.cacheReadTokens, 200);

  // ── events ──
  const types = eventTypes(f.events);
  assert.ok(types.includes("message.started"));
  assert.ok(types.includes("message.delta"));
  assert.ok(types.includes("message.part"));
  assert.equal(types.filter((t) => t === "message.finished").length, 1);
  const finished = f.events.find((e) => e.type === "turn.finished")!;
  assert.deepEqual(finished.data, {
    turnId: turn.id,
    sessionId: f.session.id,
    status: "done",
  });

  // ── THE ACCEPTANCE CRITERION, against a transcript the runner wrote ──
  // A second turn over the same session: the reasoning this turn persisted must be
  // absent from every payload, exactly as the seeded one was.
  const llm2 = scriptedLlm([{ content: [text("Nothing further."), stop("stop-2")] }]);
  const f2 = { ...f, ctx: { ...f.ctx, llm: llm2.client } };
  userMessage(f.db, f.session.id, "anything else?", 5_000);
  await beginTurn(f2.ctx, f.session.id, f.deps).done;

  const replayed = allBlocks(llm2.calls[0].messages);
  assert.equal(
    replayed.filter((b) => b.type === "reasoning").length,
    0,
    "no reasoning part, from any turn, ever reaches the provider payload",
  );
  assert.ok(
    !JSON.stringify(llm2.calls[0]).includes("weighing two approaches"),
    "the text of a stored reasoning part is not replayed under any block type",
  );
  // The turn's own words and its program's result did replay.
  assert.ok(
    replayed.some((b) => b.type === "text" && b.text === "Listed the directory: three files."),
  );
  assert.ok(replayed.some((b) => b.type === "tool_result" && b.content === "a.ts\nb.ts\nc.ts"));
  assert.ok(prior.id !== message.id);

  f.db.close();
});

// ---- ending rules -----------------------------------------------------------

test("a turn that would end mute is nudged for a closing report", async () => {
  const llm = scriptedLlm([
    // Runs a program and tries to stop having said nothing.
    { content: [runSteps("c1", "console.log(1)"), stop()] },
    // Answers the nudge with the report, and stops again.
    { content: [text("Done: printed 1."), stop("stop-2")] },
  ]);
  const f = fixture({ llm: llm.client, program: () => ({ ok: true, logs: ["1"] }) });
  userMessage(f.db, f.session.id, "print one");

  const { message, done } = await Promise.resolve(beginTurn(f.ctx, f.session.id, f.deps));
  assert.equal((await done).status, "done");

  assert.equal(llm.calls.length, 2, "the stop was not honored while the turn was mute");
  // The nudge rides inside the tool_result message, never as a separate turn.
  const second = llm.calls[1].messages.at(-1)!;
  assert.equal(second.role, "user");
  assert.equal(second.content.filter((b) => b.type === "tool_result").length, 1);
  assert.ok(second.content.some((b) => b.type === "text" && /\[harness\]/.test(b.text)));

  // The nudge is loop control: it is never persisted.
  const parts = partsOf(f.db, message.id);
  assert.ok(!JSON.stringify(parts).includes("[harness]"));
  assert.equal(parts.at(-1)!.type, "text");
  f.db.close();
});

test("a persistently mute turn is forced into a text-only round", async () => {
  const llm = scriptedLlm([
    { content: [runSteps("c1", "console.log(1)"), stop()] },
    // Answers the nudge with empty thinking and another stop — the observed failure.
    { content: [reasoning(""), stop("stop-2")] },
    // The forced round has tools forbidden, so it can only speak.
    { content: [text("I printed 1.")] },
  ]);
  const f = fixture({ llm: llm.client, program: () => ({ ok: true, logs: ["1"] }) });
  userMessage(f.db, f.session.id, "print one");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  await done;

  assert.equal(llm.calls.length, 3);
  assert.equal(llm.calls[2].toolChoice, "none", "the last resort forbids tools");
  const parts = partsOf(f.db, message.id);
  assert.equal(parts.at(-1)!.type, "text");
  assert.equal((parts.at(-1) as Extract<Part, { type: "text" }>).text, "I printed 1.");
  f.db.close();
});

test("a turn that trails off without stop is nudged, and the nudges are bounded", async () => {
  // Never calls stop, never calls a tool: the runaway shape.
  const rounds = Array.from({ length: 8 }, () => ({ content: [text("...thinking out loud")] }));
  const llm = scriptedLlm(rounds);
  const f = fixture({ llm: llm.client });
  userMessage(f.db, f.session.id, "hello");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  const outcome = await done;

  assert.equal(outcome.status, "done", "a nudge cap ends the turn, it does not fail it");
  assert.equal(llm.calls.length, MAX_STOP_NUDGES + 1);
  // Every nudge lived in memory only.
  assert.ok(!JSON.stringify(partsOf(f.db, message.id)).includes("[harness]"));
  assert.ok(
    llm.calls[1].messages.some((m) =>
      m.role === "user" && m.content.some((b) => b.type === "text" && /still open/.test(b.text))
    ),
  );
  f.db.close();
});

test("an emitted <stop/> sentinel ends the turn and is stripped from the transcript", async () => {
  const llm = scriptedLlm([{ content: [text("All done.\n<stop/>")] }]);
  const f = fixture({ llm: llm.client });
  userMessage(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  await done;

  assert.equal(llm.calls.length, 1, "the sentinel is honored as a stop, not nudged");
  const parts = partsOf(f.db, message.id);
  assert.equal((parts[0] as Extract<Part, { type: "text" }>).text, "All done.");
  f.db.close();
});

// ---- failure paths ----------------------------------------------------------

test("a program that fails is a tool result the next round can act on, not a turn error", async () => {
  const llm = scriptedLlm([
    { content: [runSteps("c1", "boom()")] },
    { content: [text("That threw; I will try another way."), stop()] },
  ]);
  const f = fixture({
    llm: llm.client,
    program: () => ({
      ok: false,
      logs: ["about to fail"],
      error: "ReferenceError: boom is not defined",
    }),
  });
  userMessage(f.db, f.session.id, "run it");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  assert.equal((await done).status, "done");

  const result = partsOf(f.db, message.id).find((p) => p.type === "tool_result")!;
  assert.equal(result.isError, true);
  // Partial output leads: the lines it printed are most of what the model needs.
  assert.match(result.output as string, /^about to fail\n\nReferenceError/);
  f.db.close();
});

test("a malformed run_steps input is refused rather than executed", async () => {
  const llm = scriptedLlm([
    { content: [{ type: "tool_use", id: "c1", name: RUN_STEPS, input: { code: 42 } }] },
    { content: [text("Retrying with a string."), stop()] },
  ]);
  const f = fixture({ llm: llm.client });
  userMessage(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  await done;

  assert.equal(f.programs.length, 0, "nothing was executed");
  const result = partsOf(f.db, message.id).find((p) => p.type === "tool_result")!;
  assert.equal(result.isError, true);
  assert.match(result.output as string, /invalid input for run_steps/);
  f.db.close();
});

test("an unknown tool name is answered, not executed", async () => {
  const llm = scriptedLlm([
    { content: [{ type: "tool_use", id: "c1", name: "read_file", input: {} }] },
    { content: [text("Right, I only have run_steps."), stop()] },
  ]);
  const f = fixture({ llm: llm.client });
  userMessage(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  await done;
  assert.equal(f.programs.length, 0);
  const result = partsOf(f.db, message.id).find((p) => p.type === "tool_result")!;
  assert.match(result.output as string, /unknown tool: read_file/);
  f.db.close();
});

/**
 * `view` is not invented — it is the file-reading host function. A model that
 * reaches for it at the TOOL layer has the right capability and the wrong place,
 * and answering that with a bare "unknown tool" reads as "bough cannot read
 * files". A haiku run drew exactly that conclusion and rebuilt its approach
 * around `bash`, twice, in one turn.
 */
test("a host function called as a tool is told where it actually lives", async () => {
  const llm = scriptedLlm([
    { content: [{ type: "tool_use", id: "c1", name: "view", input: { path: "./x.py" } }] },
    { content: [text("Right, view is a function inside the program."), stop()] },
  ]);
  const f = fixture({ llm: llm.client });
  userMessage(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  await done;
  assert.equal(f.programs.length, 0);
  const out = partsOf(f.db, message.id).find((p) => p.type === "tool_result")!.output as string;
  // It must say the capability EXISTS, where it lives, and how to call it —
  // "the only tools are ..." alone leaves the model to guess the recovery.
  assert.match(out, /`view` IS available/);
  assert.match(out, /host function/);
  assert.match(out, /await view\(/);
  f.db.close();
});

test("a provider failure ends the turn with a message, a closed row and a closed message", async () => {
  const llm = scriptedLlm([{
    throws: () => Object.assign(new Error("Anthropic: 400 bad prompt"), { status: 400 }),
  }]);
  const f = fixture({ llm: llm.client });
  userMessage(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  const outcome = await done;

  assert.equal(outcome.status, "error");
  const stored = f.db.getMessage(message.id)!;
  assert.equal(stored.pending, false, "a failed turn still closes its message");
  assert.match((stored.parts.at(-1) as Extract<Part, { type: "text" }>).text, /⚠︎ Turn failed/);
  const turn = f.db.turnForMessage(message.id)!;
  assert.equal(turn.status, "error");
  assert.ok(turn.error);
  assert.equal(f.reported.length, 1, "the raw error reached the server log, once");
  assert.equal(f.db.busySessionIds().has(f.session.id), false, "the session is free again");
  f.db.close();
});

test("a turn that would overflow the context window fails naming the limit", async () => {
  const llm = scriptedLlm([
    // One enormous round, then the loop should refuse to send another.
    {
      content: [runSteps("c1", "console.log(1)")],
      usage: { inputTokens: 100_000_000, outputTokens: 10 },
    },
  ]);
  const f = fixture({ llm: llm.client, program: () => ({ ok: true, logs: ["1"] }) });
  userMessage(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  const outcome = await done;

  assert.equal(outcome.status, "error");
  assert.equal(llm.calls.length, 1, "the doomed round was never sent");
  const note = (f.db.getMessage(message.id)!.parts.at(-1) as Extract<Part, { type: "text" }>).text;
  assert.match(note, /context window/);
  assert.match(note, /claude-opus-4-8/);
  assert.match(note, /Compact or fork/, "it names the move that resolves it");
  f.db.close();
});

// ---- delegation outcome -----------------------------------------------------

// ---- the server seam --------------------------------------------------------

test("createTurnStarter runs a turn when idle and only queues when busy", async () => {
  const llm = scriptedLlm([
    { content: [text("first answer"), stop("s1")] },
    { content: [text("second answer"), stop("s2")] },
  ]);
  const f = fixture({ llm: llm.client });
  const start = createTurnStarter(f.deps as TurnDeps);

  const first = userMessage(f.db, f.session.id, "one", Date.now());
  start(f.ctx, f.session, first);
  // Synchronous up to the first await, so the session is claimed by now.
  assert.equal(f.registry.isRunning(f.session.id), true);

  // A second call while busy must not start a second turn on the same session.
  const second = userMessage(f.db, f.session.id, "two", Date.now());
  start(f.ctx, f.session, second);
  assert.equal(llm.calls.length, 1, "the busy session started nothing");

  // The queued message drains into a fresh turn of its own.
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(llm.calls.length, 2);
  assert.equal(f.db.turnsForSession(f.session.id).length, 2);
  assert.equal(f.db.busySessionIds().size, 0);
  f.db.close();
});

test("a subagent session records its turn's outcome for the tree view", async () => {
  const ok = scriptedLlm([{ content: [text("Report: did the thing."), stop()] }]);
  const f = fixture({ llm: ok.client, kind: "subagent" });
  userMessage(f.db, f.session.id, "do the thing");
  await beginTurn(f.ctx, f.session.id, f.deps).done;
  assert.equal(f.db.getSession(f.session.id)!.outcomeOk, true);
  f.db.close();
});

test("a session's model pin beats the global default, the way effort does", async () => {
  // Spec §4: `model` and `effort` are per-session OVERRIDES; absent = the global
  // default. `AppCtx.model` IS that global default, so reading it first would make
  // `setSessionModel` a no-op on any install that sets `BOUGH_MODEL` — and the two
  // fields would disagree about their own rule, since `effort` already resolves
  // session-first.
  const llm = scriptedLlm([{ content: [text("done"), stop()] }]);
  const f = fixture({ llm: llm.client, model: "claude-opus-4-8" });
  f.db.setSessionModel(f.session.id, "claude-sonnet-4-5");
  f.db.setSessionEffort(f.session.id, "high");
  userMessage(f.db, f.session.id, "hi");

  await beginTurn(f.ctx, f.session.id, f.deps).done;

  assert.equal(llm.calls[0].model, "claude-sonnet-4-5", "the pin, not the ctx default");
  assert.equal(llm.calls[0].effort, "high");
  f.db.close();
});

test("with no pin, the ctx default wins, and with neither, the built-in does", async () => {
  const pinned = scriptedLlm([{ content: [text("done"), stop()] }]);
  const f = fixture({ llm: pinned.client, model: "claude-opus-4-8" });
  userMessage(f.db, f.session.id, "hi");
  await beginTurn(f.ctx, f.session.id, f.deps).done;
  assert.equal(pinned.calls[0].model, "claude-opus-4-8");
  f.db.close();

  const bare = scriptedLlm([{ content: [text("done"), stop()] }]);
  const g = fixture({ llm: bare.client });
  (g.ctx as { model?: string }).model = undefined;
  userMessage(g.db, g.session.id, "hi");
  await beginTurn(g.ctx, g.session.id, g.deps).done;
  assert.equal(bare.calls[0].model, DEFAULT_MODEL);
  g.db.close();
});

// ---- the workspace note -----------------------------------------------------

test("every turn's prompt is told which checkout it is editing", async () => {
  // The seam this closes: `PromptInput.notes` and `TurnDeps.notes` both existed and
  // nobody filled either, so the model was never told where `bash` starts or where a
  // relative `view()` path resolves — and the program's own cwd is the SERVER's
  // directory, not the workspace, so guessing wrong is silent and reachable.
  const llm = scriptedLlm([{ content: [text("done"), stop("c1")] }]);
  const f = fixture({ llm: llm.client });
  f.db.setSessionWorkspace(f.session.id, "/checkouts/acme");

  let seen: PromptInput | undefined;
  userMessage(f.db, f.session.id, "hi");
  await beginTurn(f.ctx, f.session.id, {
    ...f.deps,
    assemble: (input: PromptInput) => {
      seen = input;
      return { system: "SYSTEM", systemVolatile: "", sections: [] };
    },
  } as TurnDeps).done;

  const notes = [...(seen?.notes ?? [])];
  assert.ok(notes.length > 0, "the turn must supply at least the workspace note");
  assert.ok(
    notes[0].includes("/checkouts/acme"),
    `the workspace note must name the session's checkout, got: ${notes[0]}`,
  );
});

test("a caller's own notes are kept, and the workspace note leads", async () => {
  const llm = scriptedLlm([{ content: [text("done"), stop("c1")] }]);
  const f = fixture({ llm: llm.client });
  f.db.setSessionWorkspace(f.session.id, "/checkouts/acme");

  let seen: PromptInput | undefined;
  userMessage(f.db, f.session.id, "hi");
  await beginTurn(f.ctx, f.session.id, {
    ...f.deps,
    notes: ["## Project rules\n\nno emoji"],
    assemble: (input: PromptInput) => {
      seen = input;
      return { system: "SYSTEM", systemVolatile: "", sections: [] };
    },
  } as TurnDeps).done;

  const notes = [...(seen?.notes ?? [])];
  assert.equal(notes.length, 2);
  assert.ok(notes[0].startsWith("## Workspace"));
  assert.ok(notes[1].includes("no emoji"), "a caller's notes must survive");
});
