/**
 * Interrupt, queueing, and the retry ring — the three consequences of "one turn per
 * session" (spec §5).
 *
 * Three of these tests are the task's acceptance criteria and each asserts a
 * different kind of loss:
 *
 *   - **Interrupt mid-program leaves a well-formed transcript.** The failure it
 *     guards is a `tool_call` with no `tool_result`, which every provider rejects —
 *     one stop would make the session unreplayable forever. The partial output has
 *     to survive too, marked `interrupted` rather than `isError`, because "you
 *     stopped it" and "it failed" are different facts for the user and for the next
 *     round.
 *   - **Two rapid messages produce two ordered turns with no loss.** The failure is
 *     a dropped message or two turns racing on one transcript.
 *   - **A truncated tool call retries rather than executing.** The failure is the
 *     worst one in the file: executing a call whose input was cut off runs *the
 *     wrong program* against the user's real checkout. Asserted by looking at what
 *     the program runner was actually handed.
 *
 * Everything is offline: a fake `LlmClient`, a fake program runner, an in-memory
 * database, no worker and no socket.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { LlmError } from "../errors.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import type { BoughEvent, MessageRetryData } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx, LlmBlock, LlmClient, LlmParams, LlmResult } from "../types.ts";
import { beginTurn, type ProgramRun, RUN_STEPS, STOP, type TurnDeps } from "./runner.ts";
import {
  abortableDelay,
  classifyRoundFailure,
  hasUnansweredInput,
  isTruncatedToolCall,
  MAX_ROUND_RETRIES,
  shouldDrain,
  TurnRegistry,
} from "./queue.ts";

// ---- fixtures ---------------------------------------------------------------

const STUB_PROMPT = () => ({ system: "SYSTEM", systemVolatile: "", sections: [] as never[], shas: [] });

const text = (t: string): LlmBlock => ({ type: "text", text: t });
const runSteps = (id: string, code: string): LlmBlock => ({
  type: "tool_use",
  id,
  name: RUN_STEPS,
  input: { code },
});
const stop = (id = "stop-1"): LlmBlock => ({ type: "tool_use", id, name: STOP, input: {} });

interface ScriptedRound {
  content?: LlmBlock[];
  throws?: () => unknown;
}

function scriptedLlm(rounds: ScriptedRound[]): { client: LlmClient; calls: LlmParams[] } {
  const calls: LlmParams[] = [];
  let i = 0;
  return {
    calls,
    client: {
      run(params): Promise<LlmResult> {
        calls.push(structuredClone(params));
        const round = rounds[i++];
        if (!round) throw new Error(`the fake model ran out of script after ${i - 1} round(s)`);
        if (round.throws) return Promise.reject(round.throws());
        return Promise.resolve({ content: round.content ?? [], stopReason: "tool_use" });
      },
    },
  };
}

interface Fixture {
  db: SqliteDb;
  bus: Bus;
  ctx: AppCtx;
  events: BoughEvent[];
  session: Session;
  registry: TurnRegistry;
  programs: ProgramRun[];
  deps: TurnDeps;
}

function fixture(opts: {
  llm: LlmClient;
  program?: (run: ProgramRun) => Promise<ProgramResult>;
}): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "queue test",
    kind: "root",
    createdAt: Date.now(),
    parentId: null,
  });
  const registry = new TurnRegistry();
  const programs: ProgramRun[] = [];
  const deps: TurnDeps = {
    registry,
    assemble: STUB_PROMPT,
    outageDelayMs: 0,
    reportError: () => {},
    program: (run) => {
      programs.push(run);
      return opts.program?.(run) ?? Promise.resolve({ ok: true, logs: [] });
    },
  };
  return {
    db,
    bus,
    events,
    session,
    registry,
    programs,
    deps,
    ctx: { db, bus, llm: opts.llm, model: "claude-opus-4-8" },
  };
}

function post(db: SqliteDb, sessionId: string, body: string): Message {
  return db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role: "user",
    parts: [{ type: "text", text: body }],
    pending: false,
    createdAt: Date.now(),
  });
}

// ---- AC 1: interrupt mid-program --------------------------------------------

test("interrupting mid-program leaves a well-formed, replayable transcript", async () => {
  const llm = scriptedLlm([{
    content: [text("Starting the build."), runSteps("c1", "await bash('make')")],
  }]);
  let started!: () => void;
  const reachedProgram = new Promise<void>((r) => started = r);

  const f = fixture({
    llm: llm.client,
    // A program that runs until the turn's signal fires, then reports what
    // survived — the shape `runProgram` produces on an abort (harness/vm.ts).
    program: (run) =>
      new Promise<ProgramResult>((resolve) => {
        started();
        run.onLog("compiling…");
        run.signal.addEventListener("abort", () =>
          resolve({
            ok: false,
            interrupted: true,
            logs: ["compiling…"],
            error: "program interrupted by the user — the 1 line(s) it printed before " +
              "stopping are above; anything it had already done still stands",
          }), { once: true });
      }),
  });
  post(f.db, f.session.id, "build it");

  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  await reachedProgram;
  assert.equal(f.registry.isRunning(f.session.id), true);
  assert.equal(f.registry.interrupt(f.session.id), true);

  const outcome = await done;
  assert.equal(outcome.status, "interrupted");

  const stored = f.db.getMessage(message.id)!;
  assert.equal(stored.pending, false, "an interrupted message is closed, not left pending");
  assert.deepEqual(stored.parts.map((p) => p.type), [
    "text",
    "tool_call",
    "tool_result",
    "text",
  ]);

  // Every tool_call has its tool_result — the thing that keeps the thread valid.
  const calls = stored.parts.filter((p) => p.type === "tool_call");
  const results = stored.parts.filter((p) => p.type === "tool_result");
  assert.deepEqual(calls.map((c) => c.id), results.map((r) => r.callId));

  const result = results[0] as Extract<Part, { type: "tool_result" }>;
  assert.equal(result.interrupted, true, "stopped, which is not the same as failed");
  assert.match(result.output as string, /compiling…/, "partial output survived");
  assert.match(result.output as string, /interrupted by the user/);

  // The closing note is the stop marker, not a failure marker.
  assert.equal((stored.parts.at(-1) as Extract<Part, { type: "text" }>).text, "⏹ Stopped.");

  const turn = f.db.turnForMessage(message.id)!;
  assert.equal(turn.status, "interrupted");
  assert.equal(turn.error, null, "an interrupt is not an error");
  assert.equal(f.db.busySessionIds().size, 0, "the session is free");
  assert.equal(f.registry.isRunning(f.session.id), false);

  // No further round was asked for: stop means stop.
  assert.equal(llm.calls.length, 1);
  f.db.close();
});

test("an interrupt names the background shells that survive it", async () => {
  const llm = scriptedLlm([{ content: [runSteps("c1", "x")] }]);
  const f = fixture({
    llm: llm.client,
    program: (run) =>
      new Promise<ProgramResult>((resolve) =>
        run.signal.addEventListener(
          "abort",
          () => resolve({ ok: false, interrupted: true, logs: [], error: "interrupted" }),
          { once: true },
        )
      ),
  });
  post(f.db, f.session.id, "go");
  const { message, done } = beginTurn(f.ctx, f.session.id, {
    ...f.deps,
    survivingJobs: () => ["bg_1", "bg_2"],
  });
  await new Promise((r) => setTimeout(r, 0));
  f.registry.interrupt(f.session.id);
  await done;

  const note = (f.db.getMessage(message.id)!.parts.at(-1) as Extract<Part, { type: "text" }>).text;
  assert.match(note, /bg_1, bg_2 still running/);
  assert.match(note, /they survive the interrupt/);
  f.db.close();
});

test("an interrupt cascades to registered detached children, even when the session is idle", () => {
  const registry = new TurnRegistry();
  const stopped: string[] = [];
  const off = registry.onInterrupt("parent", () => stopped.push("child-a"));
  registry.onInterrupt("parent", () => {
    throw new Error("this child is already gone");
  });
  registry.onInterrupt("parent", () => stopped.push("child-b"));

  // No turn running: a detached child outlives its spawner's turn (spec §7), and
  // an explicit stop still has to reach it.
  assert.equal(registry.isRunning("parent"), false);
  assert.equal(registry.interrupt("parent"), true);
  assert.deepEqual(stopped, ["child-a", "child-b"], "a throwing hook does not stop the cascade");

  off();
  registry.interrupt("parent");
  assert.deepEqual(stopped, ["child-a", "child-b", "child-b"]);
  assert.equal(registry.interrupt("nobody"), false);
});

test("a session cannot run two turns at once", () => {
  const registry = new TurnRegistry();
  const first = registry.begin("s1");
  assert.throws(() => registry.begin("s1"), /already running/);
  registry.end("s1", new AbortController()); // a stale end from a superseded turn
  assert.equal(registry.isRunning("s1"), true, "identity-checked");
  registry.end("s1", first);
  assert.equal(registry.isRunning("s1"), false);
});

// ---- AC 2: two rapid messages ------------------------------------------------

test("two rapid messages produce two ordered turns with no loss", async () => {
  // Turn 1 answers the first message; turn 2 answers the second. The fake hands
  // each round back only after the test has had a chance to post.
  const llm = scriptedLlm([
    { content: [text("Answering the first."), stop("s1")] },
    { content: [text("Answering the second."), stop("s2")] },
  ]);
  const f = fixture({ llm: llm.client });

  const drains: string[] = [];
  const deps: TurnDeps = {
    ...f.deps,
    // Observed rather than replaced: the real recursion still runs, so this test
    // exercises the actual drain path.
    startNext: (ctx, sessionId) => {
      drains.push(sessionId);
      void beginTurn(ctx, sessionId, deps).done;
    },
  };

  post(f.db, f.session.id, "first");
  const first = beginTurn(f.ctx, f.session.id, deps);
  // The second message lands while turn 1 is in flight: persisted like any other,
  // and NOT started — `server/sessions.ts` sees a busy session and 202s.
  assert.equal(f.db.busySessionIds().has(f.session.id), true);
  post(f.db, f.session.id, "second");
  assert.equal(hasUnansweredInput(f.db, f.session.id), true);

  await first.done;
  // The drain is synchronous with the first turn's release, so by the time `done`
  // settles the second turn has been started; let it finish.
  await new Promise((r) => setTimeout(r, 10));

  assert.deepEqual(drains, [f.session.id], "exactly one drain, not one per message");
  assert.equal(llm.calls.length, 2);

  // Two turns, in order, both finished.
  const turnRows = f.db.turnsForSession(f.session.id);
  assert.equal(turnRows.length, 2);
  assert.deepEqual(turnRows.map((t) => t.status), ["done", "done"]);
  assert.equal(f.db.busySessionIds().size, 0);

  // The transcript alternates and nothing was dropped.
  const own = f.db.messagesFor(f.session.id);
  assert.deepEqual(own.map((m) => m.role), ["user", "supervisor", "user", "supervisor"]);
  assert.deepEqual(
    own.map((m) => (m.parts[0] as Extract<Part, { type: "text" }>).text),
    ["first", "Answering the first.", "second", "Answering the second."],
  );
  assert.equal(own.every((m) => !m.pending), true);

  // Turn 2 saw the queued message; turn 1 could not have.
  const round1 = JSON.stringify(llm.calls[0].messages);
  const round2 = JSON.stringify(llm.calls[1].messages);
  assert.ok(round1.includes("first"));
  assert.ok(!round1.includes("second"), "the queued message did not race into the live turn");
  assert.ok(round2.includes("first") && round2.includes("second"));
  assert.ok(round2.indexOf("Answering the first.") < round2.indexOf("second"), "in order");

  // And it stops: nothing is left unanswered, so no third turn starts.
  assert.equal(hasUnansweredInput(f.db, f.session.id), false);
  f.db.close();
});

test("the drain condition is derived from the transcript, not from an in-memory flag", () => {
  const db = openDb(":memory:");
  const registry = new TurnRegistry();
  const session = db.createSession({
    id: crypto.randomUUID(),
    title: "t",
    kind: "root",
    createdAt: Date.now(),
    parentId: null,
  });

  assert.equal(hasUnansweredInput(db, session.id), false, "an empty session owes nothing");

  post(db, session.id, "hello");
  assert.equal(hasUnansweredInput(db, session.id), true);

  db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "supervisor",
    parts: [{ type: "text", text: "hi" }],
    pending: false,
    createdAt: Date.now(),
  });
  assert.equal(hasUnansweredInput(db, session.id), false);

  // A harness note (a subagent's report, a job exit) owes a turn just as a user
  // message does — that is how a finished background child wakes its spawner.
  db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "system",
    parts: [{ type: "text", text: "[subagent finished] …" }],
    pending: false,
    createdAt: Date.now(),
  });
  assert.equal(hasUnansweredInput(db, session.id), true);

  // The explicit nudge is take-and-clear, and it is an OR with the derived check.
  registry.enqueue(session.id);
  assert.equal(shouldDrain(db, session.id, registry), true);
  assert.equal(shouldDrain(db, session.id, registry), true, "still owed by the transcript");
  db.close();
});

// ---- AC 3: the truncated tool call ------------------------------------------

test("a tool call truncated mid-stream is retried, never executed", async () => {
  // What `llm/stream.ts` raises rather than falling back to `{}`.
  const truncation = () =>
    new LlmError("anthropic: run_steps call arrived with no arguments (truncated mid-call)");

  const llm = scriptedLlm([
    { throws: truncation },
    // The re-streamed round lands intact.
    { content: [runSteps("c1", "await bash('git status')")] },
    { content: [text("Clean tree."), stop()] },
  ]);
  const f = fixture({
    llm: llm.client,
    program: () => Promise.resolve({ ok: true, logs: ["nothing to commit"] }),
  });
  post(f.db, f.session.id, "check the tree");

  const { message, done } = beginTurn(f.ctx, f.session.id, f.deps);
  assert.equal((await done).status, "done");

  // THE assertion: the program that ran is the one the model actually wrote.
  assert.equal(f.programs.length, 1, "the truncated call was not executed");
  assert.equal(f.programs[0].code, "await bash('git status')");
  assert.equal(llm.calls.length, 3, "the round was re-streamed");

  // The retry is announced, so a client drops the partial text it had buffered.
  const retries = f.events.filter((e) => e.type === "message.retry");
  assert.equal(retries.length, 1);
  const data = retries[0].data as MessageRetryData;
  assert.equal(data.messageId, message.id);
  assert.equal(data.attempt, 1);
  assert.match(data.reason, /cut off mid-stream/);
  assert.match(data.reason, /rather than executing a truncated program/);

  // Nothing about the retry reached the transcript beyond the real round.
  const parts = f.db.getMessage(message.id)!.parts;
  assert.deepEqual(parts.map((p) => p.type), ["tool_call", "tool_result", "text"]);
  f.db.close();
});

test("an exhausted retry surfaces as a turn error rather than an executed guess", async () => {
  const truncation = () =>
    new LlmError("openai: run_steps call has malformed arguments (truncated mid-call)");
  const llm = scriptedLlm([
    { throws: truncation },
    { throws: truncation },
    { throws: truncation },
    { throws: truncation },
  ]);
  const f = fixture({ llm: llm.client });
  post(f.db, f.session.id, "go");

  const outcome = await beginTurn(f.ctx, f.session.id, f.deps).done;
  assert.equal(outcome.status, "error");
  assert.equal(f.programs.length, 0, "still never executed");
  assert.equal(llm.calls.length, MAX_ROUND_RETRIES + 1, "retries are bounded");
  assert.equal(f.db.busySessionIds().size, 0);
  f.db.close();
});

test("classification: what retries, what waits, and what does not", () => {
  const truncated = new LlmError(
    "provider: run_steps call arrived with no arguments (truncated mid-call)",
  );
  assert.equal(isTruncatedToolCall(truncated), true);
  const first = classifyRoundFailure(truncated, 1, { outageDelayMs: 60_000 });
  assert.equal(first.retry, true);
  assert.equal(first.delayMs, 0, "a lost frame is not an outage — re-stream now");

  // A provider outage waits, because the client's own backoff is already spent.
  const outage = new LlmError("provider: 503 upstream unavailable", 503);
  const second = classifyRoundFailure(outage, 1, { outageDelayMs: 60_000 });
  assert.equal(second.retry, true);
  assert.equal(second.delayMs, 60_000);

  // A caller's own mistake is not retried: six attempts only delay the message
  // that explains it.
  assert.equal(classifyRoundFailure(new LlmError("bad request", 400), 1).retry, false);

  // The user's stop is an answer, not a failure.
  const abort = new DOMException("aborted", "AbortError");
  assert.equal(classifyRoundFailure(abort, 1).retry, false);

  // Bounded.
  assert.equal(classifyRoundFailure(truncated, MAX_ROUND_RETRIES + 1).retry, false);

  // The reason is one bounded line — it goes straight into an event payload.
  const noisy = new LlmError(`provider: 500 ${"x".repeat(500)}\nand\nmore`, 500);
  const reason = classifyRoundFailure(noisy, 1).reason;
  assert.ok(reason.length <= 120);
  assert.ok(!reason.includes("\n"));
});

test("a retry wait is cut short by an interrupt", async () => {
  const controller = new AbortController();
  const waited = abortableDelay(60_000, controller.signal);
  controller.abort();
  await assert.rejects(() => waited, (err: Error) => err.name === "AbortError");

  // Already aborted, and the zero case.
  await assert.rejects(
    () => abortableDelay(0, AbortSignal.abort()),
    (err: Error) => err.name === "AbortError",
  );
  await abortableDelay(0);
});
