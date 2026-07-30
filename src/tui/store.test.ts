/**
 * Tests for the TUI's data layer.
 *
 * Everything here runs the reducer directly over a RECORDED event sequence — no Ink,
 * no terminal, no socket, no server (plan §7). That is the acceptance criterion for
 * T9.1 and it is also the point of the module: if a reducer needs a renderer mounted
 * to be exercised, the boundary this task exists to draw is not there.
 *
 * The load-bearing cases, and why each one is here:
 *
 *   - **Disconnect → snapshot → re-delivery.** The reconnect path re-fetches and
 *     reconciles by message id; it does not replay from a seq (spec §3, plan §6.16).
 *     So a client necessarily sees, twice, everything the snapshot already contained.
 *     The test re-delivers the entire recorded log after the snapshot and asserts the
 *     transcript is byte-identical to the single-delivery case: no duplicate message,
 *     no duplicate part, no doubled delta.
 *   - **No lost deltas.** The other half of the same property, and the easy one to
 *     break by over-deduping: text streamed before the outage survives the snapshot
 *     while the message is still pending, and text streamed after it is appended
 *     exactly once.
 *   - **`seq` resets on restart.** The reason the dedupe key is `seq:ts` and not
 *     `seq`. After a restart the server's first event is `seq: 1` again; a client that
 *     deduped on seq alone would silently drop every event of the new process.
 *   - **Derived visibility.** A `subagent` session announced on the stream must not
 *     appear in the top-level list (spec §4) — the client derives it the same way the
 *     server does, and stores nothing.
 *   - **The shell.** A fake `Api` and a fake event stream prove the wiring: a
 *     reconnect triggers exactly one re-fetch per session, and the events that arrive
 *     around it still land exactly once.
 *
 * `node:assert/strict`, not `@std/assert`: jsr.io is unreachable here and a test that
 * cannot run offline does not belong in `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import type { BoughEvent, EventType } from "../schema/events.ts";
import type { AskQuestion, Message, Part, Session } from "../schema/parts.ts";
import type { Api, SessionRow, SessionSnapshot } from "./api.ts";
import {
  createStore,
  currentAsk,
  DEDUPE_WINDOW,
  eventKey,
  initialState,
  isBusy,
  isDuplicate,
  type LiveUnit,
  liveUnits,
  marksFor,
  mergeThread,
  partKey,
  reduce,
  type StoreAction,
  type TuiState,
} from "./store.ts";
import type { EventStream, EventStreamOptions } from "./events.ts";
import { unitLine } from "./format.ts";

// ---- the recorder -----------------------------------------------------------

const SESSION = "sess-1";
const OTHER = "sess-2";

/**
 * A stamped event, exactly as the bus would have produced it: `seq` monotonic from 1,
 * `ts` the wall clock at publish. The recorder is what makes "replay the same log
 * twice" a one-liner in the tests below.
 */
class Recorder {
  #seq = 0;
  #ts: number;
  readonly log: BoughEvent[] = [];

  constructor(startTs = 1_000) {
    this.#ts = startTs;
  }

  get now(): number {
    return this.#ts;
  }

  /** Advance the clock without publishing — the outage, the round trip. */
  tick(ms: number): void {
    this.#ts += ms;
  }

  emit(type: EventType, data: unknown, sessionId: string | undefined = SESSION): BoughEvent {
    this.#ts += 1;
    const event: BoughEvent = {
      type,
      seq: ++this.#seq,
      ts: this.#ts,
      data,
      ...(sessionId ? { sessionId } : {}),
    };
    this.log.push(event);
    return event;
  }

  /** Restart the server under the client: `seq` resets, the clock does not. */
  restart(): void {
    this.#seq = 0;
  }
}

function session(id: string, over: Partial<Session> = {}): Session {
  return {
    id,
    title: `session ${id}`,
    kind: "root",
    createdAt: 1,
    parentId: null,
    ...over,
  };
}

function row(id: string, over: Partial<SessionRow> = {}): SessionRow {
  return { ...session(id), busy: false, ...over };
}

function message(id: string, over: Partial<Message> = {}): Message {
  return {
    id,
    sessionId: SESSION,
    role: "supervisor",
    parts: [],
    pending: false,
    createdAt: 1,
    ...over,
  };
}

const TOOL_CALL: Part = {
  type: "tool_call",
  id: "call-1",
  name: "run_steps",
  input: { code: "1" },
};
const TOOL_RESULT: Part = { type: "tool_result", callId: "call-1", output: "ok", isError: false };
const TEXT: Part = { type: "text", text: "Looking…!" };

/** Apply a whole log, in order, through the `event` action. */
function replay(state: TuiState, events: readonly BoughEvent[]): TuiState {
  return events.reduce((s, event) => reduce(s, { type: "event", event }), state);
}

function apply(state: TuiState, ...actions: StoreAction[]): TuiState {
  return actions.reduce(reduce, state);
}

/**
 * The whole recorded conversation, split at the outage.
 *
 * `before` is what the client saw live; `missed` is what the server published while
 * the stream was down; `after` is what arrives once it is back. `log` is all of it,
 * which is what gets re-delivered.
 */
function record() {
  const rec = new Recorder();
  const user = message("m-user", { role: "user", parts: [{ type: "text", text: "go" }] });
  const supervisor = message("m-1", { pending: true });

  const before = [
    rec.emit("session.created", session(SESSION)),
    rec.emit("message.started", user),
    rec.emit("message.started", supervisor),
    rec.emit("message.delta", { messageId: "m-1", delta: "Look" }),
    rec.emit("message.delta", { messageId: "m-1", delta: "ing…" }),
    rec.emit("tool.log", { messageId: "m-1", callId: "call-1", line: "compiling" }),
    rec.emit("message.part", { messageId: "m-1", part: TOOL_CALL }),
  ];
  // The outage. Both of these are lost to the client and restored by the fetch.
  const missed = [
    rec.emit("message.part", { messageId: "m-1", part: TOOL_RESULT }),
    rec.emit("message.delta", { messageId: "m-1", delta: "!" }),
  ];
  return { rec, user, supervisor, before, missed };
}

/** What `GET /sessions/:id` returns at the moment of the reconnect. */
function snapshotAfterOutage(): SessionSnapshot {
  return {
    session: session(SESSION),
    thread: [
      message("m-user", { role: "user", parts: [{ type: "text", text: "go" }] }),
      // Persisted parts only: the model is still mid-stream, so there is no text part
      // and the message is still pending.
      message("m-1", { parts: [TOOL_CALL, TOOL_RESULT], pending: true }),
    ],
    usage: {
      inputTokens: 10,
      outputTokens: 5,
      reasoningTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      costUsd: 0.01,
      tree: {
        inputTokens: 10,
        outputTokens: 5,
        reasoningTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        costUsd: 0.01,
      },
    },
  };
}

// ---- the acceptance test ----------------------------------------------------

test("reconnect re-delivers applied events: no duplicate messages, no lost deltas", () => {
  const { rec, before, missed } = record();

  // 1. Live, through the outage point.
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, before);

  assert.equal(state.thread.length, 2);
  assert.equal(state.streaming["m-1"], "Looking…");
  assert.deepEqual(state.toolLogs["call-1"], ["compiling"]);

  // 2. The stream drops. Two events are published that this client never sees.
  state = reduce(state, { type: "connection", connected: false });
  void missed;

  // 3. Reconnect: re-fetch and reconcile by message id. The watermark is taken when
  // the request is ISSUED, which is after everything published so far.
  rec.tick(500);
  const at = rec.now;
  rec.tick(20); // the round trip
  state = reduce(state, { type: "snapshot", at, snapshot: snapshotAfterOutage() });
  state = reduce(state, { type: "connection", connected: true });

  // The outage's missed parts are restored by the fetch…
  const supervisorNow = () => state.thread.find((m) => m.id === "m-1")!;
  assert.deepEqual(supervisorNow().parts, [TOOL_CALL, TOOL_RESULT]);
  // …and the delta streamed before the outage survives, because the message is still
  // pending and that text is persisted nowhere else. THIS is "no lost deltas".
  assert.equal(state.streaming["m-1"], "Looking…");

  // 4. THE CASE THIS TEST EXISTS FOR: the whole recorded log is delivered again —
  // every event the client already applied, plus the two the snapshot restored.
  const before4 = state;
  state = replay(state, rec.log);
  assert.equal(state, before4, "a fully re-delivered log must be a no-op, reference and all");

  assert.equal(state.thread.length, 2, "no duplicate message");
  assert.deepEqual(supervisorNow().parts, [TOOL_CALL, TOOL_RESULT], "no duplicate part");
  assert.equal(state.streaming["m-1"], "Looking…", "no doubled delta");
  assert.deepEqual(state.toolLogs["call-1"], ["compiling"], "no doubled tool log");

  // 5. Live again. New events are stamped after the watermark and must all land.
  const live = [
    rec.emit("message.delta", { messageId: "m-1", delta: "!" }),
    rec.emit("message.part", { messageId: "m-1", part: TEXT }),
    rec.emit("message.finished", { messageId: "m-1" }),
    rec.emit("turn.finished", { turnId: "t-1", sessionId: SESSION, status: "done" }),
  ];
  // The delta lands once even though the redialed stream overlapped the old one and
  // delivered it twice — this is the dedupe window, not the watermark: it is NEWER
  // than the snapshot.
  state = replay(state, [live[0], live[0], live[1], live[2], live[3]]);

  const finished = supervisorNow();
  assert.deepEqual(finished.parts, [TOOL_CALL, TOOL_RESULT, TEXT], "one text part, appended once");
  assert.equal(finished.pending, false);
  assert.equal(state.streaming["m-1"], undefined, "the finalized text supersedes the buffer");
  assert.equal(state.thread.length, 2);
  assert.equal(isBusy(state), false);
  const listed = state.sessions.find((s) => s.id === SESSION)!;
  assert.equal(listed.busy, false);
  assert.equal(listed.lastTurnStatus, "done");
});

test("the delta text that reaches the finalized part is neither doubled nor short", () => {
  // The same property stated as arithmetic: the streamed prefix plus the post-snapshot
  // delta is exactly the text the model produced, with the whole log delivered twice.
  const rec = new Recorder();
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, [
    rec.emit("message.started", message("m-1", { pending: true })),
    rec.emit("message.delta", { messageId: "m-1", delta: "one " }),
    rec.emit("message.delta", { messageId: "m-1", delta: "two " }),
  ]);
  rec.tick(100);
  const at = rec.now;
  state = reduce(state, {
    type: "snapshot",
    at,
    snapshot: {
      session: session(SESSION),
      thread: [message("m-1", { pending: true })],
      usage: snapshotAfterOutage().usage,
    },
  });
  state = replay(state, [
    ...rec.log,
    rec.emit("message.delta", { messageId: "m-1", delta: "three" }),
  ]);
  state = replay(state, rec.log); // and once more, for good measure

  assert.equal(state.streaming["m-1"], "one two three");
});

test("`seq` resets on restart, so the dedupe key cannot be `seq` alone", () => {
  const rec = new Recorder();
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, [
    rec.emit("message.started", message("m-1", { pending: true })),
    rec.emit("message.delta", { messageId: "m-1", delta: "before" }),
  ]);

  // The server restarts. The client refetches — the thread is empty again because the
  // turn was orphaned — and the new process starts stamping from seq 1.
  rec.tick(50);
  const at = rec.now;
  state = reduce(state, {
    type: "snapshot",
    at,
    snapshot: {
      session: session(SESSION),
      thread: [message("m-2", { pending: true })],
      usage: snapshotAfterOutage().usage,
    },
  });
  rec.restart();
  rec.tick(10);
  const fresh = rec.emit("message.delta", { messageId: "m-2", delta: "after" });
  assert.equal(fresh.seq, 1, "the recorder must actually reset, or this test proves nothing");

  state = reduce(state, { type: "event", event: fresh });
  assert.equal(state.streaming["m-2"], "after", "a restarted server's seq 1 is not the old seq 1");
});

test("the snapshot watermark drops only events the fetch already contains", () => {
  const state = apply(initialState(), {
    type: "snapshot",
    at: 5_000,
    snapshot: {
      session: session(SESSION),
      thread: [],
      usage: snapshotAfterOutage().usage,
    },
  });
  const older: BoughEvent = {
    type: "message.delta",
    sessionId: SESSION,
    seq: 9,
    ts: 4_999,
    data: {},
  };
  const newer: BoughEvent = {
    type: "message.delta",
    sessionId: SESSION,
    seq: 10,
    ts: 5_000,
    data: {},
  };
  const elsewhere: BoughEvent = {
    type: "message.delta",
    sessionId: OTHER,
    seq: 11,
    ts: 1,
    data: {},
  };
  const global: BoughEvent = { type: "workflow.log", seq: 12, ts: 1, data: {} };

  assert.equal(isDuplicate(state, older), true);
  assert.equal(isDuplicate(state, newer), false, "the boundary is exclusive: `at` itself is live");
  assert.equal(isDuplicate(state, elsewhere), false, "another session has its own watermark");
  assert.equal(isDuplicate(state, global), false, "an un-scoped event is never watermarked away");
});

test("the dedupe window is bounded and keeps the most recent identities", () => {
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  const first: BoughEvent = {
    type: "tool.log",
    sessionId: SESSION,
    seq: 1,
    ts: 1,
    data: { messageId: "m", callId: "c", line: "x" },
  };
  for (let i = 1; i <= DEDUPE_WINDOW + 10; i++) {
    state = reduce(state, {
      type: "event",
      event: {
        type: "tool.log",
        sessionId: SESSION,
        seq: i,
        ts: i,
        data: { messageId: "m", callId: "c", line: "x" },
      },
    });
  }
  assert.equal(state.seen.length, DEDUPE_WINDOW);
  assert.equal(state.seen.includes(eventKey(first)), false, "the oldest identity aged out");
  assert.equal(state.seen[state.seen.length - 1], `${DEDUPE_WINDOW + 10}:${DEDUPE_WINDOW + 10}`);
});

// ---- reducer rules ----------------------------------------------------------

test("a subagent announcement never enters the top-level list", () => {
  let state = initialState();
  state = reduce(state, {
    type: "event",
    event: {
      type: "session.created",
      sessionId: "sub-1",
      seq: 1,
      ts: 1,
      data: session("sub-1", { kind: "subagent", originId: SESSION }),
    },
  });
  assert.equal(state.sessions.length, 0, "delegated work collapses under its origin (spec §4)");

  state = reduce(state, {
    type: "event",
    event: { type: "session.created", sessionId: "root-2", seq: 2, ts: 2, data: session("root-2") },
  });
  assert.deepEqual(state.sessions.map((s) => s.id), ["root-2"]);
});

test("another session's turn marks its row busy without touching the open thread", () => {
  let state = apply(
    initialState(),
    { type: "sessions", sessions: [row(SESSION), row(OTHER)] },
    { type: "open", sessionId: SESSION },
  );
  state = reduce(state, {
    type: "event",
    event: {
      type: "message.started",
      sessionId: OTHER,
      seq: 1,
      ts: 1,
      data: message("m-other", { sessionId: OTHER, pending: true }),
    },
  });
  assert.equal(state.thread.length, 0, "a message of another session is not in this thread");
  assert.equal(state.sessions.find((s) => s.id === OTHER)!.busy, true);
});

test("a background session finishing is announced once, with a distinct seq", () => {
  let state = apply(
    initialState(),
    { type: "sessions", sessions: [row(SESSION), row(OTHER, { busy: true })] },
    { type: "open", sessionId: SESSION },
  );
  state = reduce(state, {
    type: "event",
    event: {
      type: "message.finished",
      sessionId: OTHER,
      seq: 1,
      ts: 1,
      data: { messageId: "m-other" },
    },
  });
  assert.equal(state.background?.sessionId, OTHER);
  assert.equal(state.background?.seq, 1);
  assert.equal(state.sessions.find((s) => s.id === OTHER)!.unseen, true);

  // Opening it clears the mark; a server refetch must not bring it back.
  state = reduce(state, { type: "open", sessionId: OTHER });
  assert.equal(state.sessions.find((s) => s.id === OTHER)!.unseen, false);
  state = reduce(state, { type: "sessions", sessions: [row(SESSION), row(OTHER)] });
  assert.equal(state.sessions.find((s) => s.id === OTHER)!.unseen, undefined);
});

test("a subagent's finish raises no background toast", () => {
  let state = apply(
    initialState(),
    { type: "sessions", sessions: [row(SESSION), row("sub-1", { kind: "subagent", busy: true })] },
    { type: "open", sessionId: SESSION },
  );
  state = reduce(state, {
    type: "event",
    event: {
      type: "message.finished",
      sessionId: "sub-1",
      seq: 1,
      ts: 1,
      data: { messageId: "m" },
    },
  });
  assert.equal(state.background, null, "a subagent finishes inside its spawner's turn — not news");
});

test("message.retry drops the partial text rather than prefixing the re-stream", () => {
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, [
    {
      type: "message.started",
      sessionId: SESSION,
      seq: 1,
      ts: 1,
      data: message("m-1", { pending: true }),
    },
    {
      type: "message.delta",
      sessionId: SESSION,
      seq: 2,
      ts: 2,
      data: { messageId: "m-1", delta: "half a too" },
    },
    {
      type: "message.retry",
      sessionId: SESSION,
      seq: 3,
      ts: 3,
      data: { messageId: "m-1", attempt: 2, reason: "truncated tool call" },
    },
    {
      type: "message.delta",
      sessionId: SESSION,
      seq: 4,
      ts: 4,
      data: { messageId: "m-1", delta: "all of it" },
    },
  ]);
  assert.equal(state.streaming["m-1"], "all of it");
  assert.match(state.notice ?? "", /attempt 2/);
});

test("ask() holds surface oldest-first and settle out of the queue", () => {
  const hold = (id: string, status: AskQuestion["status"]): AskQuestion => ({
    id,
    sessionId: SESSION,
    messageId: "m-1",
    question: `q ${id}`,
    status,
    ts: 1,
  });
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, [
    { type: "ask.question", sessionId: SESSION, seq: 1, ts: 1, data: hold("q1", "pending") },
    { type: "ask.question", sessionId: SESSION, seq: 2, ts: 2, data: hold("q2", "pending") },
  ]);
  assert.equal(currentAsk(state)?.id, "q1");
  assert.equal(state.asks.length, 2);

  // Optimistic settle, then the confirming event. Neither may resurrect it.
  state = reduce(state, { type: "ask.settled", id: "q1" });
  assert.equal(currentAsk(state)?.id, "q2");
  state = reduce(state, {
    type: "event",
    event: {
      type: "ask.question",
      sessionId: SESSION,
      seq: 3,
      ts: 3,
      data: hold("q1", "answered"),
    },
  });
  assert.deepEqual(state.asks.map((q) => q.id), ["q2"]);
});

test("opening a session drops everything that belonged to the previous one", () => {
  let state = apply(
    initialState(),
    { type: "sessions", sessions: [row(SESSION), row(OTHER)] },
    { type: "open", sessionId: SESSION },
    { type: "queue", text: "typed while busy" },
  );
  state = replay(state, [
    {
      type: "message.started",
      sessionId: SESSION,
      seq: 1,
      ts: 1,
      data: message("m-1", { pending: true }),
    },
    {
      type: "session.activity",
      sessionId: SESSION,
      seq: 2,
      ts: 2,
      data: { sessionId: SESSION, activity: "running tests" },
    },
  ]);
  assert.equal(state.activity, "running tests");
  assert.equal(state.queued.length, 1);

  state = reduce(state, { type: "open", sessionId: OTHER });
  assert.deepEqual(state.thread, []);
  assert.deepEqual(state.queued, [], "a staged message belongs to the session it was typed in");
  assert.equal(state.activity, null);
  assert.equal(state.streaming["m-1"], undefined);
});

test("mergeThread keeps stream-only messages and the longer part list", () => {
  const fromDb = [message("a", { parts: [TOOL_CALL] }), message("b")];
  const local = [
    message("a", { parts: [TOOL_CALL, TOOL_RESULT], pending: true }),
    message("c", { pending: true }),
  ];
  const merged = mergeThread(fromDb, local);
  assert.deepEqual(merged.map((m) => m.id), ["a", "b", "c"]);
  assert.deepEqual(merged[0].parts, [TOOL_CALL, TOOL_RESULT]);
  assert.equal(merged[0].pending, false, "finished beats pending — `pending` only ever clears");
});

test("part identity is what makes an append idempotent", () => {
  assert.equal(partKey(TOOL_CALL), "tool_call:call-1");
  assert.equal(partKey(TOOL_RESULT), "tool_result:call-1");
  assert.equal(partKey({ type: "text", text: "hi" }), null);
  assert.equal(partKey({ type: "reasoning", text: "hm" }), null);
});

// ---- the shell --------------------------------------------------------------

/** A fake `Api` that records calls and answers from fixtures. No socket, no server. */
function fakeApi(overrides: Partial<Api> = {}) {
  const calls: string[] = [];
  const base: Partial<Api> = {
    base: "http://127.0.0.1:0",
    eventsUrl: () => "http://127.0.0.1:0/events",
    listSessions: () => {
      calls.push("listSessions");
      return Promise.resolve([row(SESSION)]);
    },
    listQuestions: () => {
      calls.push("listQuestions");
      return Promise.resolve([]);
    },
    getSession: (id: string) => {
      calls.push(`getSession:${id}`);
      return Promise.resolve(snapshotAfterOutage());
    },
    getChanges: () => {
      calls.push("getChanges");
      return Promise.resolve({
        available: false,
        reason: "not a repository",
        base: null,
        files: [],
        workspace: null,
      });
    },
    listJobs: () => {
      calls.push("listJobs");
      return Promise.resolve({ jobs: [] });
    },
    listWorkflows: () => {
      calls.push("listWorkflows");
      return Promise.resolve({ workflows: [] });
    },
    postMessage: (id: string) => {
      calls.push(`postMessage:${id}`);
      return Promise.resolve({
        message: message("m-posted", { role: "user" }),
        queued: false,
      });
    },
  };
  return { api: { ...base, ...overrides } as unknown as Api, calls };
}

/** A fake event stream: hands the store's callbacks back so the test drives them. */
function fakeStream() {
  let opts: EventStreamOptions | null = null;
  const stream: EventStream = {
    connected: true,
    opens: 1,
    close: () => {},
    done: Promise.resolve(),
  };
  const connect = (options: EventStreamOptions): EventStream => {
    opts = options;
    return stream;
  };
  return {
    connect,
    open: (reconnect: boolean) => opts!.onOpen?.({ reconnect, attempt: reconnect ? 2 : 1 }),
    close: () => opts!.onClose?.({ error: null }),
    emit: (event: BoughEvent) => opts!.onEvent(event),
  };
}

/** Let every promise the store kicked off settle. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("the shell re-fetches on RE-connect only, and re-delivered events stay no-ops", async () => {
  const { api, calls } = fakeApi();
  const events = fakeStream();
  let clock = 10_000;
  const store = createStore({ api, connect: events.connect, now: () => clock });

  store.start();
  events.open(false);
  await settle();
  assert.equal(store.getState().connected, true);
  assert.deepEqual(
    calls.filter((c) => c.startsWith("getSession")),
    [],
    "first open must not resync",
  );

  await store.open(SESSION);
  await settle();
  assert.equal(calls.filter((c) => c.startsWith("getSession")).length, 1);
  assert.equal(store.getState().thread.length, 2);

  // Live events on top of the snapshot.
  clock += 100;
  const live: BoughEvent = {
    type: "message.delta",
    sessionId: SESSION,
    seq: 20,
    ts: clock,
    data: { messageId: "m-1", delta: "tail" },
  };
  events.emit(live);
  assert.equal(store.getState().streaming["m-1"], "tail");

  // Drop and redial. The reconnect re-fetches; the redialed stream also re-delivers
  // the event that was already applied.
  events.close();
  assert.equal(store.getState().connected, false);
  clock += 100;
  events.open(true);
  await settle();
  assert.equal(calls.filter((c) => c.startsWith("getSession")).length, 2, "exactly one re-fetch");
  events.emit(live);
  assert.equal(
    store.getState().streaming["m-1"],
    "tail",
    "still pending in the snapshot, so the buffer survives — and the re-delivery does not double it",
  );
  assert.equal(store.getState().thread.length, 2, "no duplicate message after the resync");

  await store.stop();
});

test("a queued message drains once the turn ends, and only into its own session", async () => {
  const { api, calls } = fakeApi({
    getSession: (id: string) =>
      Promise.resolve({
        session: session(id),
        thread: [message("m-1", { pending: true })],
        usage: snapshotAfterOutage().usage,
      }),
  });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect, now: () => 1 });
  store.start();
  events.open(false);
  await store.open(SESSION);
  await settle();

  assert.equal(isBusy(store.getState()), true);
  await store.send("while busy", { queue: true });
  assert.deepEqual(store.getState().queued, ["while busy"]);
  assert.equal(calls.some((c) => c.startsWith("postMessage")), false);

  // The turn ends: the drain posts it into a fresh turn (spec §5).
  events.emit({
    type: "message.finished",
    sessionId: SESSION,
    seq: 99,
    ts: 9_999,
    data: { messageId: "m-1" },
  });
  await settle();
  assert.deepEqual(store.getState().queued, []);
  assert.equal(calls.filter((c) => c === `postMessage:${SESSION}`).length, 1);

  await store.stop();
});

test("a failing request becomes a notice, never a thrown render", async () => {
  const { api } = fakeApi({
    listSessions: () => Promise.reject(new Error("can't reach the bough server")),
  });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();
  assert.match(store.getState().notice ?? "", /can't reach the bough server/);
  await store.stop();
});

test("interrupt raises the stop on the OPEN session, and says what happened", async () => {
  // Spec §5's user interrupt, from the client end. Two properties, both of which the
  // route's shape is designed around: it is always addressed at the session the user
  // is looking at, and its answer is REPORTED either way — a stop that finds nothing
  // running must say so rather than being a silent no-op, which is indistinguishable
  // from a key that is not bound.
  const stops: string[] = [];
  const { api } = fakeApi({
    interrupt: (id: string) => {
      stops.push(id);
      return Promise.resolve({
        sessionId: id,
        interrupted: true,
        message: "interrupting — the program's children are killed",
      });
    },
  });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();

  // With no session open there is nothing to address the stop at, and inventing one
  // would interrupt somebody else's turn.
  await store.interrupt();
  assert.deepEqual(stops, []);

  await store.open(SESSION);
  await settle();
  await store.interrupt();
  assert.deepEqual(stops, [SESSION]);
  assert.match(store.getState().notice ?? "", /interrupting/);

  await store.stop();
});

test("a failed interrupt is a notice, not a throw into the render", async () => {
  const { api } = fakeApi({
    interrupt: () => Promise.reject(new Error("the server went away")),
  });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();
  await store.open(SESSION);
  await settle();
  await store.interrupt();
  assert.match(store.getState().notice ?? "", /the server went away/);
  await store.stop();
});

// ---- attribution, and the audit trail ---------------------------------------

const USAGE = (over: Partial<SessionSnapshot["usage"]> = {}): SessionSnapshot["usage"] => ({
  inputTokens: 0,
  outputTokens: 0,
  reasoningTokens: 0,
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
  costUsd: 0,
  tree: {
    inputTokens: 0,
    outputTokens: 0,
    reasoningTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    costUsd: 0,
  },
  ...over,
});

test("a turn's tokens are its OWN, measured from where the session already stood", () => {
  // The number a spinner raises the question about is what THIS turn is costing, not
  // what the conversation has cost since it began. So the meter is a delta from the
  // session total at the moment the turn started, and a session that had already
  // spent 1.2k tokens does not begin its next turn reading 1.2k.
  const rec = new Recorder();
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = apply(state, {
    type: "snapshot",
    at: 0,
    snapshot: {
      session: session(SESSION),
      thread: [],
      usage: USAGE({ inputTokens: 1_000, outputTokens: 200, costUsd: 0.02 }),
    },
  });
  state = replay(state, [rec.emit("message.started", message("m-1", { pending: true }), SESSION)]);
  assert.equal(state.turn?.baseTokens, 1_200);
  assert.equal(state.turn?.tokens, 0);

  state = apply(state, {
    type: "usage",
    sessionId: SESSION,
    usage: USAGE({ inputTokens: 1_500, outputTokens: 700, costUsd: 0.05 }),
  });
  assert.equal(state.turn?.tokens, 1_000);
  assert.equal(Math.round((state.turn?.costUsd ?? 0) * 1000), 30);
  // The session meter still reports the session.
  assert.equal(state.usage?.inputTokens, 1_500);
});

test("a finished turn leaves a settled line in the transcript, and the spinner's numbers do not vanish", () => {
  const rec = new Recorder();
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, [rec.emit("message.started", message("m-1", { pending: true }), SESSION)]);
  const startedAt = state.turn!.startedAt;
  state = apply(state, {
    type: "usage",
    sessionId: SESSION,
    usage: USAGE({ inputTokens: 3_000, outputTokens: 200, costUsd: 0.021 }),
  });
  state = replay(state, [
    rec.emit("turn.finished", { sessionId: SESSION, turnId: "t1", status: "done" }, SESSION),
  ]);
  // Ended, but not settled: the numbers are only final after the refetch.
  assert.equal(state.turn?.endedAt !== null, true);
  assert.equal(state.marks.length, 0);

  state = apply(state, { type: "turn.settle", at: startedAt + 14_000 });
  assert.equal(state.turn, null);
  const mark = state.marks.at(-1)!;
  assert.equal(mark.kind, "turn");
  assert.match(mark.text, /^✓ /);
  assert.match(mark.text, /3\.2k tok/);
  // NO per-turn cost: asked for and removed. The session total lives on the status
  // row, which is the only place a dollar figure is actually read.
  assert.equal(/\$/.test(mark.text), false, mark.text);
  // A settle with nothing to settle is a no-op, not a "✓" under a live spinner.
  assert.equal(reduce(state, { type: "turn.settle", at: 0 }), state);
});

test("an interrupted turn says so, and does not wear a ✓", () => {
  const rec = new Recorder();
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = replay(state, [
    rec.emit("message.started", message("m-1", { pending: true }), SESSION),
    rec.emit("turn.finished", { sessionId: SESSION, turnId: "t1", status: "interrupted" }, SESSION),
  ]);
  state = apply(state, { type: "turn.settle", at: 0 });
  assert.match(state.marks.at(-1)!.text, /^⏹ /);
  assert.match(state.marks.at(-1)!.text, /interrupted/);
});

test("a destructive outcome outlives the notice that announced it", async () => {
  // THE SEAM. `notify` puts a line on a row that expires in ten seconds; a revert
  // deletes files. Routing the outcome through `record` writes both, so the fact is
  // still there when the toast is gone — and it is filed against the session it
  // happened in, so switching away and back does not lose it.
  const { api } = fakeApi();
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();
  await store.open(SESSION);
  await settle();

  store.record("reverted README.md");
  assert.equal(store.getState().notice, "reverted README.md");
  assert.deepEqual(
    marksFor(store.getState(), SESSION).map((m) => [m.kind, m.text]),
    [["destructive", "reverted README.md"]],
  );

  // The notice expires. The mark does not.
  store.dismissNotice();
  assert.equal(store.getState().notice, null);
  assert.equal(marksFor(store.getState(), SESSION).length, 1);

  // …and neither does looking somewhere else and coming back.
  await store.open(OTHER);
  await settle();
  assert.deepEqual(marksFor(store.getState(), OTHER), []);
  await store.open(SESSION);
  await settle();
  assert.equal(marksFor(store.getState(), SESSION).length, 1);
  await store.stop();
});

test("stopping a unit uses the right route and records what it killed", async () => {
  const killed: string[] = [];
  const { api } = fakeApi({
    killJob: (id: string, jobId: string) => {
      killed.push(`kill:${id}/${jobId}`);
      return Promise.resolve({ message: "killed" });
    },
    interrupt: (id: string) => {
      killed.push(`interrupt:${id}`);
      return Promise.resolve({ sessionId: id, interrupted: true, message: "stopping" });
    },
    stopWorkflow: (id: string) => {
      killed.push(`stopWorkflow:${id}`);
      return Promise.resolve({} as never);
    },
  });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();
  await store.open(SESSION);
  await settle();

  const unit = (over: Partial<LiveUnit>): LiveUnit => ({
    kind: "shell",
    id: "bg_7",
    sessionId: SESSION,
    title: "bg_7",
    elapsedMs: 0,
    tokens: null,
    costUsd: null,
    progress: null,
    detail: "sleep 90",
    ...over,
  });

  await store.stopUnit(unit({}));
  await store.stopUnit(unit({ kind: "subagent", id: "sub-1", sessionId: "sub-1", title: "review" }));
  await store.stopUnit(unit({ kind: "workflow", id: "run-1", sessionId: "run-1", title: "bench" }));
  assert.deepEqual(killed, [
    `kill:${SESSION}/bg_7`,
    "interrupt:sub-1",
    "stopWorkflow:run-1",
  ]);
  // Every one of them is in the ledger, with its scope named.
  assert.deepEqual(
    marksFor(store.getState(), SESSION).map((m) => m.text),
    ["killed bg_7 — sleep 90", "stopped subagent review", "stopped workflow bench"],
  );
  await store.stop();
});

test("a failed kill is a notice and leaves NO mark — nothing was destroyed", async () => {
  const { api } = fakeApi({ killJob: () => Promise.reject(new Error("no such job")) });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();
  await store.open(SESSION);
  await settle();
  await store.stopUnit({
    kind: "shell",
    id: "bg_9",
    sessionId: SESSION,
    title: "bg_9",
    elapsedMs: 0,
    tokens: null,
    costUsd: null,
    progress: null,
    detail: "sleep 1",
  });
  assert.match(store.getState().notice ?? "", /no such job/);
  assert.deepEqual(marksFor(store.getState(), SESSION), []);
  await store.stop();
});

test("liveUnits attributes every running thing separately", () => {
  const now = 100_000;
  const units = liveUnits({
    now,
    jobs: [
      {
        id: "bg_7",
        name: "the long sleep",
        sessionId: SESSION,
        pid: 1,
        command: "sleep 90",
        status: "running",
        startedAt: now - 30_000,
      },
      {
        id: "bg_6",
        name: "finished one",
        sessionId: SESSION,
        pid: 2,
        command: "done",
        status: "exited",
        startedAt: now - 60_000,
      },
    ],
    subagents: [
      row("sub-1", { title: "review app.ts", busy: true, createdAt: now - 45_000, tokens: 3_200 }),
      row("sub-2", { title: "finished", busy: false, createdAt: now - 90_000 }),
    ],
    workflows: [
      {
        id: "run-1",
        name: "bench",
        description: "",
        status: "running",
        currentPhase: "measure",
        phases: [],
        agents: { total: 8, done: 2, cached: 1, running: 2, queued: 3, failed: 0 },
        result: null,
        error: null,
        resumeOf: null,
        createdAt: now - 120_000,
        finishedAt: null,
        scriptFile: "x.js",
      },
    ],
  });
  // Exited shells, finished agents and terminal runs are not "running": the rail
  // pins live work only, which is what keeps it from growing past the terminal.
  assert.deepEqual(units.map((u) => `${u.kind}:${u.id}`), [
    "shell:bg_7",
    "subagent:sub-1",
    "workflow:run-1",
  ]);
  assert.equal(units[0].elapsedMs, 30_000);
  assert.equal(units[0].tokens, null);
  assert.equal(units[0].detail, "sleep 90");
  assert.equal(units[1].tokens, 3_200);
  // A run is the one unit that knows how far along it is — replays count as done.
  assert.equal(units[2].progress, 3 / 8);
  // …and everything else must report NO progress rather than an invented bar.
  assert.equal(units[0].progress, null);
  assert.equal(units[1].progress, null);
});

test("openJob reads a job's buffer, and a failed refresh keeps the last one", async () => {
  const JOB = {
    id: "bg_1",
    name: "dev server",
    sessionId: SESSION,
    pid: 7,
    command: "npm run dev",
    status: "running" as const,
    startedAt: 0,
  };
  let fail = false;
  const { api } = fakeApi({
    jobOutput: () =>
      fail
        ? Promise.reject(new Error("the server went away"))
        : Promise.resolve({ output: "listening on 5173", job: JOB }),
  });
  const events = fakeStream();
  const store = createStore({ api, connect: events.connect });
  store.start();
  await settle();
  await store.open(SESSION);
  await settle();

  await store.openJob("bg_1", SESSION);
  assert.equal(store.getState().jobView?.output, "listening on 5173");
  assert.equal(store.getState().jobView?.job?.name, "dev server");
  assert.equal(store.getState().jobView?.error, null);

  // A failed poll must not blank the screen: losing everything printed so far
  // because one round trip missed is worse than a stale tail with a reason on it.
  fail = true;
  await store.refreshJob();
  assert.equal(store.getState().jobView?.output, "listening on 5173");
  assert.match(store.getState().jobView?.error ?? "", /went away/);

  store.closeJob();
  assert.equal(store.getState().jobView, null);
  // …and with nothing open, a refresh is a no-op rather than a throw.
  await store.refreshJob();
  assert.equal(store.getState().jobView, null);
  await store.stop();
});

test("a multi-line command still makes ONE rail row", async () => {
  // The defect: `App` sizes the transcript by subtracting `units.length`, so a row
  // whose text contained a newline painted two rows and shoved the composer and the
  // status line off theirs — the frame came apart around a `for` loop.
  const units = liveUnits({
    now: 1_000,
    jobs: [{
      id: "bg_1",
      name: "webhook POST every 10s",
      sessionId: SESSION,
      pid: 1,
      command: 'for i in 1 2 3; do\n  echo "request $i"\n  sleep 10\ndone',
      status: "running",
      startedAt: 0,
    }],
    subagents: [],
    workflows: [],
  });
  assert.equal(units.length, 1);
  assert.equal(units[0].detail?.includes("\n"), false, units[0].detail ?? "");
  // The join is MARKED rather than silently closed up: two lines are not one line
  // with a space in it, and a reader comparing this to their scrollback should see it.
  assert.match(units[0].detail ?? "", /for i in 1 2 3; do ¶ echo "request \$i"/);
  assert.equal(unitLine(units[0], 80).includes("\n"), false);
});

/**
 * A hold raised in ANOTHER conversation used to take over this one's composer, and
 * answering it there settled it. Found in a persona audit: an approval card for a
 * workflow the tester never created appeared in their conversation, and pressing
 * Escape to get their composer back DECLINED a different conversation's run.
 *
 * `GET /questions` is unscoped (it is a reconnect path) and the event stream is
 * global, so the filtering has to happen here.
 */
test("the ask card shows only this conversation's holds — and its delegates'", () => {
  const hold = (id: string, sessionId: string) => ({
    id,
    sessionId,
    messageId: `m-${id}`,
    question: `q ${id}`,
    status: "pending" as const,
    ts: 1,
  });
  let state = apply(initialState(), { type: "open", sessionId: SESSION });
  state = reduce(state, {
    type: "sessions",
    sessions: [
      { id: SESSION, title: "mine", kind: "root", createdAt: 1, parentId: null, busy: false },
      { id: "other", title: "theirs", kind: "root", createdAt: 2, parentId: null, busy: false },
      {
        id: "branch",
        title: "fork of mine",
        kind: "fork",
        createdAt: 3,
        parentId: null,
        originId: SESSION,
        busy: false,
      },
    ],
  } as StoreAction);

  // Another root's hold: never mine, whatever order it arrived in.
  state = replay(state, [
    { type: "ask.question", sessionId: "other", seq: 1, ts: 1, data: hold("foreign", "other") },
  ]);
  assert.equal(currentAsk(state), null);

  // My own hold surfaces even though the foreign one is older.
  state = replay(state, [
    { type: "ask.question", sessionId: SESSION, seq: 2, ts: 2, data: hold("mine", SESSION) },
  ]);
  assert.equal(currentAsk(state)?.id, "mine");

  // A branch of mine is mine, resolved through `originId`.
  const branchOnly = { ...state, asks: [hold("fromBranch", "branch")] };
  assert.equal(currentAsk(branchOnly)?.id, "fromBranch");

  // A SUBAGENT's hold must stay answerable: `GET /sessions` hides delegates from the
  // top level, so it is only known because the caller passes the delegate list. Without
  // it the hold would be filtered out and the delegate would park until its turn died.
  const delegateOnly = { ...state, asks: [hold("fromAgent", "agent-1")] };
  assert.equal(currentAsk(delegateOnly), null);
  assert.equal(currentAsk(delegateOnly, [{ id: "agent-1" }])?.id, "fromAgent");

  // No conversation open: nothing may claim the composer.
  assert.equal(currentAsk({ ...state, currentId: null }), null);
});
