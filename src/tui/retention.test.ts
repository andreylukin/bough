/**
 * What the TUI is allowed to REMEMBER — the growth invariants of `TuiState`.
 *
 * `store.test.ts` asks whether the reducer computes the right state. This file asks
 * a different question about the same reducer: after an hour of use, how much of
 * that state is still there? A TUI is the one process in this system that is
 * expected to run for days, so every container in `TuiState` is either bounded by a
 * documented cap or freed by a documented event — and a container that is neither
 * is a leak, whatever its contents are correct.
 *
 * The method is the one the rest of the suite already uses: replay a recorded event
 * sequence through the pure reducer and assert on the result (plan §7). Nothing here
 * measures a heap. A heap threshold would be both flaky and imprecise — it fails on
 * a busy machine and it cannot say WHICH map grew — whereas a count of retained
 * entries is exact, fast, and names the leak in the assertion message. The workloads
 * below are deliberately adversarial (thousands of log lines, hundreds of session
 * switches), because a leak is by definition invisible at the sizes a normal test
 * uses.
 *
 * The three findings that motivated the file, each now an invariant with a test:
 *
 *   1. **`toolLogs` outlived its reader.** Live program output accumulated per call
 *      id and was released only by a session switch, while `lines.ts` stops reading
 *      it the moment the `tool_result` lands. A session with a chatty test run
 *      retained every line of it, unread, until you switched away.
 *   2. **`workflowLogs` outlived everything.** Keyed by run id — which never recurs
 *      — and cleared by nothing at all, not even the session switch that clears the
 *      `workflows` chips those lines annotate.
 *   3. **`reconciledAt` grew per session browsed.** A watermark is a real fact, but
 *      one for every conversation ever opened is a ledger, and the client only needs
 *      the recent ones (see `RECONCILED_LIMIT`).
 *
 * The already-bounded containers (`seen`, `marks`, `streaming`) are here too. They
 * were correct before this file existed; the tests are what keeps them that way.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import type { BoughEvent, EventType } from "../schema/events.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import type { SessionSnapshot } from "./api.ts";
import {
  createStore,
  DEDUPE_WINDOW,
  initialState,
  MARK_LIMIT,
  RECONCILED_LIMIT,
  reduce,
  type StoreAction,
  type TuiState,
} from "./store.ts";

const SESSION = "sess-1";

// ---- the workload -----------------------------------------------------------

/** Stamped events, as the bus would produce them. Mirrors `store.test.ts`'s recorder. */
class Recorder {
  #seq = 0;
  #ts = 1_000;

  get now(): number {
    return this.#ts;
  }

  emit(type: EventType, data: unknown, sessionId: string | undefined = SESSION): BoughEvent {
    this.#ts += 1;
    return {
      type,
      seq: ++this.#seq,
      ts: this.#ts,
      data,
      ...(sessionId ? { sessionId } : {}),
    };
  }
}

function session(id: string): Session {
  return { id, title: `session ${id}`, kind: "root", createdAt: 1, parentId: null };
}

function message(id: string, sessionId = SESSION, over: Partial<Message> = {}): Message {
  return {
    id,
    sessionId,
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: 1,
    ...over,
  };
}

function snapshot(id: string, thread: Message[] = []): SessionSnapshot {
  const zero = {
    inputTokens: 0,
    outputTokens: 0,
    reasoningTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    costUsd: 0,
  };
  return { session: session(id), thread, usage: { ...zero, tree: { ...zero } } };
}

function apply(state: TuiState, ...actions: StoreAction[]): TuiState {
  return actions.reduce(reduce, state);
}

function deliver(state: TuiState, ...events: BoughEvent[]): TuiState {
  return events.reduce((s, event) => reduce(s, { type: "event", event }), state);
}

/** A session opened and snapshotted — the state every workload below starts from. */
function openSession(rec: Recorder, id = SESSION): TuiState {
  return apply(
    initialState(),
    { type: "open", sessionId: id },
    { type: "snapshot", at: rec.now, snapshot: snapshot(id) },
  );
}

/**
 * One complete round: a message, a tool call, `lines` of live output, then the
 * finalized result — exactly the shape `turn/runner.ts` publishes.
 */
function round(rec: Recorder, n: number, lines: number, sessionId = SESSION): BoughEvent[] {
  const messageId = `${sessionId}-msg-${n}`;
  const callId = `${sessionId}-call-${n}`;
  const call: Part = { type: "tool_call", id: callId, name: "run_steps", input: { code: "1" } };
  const result: Part = { type: "tool_result", callId, output: "ok", isError: false };
  const out: BoughEvent[] = [
    rec.emit("message.started", message(messageId, sessionId), sessionId),
    rec.emit("message.part", { messageId, part: call }, sessionId),
  ];
  for (let i = 0; i < lines; i++) {
    out.push(rec.emit("tool.log", { messageId, callId, line: `line ${i} of round ${n}` }, sessionId));
  }
  out.push(rec.emit("message.part", { messageId, part: result }, sessionId));
  out.push(rec.emit("message.finished", { messageId }, sessionId));
  return out;
}

/**
 * Every unbounded-by-nature container, counted. The assertion message names the
 * container, so a future leak reports itself instead of showing up as a number.
 */
function footprint(state: TuiState) {
  return {
    toolLogKeys: Object.keys(state.toolLogs).length,
    toolLogLines: Object.values(state.toolLogs).reduce((n, ls) => n + ls.length, 0),
    streamingKeys: Object.keys(state.streaming).length,
    workflowLogKeys: Object.keys(state.workflowLogs).length,
    watermarks: Object.keys(state.reconciledAt).length,
    seen: state.seen.length,
    marks: state.marks.length,
  };
}

// ---- 1. live tool output ----------------------------------------------------

test("a call's live output is freed the moment its result lands", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  const call: Part = { type: "tool_call", id: "call-1", name: "run_steps", input: { code: "1" } };

  state = deliver(
    state,
    rec.emit("message.started", message("msg-1")),
    rec.emit("message.part", { messageId: "msg-1", part: call }),
  );
  for (let i = 0; i < 5_000; i++) {
    state = deliver(state, rec.emit("tool.log", { messageId: "msg-1", callId: "call-1", line: `${i}` }));
  }
  // While the call is running the buffer IS the only copy of that output — it must
  // be here, or the live view has nothing to render.
  assert.equal(state.toolLogs["call-1"]?.length, 5_000, "live output must be retained while running");

  const result: Part = { type: "tool_result", callId: "call-1", output: "ok", isError: false };
  state = deliver(state, rec.emit("message.part", { messageId: "msg-1", part: result }));

  assert.equal(state.toolLogs["call-1"], undefined, "the buffer must be released with the result");
  assert.equal(footprint(state).toolLogLines, 0);
  // Released, not lost: the finalized result carries the same output.
  const parts = state.thread.find((m) => m.id === "msg-1")?.parts ?? [];
  assert.ok(parts.some((p) => p.type === "tool_result" && p.callId === "call-1"));
});

test("a long session retains no output from rounds that finished", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  for (let n = 0; n < 50; n++) state = deliver(state, ...round(rec, n, 200));

  const f = footprint(state);
  assert.equal(f.toolLogKeys, 0, "every settled call must have released its buffer");
  assert.equal(f.toolLogLines, 0, `10,000 log lines were retained after their rounds ended`);
  assert.equal(f.streamingKeys, 0, "no live text buffer outlives its finalized part");
  assert.equal(state.thread.length, 50, "the transcript itself is the thing that keeps growing");
});

test("output of a call with no result survives, and a session switch frees it", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  const call: Part = { type: "tool_call", id: "call-9", name: "run_steps", input: { code: "1" } };
  state = deliver(
    state,
    rec.emit("message.started", message("msg-9")),
    rec.emit("message.part", { messageId: "msg-9", part: call }),
    rec.emit("tool.log", { messageId: "msg-9", callId: "call-9", line: "still going" }),
  );
  // An interrupted or still-running call never gets a result. Keeping its lines is
  // correct — they are on screen — so the session switch is the release path.
  assert.equal(state.toolLogs["call-9"]?.length, 1);

  state = apply(state, { type: "open", sessionId: "sess-2" });
  assert.deepEqual(state.toolLogs, {}, "the previous session's live output must not follow you");
});

// ---- 2. workflow narrator lines ---------------------------------------------

test("workflow log lines do not outlive the session that ran them", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  for (let i = 0; i < 200; i++) {
    state = deliver(state, rec.emit("workflow.log", { runId: `run-${i}`, line: `phase ${i}` }));
  }
  assert.equal(footprint(state).workflowLogKeys, 200, "a live run's narrator line is state");

  state = apply(state, { type: "open", sessionId: "sess-2" });
  assert.equal(
    footprint(state).workflowLogKeys,
    0,
    "run ids never recur, so a line kept past its session is unreachable forever",
  );
});

test("browsing many sessions accumulates no workflow lines", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  for (let s = 0; s < 100; s++) {
    state = deliver(state, rec.emit("workflow.log", { runId: `run-${s}`, line: "working" }));
    state = apply(state, { type: "open", sessionId: `sess-${s}` });
  }
  assert.ok(
    footprint(state).workflowLogKeys <= 1,
    `100 session switches left ${footprint(state).workflowLogKeys} workflow log lines behind`,
  );
});

// ---- 3. snapshot watermarks -------------------------------------------------

test("snapshot watermarks are capped, newest kept", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  const total = RECONCILED_LIMIT * 8;
  for (let i = 0; i < total; i++) {
    state = apply(state, { type: "snapshot", at: 10_000 + i, snapshot: snapshot(`other-${i}`) });
  }

  const kept = Object.keys(state.reconciledAt);
  assert.equal(kept.length, RECONCILED_LIMIT, `browsing ${total} sessions kept ${kept.length} watermarks`);
  // The newest survive: those are the sessions a re-delivered event could still name.
  assert.ok(state.reconciledAt[`other-${total - 1}`] !== undefined, "the newest watermark must survive");
  assert.equal(state.reconciledAt["other-0"], undefined, "the oldest must have been evicted");
});

test("the open session's watermark is never evicted", () => {
  const rec = new Recorder();
  // The open session is snapshotted FIRST, so it is the oldest by timestamp and the
  // first thing a naive oldest-first eviction would drop — while being the one
  // session whose events are still arriving.
  let state = openSession(rec);
  state = apply(state, { type: "snapshot", at: 5_000, snapshot: snapshot(SESSION) });
  for (let i = 0; i < RECONCILED_LIMIT * 4; i++) {
    state = apply(state, { type: "snapshot", at: 10_000 + i, snapshot: snapshot(`other-${i}`) });
  }

  assert.equal(state.reconciledAt[SESSION], 5_000, "the open session's watermark must be kept");
  assert.equal(Object.keys(state.reconciledAt).length, RECONCILED_LIMIT);
  // And it still does its job: an event older than the snapshot is dropped.
  const stale: BoughEvent = { type: "message.finished", seq: 1, ts: 4_000, sessionId: SESSION, data: { messageId: "old" } };
  assert.equal(deliver(state, stale), state, "the surviving watermark must still drop stale events");
});

// ---- 4. the bounds that were already right ----------------------------------

test("the dedupe window and the mark ledger stay at their caps", () => {
  const rec = new Recorder();
  let state = openSession(rec);
  for (let i = 0; i < DEDUPE_WINDOW * 4; i++) {
    state = deliver(state, rec.emit("session.activity", { sessionId: SESSION, activity: `step ${i}` }));
  }
  assert.equal(state.seen.length, DEDUPE_WINDOW, "the dedupe window is a window, not a ledger");

  for (let i = 0; i < MARK_LIMIT * 3; i++) {
    state = apply(state, { type: "mark", sessionId: SESSION, at: 20_000 + i, text: `reverted f${i}` });
  }
  assert.equal(state.marks.length, MARK_LIMIT, "marks are capped");
  assert.ok(
    state.marks[state.marks.length - 1].text.endsWith(`f${MARK_LIMIT * 3 - 1}`),
    "the cap must drop the OLDEST marks, keeping the recent ones",
  );
});

// ---- 5. the whole footprint, under a long day's use -------------------------

test("a long day of use leaves every container bounded", () => {
  const rec = new Recorder();
  let state = openSession(rec);

  // 40 rounds in each of 25 conversations, with a workflow narrating throughout:
  // far past what a real day does, and every container must still be small.
  for (let s = 0; s < 25; s++) {
    const id = `sess-${s}`;
    state = apply(
      state,
      { type: "open", sessionId: id },
      { type: "snapshot", at: rec.now, snapshot: snapshot(id) },
    );
    for (let n = 0; n < 40; n++) {
      state = deliver(state, ...round(rec, n, 50, id));
      state = deliver(state, rec.emit("workflow.log", { runId: `run-${s}-${n}`, line: "phase" }, id));
    }
  }

  const f = footprint(state);
  assert.equal(f.toolLogLines, 0, `${f.toolLogLines} lines of dead program output retained`);
  assert.equal(f.streamingKeys, 0, `${f.streamingKeys} dead text buffers retained`);
  assert.ok(f.workflowLogKeys <= 40, `${f.workflowLogKeys} workflow lines retained across 1,000 runs`);
  assert.ok(f.watermarks <= RECONCILED_LIMIT, `${f.watermarks} watermarks retained`);
  assert.equal(f.seen, DEDUPE_WINDOW);
  assert.ok(f.marks <= MARK_LIMIT, `${f.marks} marks retained`);
  // The transcript of the OPEN session is the one thing that legitimately holds
  // everything — and it is exactly one session's worth, not twenty-five.
  assert.equal(state.thread.length, 40, "only the open session's thread is held");
});

// ---- 6. the shell: subscribers ----------------------------------------------

test("N subscribe/unsubscribe cycles leave no listener behind", () => {
  // `createStore` performs no I/O until `start()`, so the shell's subscriber
  // bookkeeping is testable with nothing mounted and nothing on the network.
  const store = createStore();
  let live = 0;

  for (let i = 0; i < 50; i++) {
    let calls = 0;
    const off = store.subscribe(() => calls++);
    store.dispatch({ type: "notice", notice: `notice ${i}` });
    assert.equal(calls, 1, `cycle ${i}: the live subscriber must be told`);
    off();
    store.dispatch({ type: "notice", notice: null });
    assert.equal(calls, 1, `cycle ${i}: a released subscriber must never be told again`);
    live += calls - 1;
  }
  assert.equal(live, 0);

  // A detached listener that would throw proves nothing still holds it.
  const off = store.subscribe(() => {
    throw new Error("this listener was released and must not run");
  });
  off();
  store.dispatch({ type: "notice", notice: "after" });
});
