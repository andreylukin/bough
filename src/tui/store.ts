/**
 * The TUI's state: one pure reducer over `BoughEvent`, plus a thin shell that feeds
 * it from `api.ts` and `events.ts`.
 *
 * THE INVARIANT THIS HOLDS: **state lives here, rendering lives in components, and
 * the reducer touches neither a terminal nor a server.** `reduce(state, action)` is a
 * pure function of data — no `fetch`, no React, no clock, no Ink — so every rule
 * below is testable by replaying a recorded event sequence with nothing mounted
 * (plan §7, "TUI: store reducers over recorded event sequences"). The previous tree's
 * `App.tsx` was 3,618 lines because this boundary did not exist; the shell at the
 * bottom of this file is the only part that performs I/O, and it does so by
 * dispatching the same actions a test dispatches by hand.
 *
 * SECOND INVARIANT — **the reducer is idempotent under re-delivery.** `seq` is a
 * dedupe key, not a resume cursor (spec §3, plan §6.16): it is per-process and resets
 * on restart, so a reconnecting client re-fetches `GET /sessions/:id` and reconciles
 * by message id rather than replaying from a cursor. That makes duplicate delivery a
 * NORMAL condition — the snapshot necessarily contains events the client already
 * applied, and a redialed stream can overlap with the connection it replaced — so the
 * reducer defends in three layers, each covering what the others cannot:
 *
 *   1. **The dedupe window.** An event is identified by `seq:ts`. `seq` alone is not
 *      enough (it resets, so a restarted server's event 1 is not the old event 1) and
 *      `ts` alone is not enough (millisecond collisions). The pair is unique within a
 *      process and, in practice, across one. A bounded window, because this is a
 *      dedupe key and not a ledger.
 *   2. **The snapshot watermark.** The server persists, THEN publishes (`bus.ts`), so
 *      every event stamped before a snapshot was requested is already reflected in
 *      that snapshot. Session-scoped events older than the watermark are dropped
 *      wholesale — which is what catches the events that were re-delivered from
 *      outside the dedupe window, and everything the outage swallowed and the fetch
 *      restored. Server and client share a machine (loopback only, spec §3), so the
 *      two clocks are the same clock.
 *   3. **Identity-keyed appends.** A message is merged by `id`; a part that HAS an
 *      identity (`tool_call.id`, `tool_result.callId`, `ask.id`) is appended only
 *      once. Text and reasoning parts have no identity — two identical `text` parts in
 *      one message are legal — so they are covered by layers 1 and 2 rather than by
 *      content comparison, which would silently swallow a real repeat.
 *
 * THIRD — **a snapshot merges, it does not clobber.** Events published while the
 * fetch was in flight are newer than the rows it read, so replacing the thread with
 * the response would lose exactly the deltas the reconnect was supposed to repair.
 * Parts only ever append within a message and `pending` only ever goes true→false, so
 * the merge is well-defined: union by id, the longer part list wins, finished beats
 * pending.
 */
import type { AskQuestion, BackgroundJob, Message, Part, Session } from "../schema/parts.ts";
import type { AnyBoughEvent, BoughEvent, BoughEventOf, EventType } from "../schema/events.ts";
import type { SessionChangeSet } from "../server/changes.ts";
import type { Api, ReplayReport, SessionRow, SessionSnapshot, WorkflowSummary } from "./api.ts";
import { api as defaultApi } from "./api.ts";
import { connectEvents, type EventStream } from "./events.ts";
import { humanizeRetryReason } from "./format.ts";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/**
 * How many event identities the dedupe window keeps. Large enough to cover a
 * reconnect overlap (a redial delivers at most the frames in flight), small enough
 * that it stays a fixed-cost check rather than an unbounded log of everything the
 * session ever saw.
 */
export const DEDUPE_WINDOW = 256;

/**
 * A listed session plus the one fact only the client knows.
 *
 * `unseen` is deliberately NOT a wire field: it means "this session finished a turn
 * while you were looking at another one", which is a property of this terminal and
 * of nothing else. `busy`, `lastTurnStatus` and `costUsd` are the server's derived
 * values (`api.ts`) and are never invented here.
 */
export interface TuiSessionRow extends SessionRow {
  unseen?: boolean;
}

export interface TuiState {
  /** Is the event stream up? False means the view may be stale, not that work stopped. */
  connected: boolean;
  /** Top-level sessions, newest first. `subagent`/`workflow_agent` never appear here. */
  sessions: TuiSessionRow[];
  currentId: string | null;
  session: Session | null;
  /** Ancestors root→parent, then own — as the server assembled it. */
  thread: Message[];
  /** messageId → text accumulated from `message.delta`, until the text part lands. */
  streaming: Record<string, string>;
  /** callId → live `console.*` lines from the running program. */
  toolLogs: Record<string, string[]>;
  /** Every unsettled `ask()` hold, oldest first. The card shows `asks[0]`. */
  asks: AskQuestion[];
  /** Typed while a turn was running, held locally until it ends. */
  queued: string[];
  notice: string | null;
  /** Cheap-tier blurb for the open session, or null. Fails silently by construction. */
  activity: string | null;
  usage: SessionSnapshot["usage"] | null;
  /** The model the next turn will call — see `SessionSnapshot.effectiveModel`. */
  effectiveModel: string | null;
  /** The effective model's context window. Null = the catalog does not know it. */
  contextLimit: number | null;
  /** null until fetched. `available: false` is an ANSWER, not an error (spec §13). */
  changes: SessionChangeSet | null;
  /** The open session's background shells AND its subagents' (spec §9). */
  jobs: BackgroundJob[];
  workflows: WorkflowSummary[];
  /** runId → the last narrator `log()` line. Memory-only, like the run's chip. */
  workflowLogs: Record<string, string>;
  /** Bumped on every `workflow.*` event — a detail view refetches on the change. */
  workflowSeq: number;
  /**
   * The open run's replay accounting. Spec §8: replay is ALWAYS reported, because a
   * relaunch that replayed nothing looks exactly like one that replayed everything.
   */
  replay: ReplayReport | null;
  /** A NON-open session finished a turn. `seq` makes repeat finishes distinct. */
  background: { sessionId: string; title: string; seq: number } | null;
  /** sessionId → when its snapshot was requested. The watermark of layer 2. */
  reconciledAt: Record<string, number>;
  /** The dedupe window of layer 1, oldest first. */
  seen: readonly string[];
}

export function initialState(): TuiState {
  return {
    connected: false,
    sessions: [],
    currentId: null,
    session: null,
    thread: [],
    streaming: {},
    toolLogs: {},
    asks: [],
    queued: [],
    notice: null,
    activity: null,
    usage: null,
    effectiveModel: null,
    contextLimit: null,
    changes: null,
    jobs: [],
    workflows: [],
    workflowLogs: {},
    workflowSeq: 0,
    replay: null,
    background: null,
    reconciledAt: {},
    seen: [],
  };
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

export type StoreAction =
  /** One event off the wire. Everything about dedupe happens under this arm. */
  | { type: "event"; event: BoughEvent }
  | { type: "connection"; connected: boolean }
  | { type: "sessions"; sessions: SessionRow[] }
  /** Focus a session. Clears everything that belonged to the previous one. */
  | { type: "open"; sessionId: string | null }
  /**
   * A fresh `GET /sessions/:id`. `at` is when the FETCH WAS ISSUED, not when it
   * landed: the conservative end of the window, so an event published during the
   * round trip is re-applied (and deduped) rather than dropped as already-known.
   */
  | { type: "snapshot"; at: number; snapshot: SessionSnapshot }
  | { type: "questions"; questions: AskQuestion[] }
  /** Optimistic settle: the next hold surfaces immediately; the event confirms it. */
  | { type: "ask.settled"; id: string }
  | { type: "changes"; sessionId: string; changes: SessionChangeSet }
  | { type: "jobs"; sessionId: string; jobs: BackgroundJob[] }
  | { type: "workflows"; sessionId: string; workflows: WorkflowSummary[] }
  | { type: "replay"; replay: ReplayReport | null }
  | { type: "notice"; notice: string | null }
  | { type: "queue"; text: string }
  | { type: "queue.drained" };

// ---------------------------------------------------------------------------
// Dedupe and reconciliation primitives (pure)
// ---------------------------------------------------------------------------

/** Layer 1's key. The pair, for the reasons in the header. */
export function eventKey(event: { seq: number; ts: number }): string {
  return `${event.seq}:${event.ts}`;
}

/**
 * Has this exact event already been applied, or does the snapshot already contain it?
 *
 * Exported because it is the rule the whole reconnect story rests on, and a rule
 * worth being able to test without building a state tree around it.
 */
export function isDuplicate(state: TuiState, event: BoughEvent): boolean {
  if (state.seen.includes(eventKey(event))) return true;
  if (event.sessionId === undefined) return false;
  const watermark = state.reconciledAt[event.sessionId];
  return watermark !== undefined && event.ts < watermark;
}

function remember(seen: readonly string[], key: string): readonly string[] {
  const next = seen.length >= DEDUPE_WINDOW
    ? seen.slice(seen.length - DEDUPE_WINDOW + 1)
    : [...seen];
  next.push(key);
  return next;
}

/** A part's identity, or null when it has none (text, reasoning). */
export function partKey(part: Part): string | null {
  switch (part.type) {
    case "tool_call":
      return `tool_call:${part.id}`;
    case "tool_result":
      return `tool_result:${part.callId}`;
    case "ask":
      return `ask:${part.id}`;
    case "image":
      return `image:${part.path}`;
    default:
      return null; // text / reasoning — legal to repeat, so never deduped by content
  }
}

/** Append `part` unless an identity-carrying twin is already there (layer 3). */
function appendPart(parts: Part[], part: Part): Part[] {
  const key = partKey(part);
  if (key !== null && parts.some((p) => partKey(p) === key)) return parts;
  return [...parts, part];
}

/**
 * Merge one message from a snapshot with the one the events built.
 *
 * Parts only append and `pending` only clears, which is what makes "take the longer
 * list, take finished over pending" a merge rather than a guess.
 */
function mergeMessage(fromDb: Message, local: Message): Message {
  return {
    ...fromDb,
    parts: local.parts.length > fromDb.parts.length ? local.parts : fromDb.parts,
    pending: fromDb.pending && local.pending,
  };
}

/**
 * The snapshot thread, plus anything the stream delivered that the read predates.
 *
 * A straight replace would drop a `message.started` that landed while the request was
 * in flight — the message would then reappear only on the next fetch, which is the
 * "my message vanished" bug the reconnect path exists to prevent.
 */
export function mergeThread(fromDb: Message[], local: Message[]): Message[] {
  const localById = new Map(local.map((m) => [m.id, m]));
  const merged = fromDb.map((m) => {
    const mine = localById.get(m.id);
    return mine ? mergeMessage(m, mine) : m;
  });
  const known = new Set(fromDb.map((m) => m.id));
  for (const m of local) if (!known.has(m.id)) merged.push(m);
  return merged;
}

// ---------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------

/** Rows carry client-side memory (`unseen`) that a server refetch must not erase. */
function mergeSessionRows(previous: TuiSessionRow[], next: SessionRow[]): TuiSessionRow[] {
  const before = new Map(previous.map((s) => [s.id, s]));
  return next.map((s) => {
    const old = before.get(s.id);
    return old?.unseen ? { ...s, unseen: true } : s;
  });
}

function patchSession(
  sessions: TuiSessionRow[],
  id: string,
  patch: (s: TuiSessionRow) => TuiSessionRow,
): TuiSessionRow[] {
  let changed = false;
  const next = sessions.map((s) => {
    if (s.id !== id) return s;
    const updated = patch(s);
    if (updated !== s) changed = true;
    return updated;
  });
  return changed ? next : sessions;
}

function patchMessage(
  thread: Message[],
  id: string,
  patch: (m: Message) => Message,
): Message[] {
  let changed = false;
  const next = thread.map((m) => {
    if (m.id !== id) return m;
    const updated = patch(m);
    if (updated !== m) changed = true;
    return updated;
  });
  return changed ? next : thread;
}

function withoutKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  if (!(key in record)) return record;
  const next = { ...record };
  delete next[key];
  return next;
}

/**
 * Apply one event. Assumes dedupe already passed — `reduce` owns that, so this
 * function is only ever asked "what does this event mean", never "have I seen it".
 */
function applyEvent(state: TuiState, raw: BoughEvent): TuiState {
  const event = raw as AnyBoughEvent;
  const current = state.currentId;
  const mine = event.sessionId !== undefined && event.sessionId === current;

  switch (event.type) {
    case "session.created": {
      const s = event.data;
      if (state.sessions.some((p) => p.id === s.id)) return state;
      // Delegated work collapses under its origin and is reached by drill-in, never
      // by the top-level list (spec §4). Visibility is derived, here as everywhere.
      if (s.kind === "subagent" || s.kind === "workflow_agent") return state;
      return { ...state, sessions: [{ ...s, busy: false }, ...state.sessions] };
    }

    case "session.updated": {
      const s = event.data;
      return {
        ...state,
        sessions: patchSession(state.sessions, s.id, (p) => ({ ...p, ...s })),
        session: state.session?.id === s.id ? { ...state.session, ...s } : state.session,
      };
    }

    case "session.activity": {
      if (!mine) return state;
      return { ...state, activity: event.data.activity };
    }

    case "message.started": {
      const m = event.data;
      const sessions = m.pending
        ? patchSession(state.sessions, m.sessionId, (s) => s.busy ? s : { ...s, busy: true })
        : state.sessions;
      if (m.sessionId !== current) return { ...state, sessions };
      const existing = state.thread.find((x) => x.id === m.id);
      const thread = existing
        // Already here: keep what the stream has accumulated. Re-appending is the
        // duplicate-message bug this whole file is arranged to make impossible.
        ? patchMessage(state.thread, m.id, (x) => mergeMessage(m, x))
        : [...state.thread, m];
      return { ...state, sessions, thread };
    }

    case "message.delta": {
      const { messageId, delta } = event.data;
      if (!mine && current !== null) return state;
      return {
        ...state,
        streaming: {
          ...state.streaming,
          [messageId]: (state.streaming[messageId] ?? "") + delta,
        },
      };
    }

    case "message.retry": {
      // The round is being re-attempted and re-streams from the top, so the partial
      // text is not a prefix of what is coming — it is a competing copy.
      const { messageId, attempt, reason } = event.data;
      return {
        ...state,
        streaming: withoutKey(state.streaming, messageId),
        notice: mine
          ? `retrying (attempt ${attempt}) — ${humanizeRetryReason(reason)}`
          : state.notice,
      };
    }

    case "message.part": {
      const { messageId, part } = event.data;
      const thread = patchMessage(state.thread, messageId, (m) => {
        const parts = appendPart(m.parts, part);
        return parts === m.parts ? m : { ...m, parts };
      });
      return {
        ...state,
        thread,
        // The finalized text part supersedes the live buffer; keeping both renders
        // the same prose twice.
        streaming: part.type === "text" ? withoutKey(state.streaming, messageId) : state.streaming,
      };
    }

    case "tool.log": {
      const { callId, line } = event.data;
      return {
        ...state,
        toolLogs: { ...state.toolLogs, [callId]: [...(state.toolLogs[callId] ?? []), line] },
      };
    }

    case "message.finished": {
      const { messageId } = event.data;
      const sessionId = event.sessionId;
      let sessions = state.sessions;
      let background = state.background;
      if (sessionId !== undefined) {
        const row = state.sessions.find((s) => s.id === sessionId);
        sessions = patchSession(state.sessions, sessionId, (s) => ({
          ...s,
          busy: false,
          unseen: s.unseen || !mine,
        }));
        // A background session finishing while you watch another is news; a subagent
        // finishing inside its spawner's still-running turn is not.
        if (row?.busy && !mine && row.kind !== "subagent" && row.kind !== "workflow_agent") {
          background = {
            sessionId,
            title: row.title || "session",
            seq: (state.background?.seq ?? 0) + 1,
          };
        }
      }
      return {
        ...state,
        sessions,
        background,
        activity: mine ? null : state.activity,
        thread: patchMessage(
          state.thread,
          messageId,
          (m) => m.pending ? { ...m, pending: false } : m,
        ),
        streaming: withoutKey(state.streaming, messageId),
      };
    }

    case "turn.finished": {
      const { sessionId, status } = event.data;
      return {
        ...state,
        sessions: patchSession(state.sessions, sessionId, (s) => ({
          ...s,
          busy: false,
          lastTurnStatus: status,
        })),
      };
    }

    case "ask.question": {
      const q = event.data;
      if (q.status === "pending") {
        const asks = state.asks.some((p) => p.id === q.id)
          ? state.asks.map((p) => (p.id === q.id ? q : p))
          : [...state.asks, q];
        return { ...state, asks };
      }
      // Settled: drop it so the next hold surfaces. The durable record is the `ask`
      // part on the transcript, which arrives as `message.part`.
      return { ...state, asks: state.asks.filter((p) => p.id !== q.id) };
    }

    case "job.spawned":
    case "job.exited": {
      const job = event.data;
      const known = state.jobs.some((j) => j.id === job.id);
      // Only the open session's own rows are patched in place. A subagent's job
      // belongs to the tree list, which the shell refetches — this reducer does not
      // know the lineage, and inventing it here would put visibility rules in two
      // places (`server/jobs.ts` owns them).
      if (!known && job.sessionId !== current) return state;
      const jobs = known
        ? state.jobs.map((j) => (j.id === job.id ? job : j))
        : [job, ...state.jobs];
      return { ...state, jobs };
    }

    case "workflow.updated": {
      const run = event.data;
      const workflows = state.workflows.some((w) => w.id === run.id)
        ? state.workflows.map((w) =>
          w.id === run.id
            ? {
              ...w,
              status: run.status,
              currentPhase: run.currentPhase,
              error: run.error,
              finishedAt: run.finishedAt,
            }
            : w
        )
        : state.workflows;
      return { ...state, workflows, workflowSeq: state.workflowSeq + 1 };
    }

    case "workflow.agent":
      return { ...state, workflowSeq: state.workflowSeq + 1 };

    case "workflow.log": {
      const { runId, line } = event.data;
      return { ...state, workflowLogs: { ...state.workflowLogs, [runId]: line } };
    }
  }
  // Exhaustive over the frozen `EVENT_TYPES`; an unreachable default would hide the
  // compile error that a new event name is supposed to cause here.
  return state;
}

/** The whole state transition. Pure: same inputs, same output, no I/O anywhere. */
export function reduce(state: TuiState, action: StoreAction): TuiState {
  switch (action.type) {
    case "event": {
      const { event } = action;
      if (isDuplicate(state, event)) return state;
      const next = applyEvent(state, event);
      // Remembered even when the event changed nothing: "seen" is about delivery,
      // not about effect, and an event that was a no-op once is a no-op twice.
      return { ...next, seen: remember(state.seen, eventKey(event)) };
    }

    case "connection":
      return state.connected === action.connected
        ? state
        : { ...state, connected: action.connected };

    case "sessions":
      return { ...state, sessions: mergeSessionRows(state.sessions, action.sessions) };

    case "open": {
      const id = action.sessionId;
      if (id === state.currentId) return state;
      // Everything below belonged to the session being left: a queued message must
      // not be sent to the new one, and a blurb describes the session it was born in.
      return {
        ...state,
        currentId: id,
        session: null,
        thread: [],
        streaming: {},
        toolLogs: {},
        queued: [],
        activity: null,
        usage: null,
        effectiveModel: null,
        contextLimit: null,
        changes: null,
        jobs: [],
        workflows: [],
        replay: null,
        sessions: id === null
          ? state.sessions
          : patchSession(state.sessions, id, (s) => s.unseen ? { ...s, unseen: false } : s),
      };
    }

    case "snapshot": {
      const { session, thread, usage, effectiveModel, contextLimit } = action.snapshot;
      if (session.id !== state.currentId) {
        // A snapshot that lost the race with a session switch. Record the watermark
        // anyway — it is a fact about that session, not about the view.
        return { ...state, reconciledAt: { ...state.reconciledAt, [session.id]: action.at } };
      }
      const merged = mergeThread(thread, state.thread);
      // Drop the live buffer of any message the database now shows as finished: its
      // text is in the parts. A still-pending message keeps its buffer — that text is
      // not persisted anywhere yet, and the outage's hole in it is repaired wholesale
      // when the finalized text part lands.
      const streaming: Record<string, string> = {};
      for (const [messageId, text] of Object.entries(state.streaming)) {
        if (merged.some((m) => m.id === messageId && m.pending)) streaming[messageId] = text;
      }
      return {
        ...state,
        session,
        thread: merged,
        streaming,
        usage,
        effectiveModel: effectiveModel ?? state.effectiveModel,
        contextLimit: contextLimit ?? state.contextLimit,
        reconciledAt: { ...state.reconciledAt, [session.id]: action.at },
      };
    }

    case "questions":
      return { ...state, asks: action.questions };

    case "ask.settled":
      return { ...state, asks: state.asks.filter((q) => q.id !== action.id) };

    case "changes":
      return action.sessionId === state.currentId ? { ...state, changes: action.changes } : state;

    case "jobs":
      return action.sessionId === state.currentId ? { ...state, jobs: action.jobs } : state;

    case "workflows":
      return action.sessionId === state.currentId
        ? { ...state, workflows: action.workflows }
        : state;

    case "replay":
      return { ...state, replay: action.replay };

    case "notice":
      return { ...state, notice: action.notice };

    case "queue":
      return { ...state, queued: [...state.queued, action.text] };

    case "queue.drained":
      return state.queued.length === 0 ? state : { ...state, queued: [] };
  }
}

// ---------------------------------------------------------------------------
// Selectors — derived, never stored
// ---------------------------------------------------------------------------

/** A turn is in flight in the open session. Derived from the thread, like the server. */
export function isBusy(state: TuiState): boolean {
  return state.thread.some((m) => m.pending);
}

/** The hold the card shows. One at a time, oldest first (spec §6). */
export function currentAsk(state: TuiState): AskQuestion | null {
  return state.asks[0] ?? null;
}

/** The message's live text, or the text it finalized into. */
export function liveText(state: TuiState, messageId: string): string {
  return state.streaming[messageId] ?? "";
}

// ---------------------------------------------------------------------------
// The shell — the only part of this file that performs I/O
// ---------------------------------------------------------------------------

export interface StoreDeps {
  /** Absent = the production client. A test passes a fake and never binds a socket. */
  api?: Api;
  /** Absent = the real SSE client. Injected so a test drives events by hand. */
  connect?: typeof connectEvents;
  /** Absent = `Date.now`. The snapshot watermark reads this and nothing else does. */
  now?: () => number;
}

export interface Store {
  getState(): TuiState;
  /** Returns an unsubscribe thunk. Called after every state change, never per event. */
  subscribe(listener: (state: TuiState) => void): () => void;
  dispatch(action: StoreAction): void;
  /** Open the stream and load the session list and any live `ask()` holds. */
  start(): void;
  stop(): Promise<void>;
  reload(): Promise<void>;
  open(sessionId: string): Promise<void>;
  createSession(workspace?: string): Promise<Session | null>;
  /**
   * Post a message. While a turn runs, `queue` holds it locally and it drains into a
   * fresh turn when the current one ends; without `queue` it is posted immediately
   * and the server queues it (spec §5) — steering, rather than staging.
   */
  send(text: string, opts?: { queue?: boolean; sessionId?: string }): Promise<void>;
  drainQueue(): Promise<void>;
  answerAsk(answer: string): Promise<void>;
  declineAsk(): Promise<void>;
  /**
   * Stop the running turn (spec §5). Always resolves: the server answers 200 with
   * `interrupted: false` when the turn had already ended, so pressing the key a beat
   * late is a no-op with a sentence rather than an error banner. The turn actually
   * ending arrives as `turn.finished` on the stream like every other turn fact.
   */
  interrupt(): Promise<void>;
  refreshChanges(): Promise<void>;
  refreshJobs(): Promise<void>;
  refreshWorkflows(): Promise<void>;
  /** Spec §8: replay is always reported. This is what fetches the counts. */
  refreshReplay(runId: string): Promise<void>;
  /** Re-fetch everything the stream would have carried while it was down. */
  resync(): Promise<void>;
  notify(message: string): void;
  dismissNotice(): void;
}

export function createStore(deps: StoreDeps = {}): Store {
  const api = deps.api ?? defaultApi;
  const connect = deps.connect ?? connectEvents;
  const now = deps.now ?? Date.now;

  let state = initialState();
  const listeners = new Set<(state: TuiState) => void>();
  let stream: EventStream | null = null;

  function dispatch(action: StoreAction): void {
    const next = reduce(state, action);
    if (next === state) return;
    state = next;
    for (const listener of listeners) {
      try {
        listener(state);
      } catch {
        // One wedged renderer must not stop the others from being told, for the same
        // reason the bus isolates its listeners (plan §6.6).
      }
    }
  }

  /** Report a failure to the user instead of throwing into a render. */
  function fail(error: unknown): void {
    dispatch({ type: "notice", notice: error instanceof Error ? error.message : String(error) });
  }

  const reload = async () => {
    try {
      dispatch({ type: "sessions", sessions: await api.listSessions() });
    } catch (error) {
      fail(error);
    }
  };

  const refreshAsks = async () => {
    try {
      dispatch({ type: "questions", questions: await api.listQuestions() });
    } catch (error) {
      fail(error);
    }
  };

  const refreshChanges = async () => {
    const id = state.currentId;
    if (!id) return;
    try {
      dispatch({ type: "changes", sessionId: id, changes: await api.getChanges(id) });
    } catch (error) {
      fail(error);
    }
  };

  const refreshJobs = async () => {
    const id = state.currentId;
    if (!id) return;
    try {
      const { jobs } = await api.listJobs(id);
      dispatch({ type: "jobs", sessionId: id, jobs });
    } catch (error) {
      fail(error);
    }
  };

  const refreshWorkflows = async () => {
    const id = state.currentId;
    if (!id) return;
    try {
      const { workflows } = await api.listWorkflows(id);
      dispatch({ type: "workflows", sessionId: id, workflows });
    } catch (error) {
      fail(error);
    }
  };

  const refreshReplay = async (runId: string) => {
    try {
      dispatch({ type: "replay", replay: await api.workflowReplay(runId) });
    } catch (error) {
      fail(error);
    }
  };

  /**
   * Fetch a session and reconcile. The watermark is taken BEFORE the request, so an
   * event published during the round trip is re-applied rather than assumed known.
   */
  const snapshot = async (id: string) => {
    const at = now();
    const result = await api.getSession(id);
    dispatch({ type: "snapshot", at, snapshot: result });
  };

  const open = async (sessionId: string) => {
    dispatch({ type: "open", sessionId });
    try {
      await snapshot(sessionId);
    } catch (error) {
      fail(error);
      return;
    }
    await Promise.all([refreshChanges(), refreshJobs(), refreshWorkflows()]);
  };

  /**
   * The reconnect path (spec §3). It re-fetches; it does not replay from a seq —
   * there is no seq to replay from, and the database is the source of truth.
   */
  const resync = async () => {
    await Promise.all([reload(), refreshAsks()]);
    const id = state.currentId;
    if (!id) return;
    try {
      await snapshot(id);
    } catch (error) {
      // Unreachable server: the next reconnect resyncs again. Not a notice — the
      // disconnected indicator already says it.
      return;
    }
    await Promise.all([refreshChanges(), refreshJobs(), refreshWorkflows()]);
  };

  const drainQueue = async () => {
    const id = state.currentId;
    if (!id || state.queued.length === 0 || isBusy(state)) return;
    const pending = state.queued;
    dispatch({ type: "queue.drained" });
    for (const text of pending) {
      try {
        await api.postMessage(id, { text });
      } catch (error) {
        fail(error);
      }
    }
  };

  const send = async (text: string, opts: { queue?: boolean; sessionId?: string } = {}) => {
    const id = opts.sessionId ?? state.currentId;
    if (!id) return;
    if (opts.queue && isBusy(state)) {
      dispatch({ type: "queue", text });
      return;
    }
    try {
      await api.postMessage(id, { text });
    } catch (error) {
      fail(error);
    }
  };

  const settleAsk = async (run: (q: AskQuestion) => Promise<unknown>) => {
    const q = currentAsk(state);
    if (!q) return;
    dispatch({ type: "ask.settled", id: q.id });
    try {
      await run(q);
    } catch {
      // Already settled or expired server-side. The holds are memory-only, so the
      // server is the only place that knows — re-read rather than guess.
      await refreshAsks();
    }
  };

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    dispatch,

    start() {
      if (stream) return;
      stream = connect({
        url: api.eventsUrl(),
        onEvent: (event) => {
          dispatch({ type: "event", event });
          // The change set has no event of its own (`schema/events.ts`), so the rail
          // refreshes when a turn ends — which is when the files stop moving.
          //
          // Usage has no event either, and used to have no refresh: it arrived ONLY
          // with a snapshot, so a session's cost read null from the moment it was
          // created until you switched away and came back. The server had the
          // number the whole time. A turn ending is exactly when spend changes, so
          // it is when the meter re-reads it.
          if (event.type === "turn.finished") {
            void refreshChanges();
            const finished = event as BoughEventOf<"turn.finished">;
            if (finished.data.sessionId === state.currentId) {
              void snapshot(finished.data.sessionId);
            }
          }
          if (event.type === "job.spawned" || event.type === "job.exited") void refreshJobs();
          if (event.type === "workflow.updated" || event.type === "workflow.agent") {
            void refreshWorkflows();
          }
          if (event.type === "message.finished") void drainQueue();
        },
        onOpen: ({ reconnect }) => {
          dispatch({ type: "connection", connected: true });
          if (reconnect) void resync();
        },
        onClose: () => dispatch({ type: "connection", connected: false }),
      });
      void reload();
      void refreshAsks();
    },

    async stop() {
      const open = stream;
      stream = null;
      open?.close();
      await open?.done;
      dispatch({ type: "connection", connected: false });
    },

    reload,
    open,

    async createSession(workspace?: string) {
      try {
        // No title: the cheap tier names the session from its first message (§12).
        const session = await api.createSession(workspace ? { workspace } : {});
        await open(session.id);
        await reload();
        return session;
      } catch (error) {
        fail(error);
        return null;
      }
    },

    send,
    drainQueue,

    answerAsk: (answer: string) => settleAsk((q) => api.answerQuestion(q.sessionId, q.id, answer)),
    declineAsk: () => settleAsk((q) => api.declineQuestion(q.sessionId, q.id)),

    async interrupt() {
      const id = state.currentId;
      if (!id) return;
      try {
        const result = await api.interrupt(id);
        // Said either way. "nothing was running" is the answer when the turn beat the
        // keypress, and a silent no-op there reads as a stop button that does nothing.
        dispatch({ type: "notice", notice: result.message });
      } catch (error) {
        fail(error);
      }
    },

    refreshChanges,
    refreshJobs,
    refreshWorkflows,
    refreshReplay,
    resync,

    notify: (message: string) => dispatch({ type: "notice", notice: message }),
    dismissNotice: () => dispatch({ type: "notice", notice: null }),
  };
}

/** Re-exported so a component imports its state shape from one place. */
export type { EventType };
