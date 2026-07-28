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
import type {
  AskQuestion,
  BackgroundJob,
  Message,
  Part,
  Session,
  TurnStatus,
} from "../schema/parts.ts";
import type { AnyBoughEvent, BoughEvent, BoughEventOf, EventType } from "../schema/events.ts";
import type { SessionChangeSet } from "../server/changes.ts";
import type { Effort } from "../types.ts";
import type { Api, ReplayReport, SessionRow, SessionSnapshot, WorkflowSummary } from "./api.ts";
import { api as defaultApi } from "./api.ts";
import { connectEvents, type EventStream } from "./events.ts";
import { fmtDuration, fmtTokens, fmtUsd, humanizeRetryReason, oneLine } from "./format.ts";

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

/**
 * A fact about this conversation that the SERVER does not store and the transcript
 * must not lose.
 *
 * WHY THIS EXISTS. Reverting a file printed `reverted README.md` on the notice row,
 * and `NOTICE_TTL_MS` expired that row ten seconds later. Both halves were
 * reasonable on their own — an outcome belongs on the notice row, and a notice that
 * never expires becomes a stale claim — and together they meant a destructive act
 * (files deleted, edits thrown away) left NO record anywhere ten seconds after it
 * happened. Raising the TTL does not fix that; an audit trail is not a slow toast.
 *
 * So a destructive outcome is written HERE as well, and marks do not expire. They
 * are keyed by session and interleaved into the transcript by `at`, so scrolling
 * back through a conversation shows what was reverted and what was killed at the
 * point in the conversation where it happened — which is the only place the fact is
 * legible.
 *
 * Memory-only, deliberately: nothing here is a server record and none of it is
 * shown to the model. It lives as long as the process, which is as long as the
 * question "what did I just do to my checkout" is live.
 */
export interface TranscriptMark {
  /** Unique and stable, so a renderer can key rows by it. */
  id: string;
  sessionId: string;
  /** When it happened. What the transcript orders marks by. */
  at: number;
  /**
   * `destructive` — a revert, a kill: something that cannot be undone, recorded
   * because spec §7 says destructive actions are recorded.
   * `turn` — how a turn settled: elapsed, tokens, spend. Not destructive, but the
   * same problem in the other direction: the spinner's numbers vanished the instant
   * it stopped and the transcript kept no record of what a turn cost.
   */
  kind: "destructive" | "turn";
  /** The whole line, already worded. Rendered as one row. */
  text: string;
}

/** How many marks are kept. A ledger of a session, not of a lifetime. */
export const MARK_LIMIT = 500;

/**
 * The turn in flight in the OPEN session, and what it has cost so far.
 *
 * The running line said `⚀ waiting for a command to complete · 17s · esc interrupts`
 * — motion and elapsed and nothing else — while `busyLine` had accepted `tokens` and
 * `costUsd` since the day it was written and nobody passed them. The numbers were
 * not missing, they were unfetched: `usage` arrived only with a session snapshot,
 * and a snapshot arrived only when the turn ended. `turn/runner.ts` writes usage per
 * round, so polling `GET /sessions/:id/usage` while a turn runs makes them live.
 *
 * `base*` is the session total at the moment the turn started; the turn's own
 * numbers are the delta. A session total on the running line would be the wrong
 * number — it answers "what has this conversation cost", and the question a spinner
 * raises is "what is THIS costing".
 */
export interface TurnMeter {
  sessionId: string;
  startedAt: number;
  baseTokens: number;
  baseCostUsd: number;
  /** This turn's own tokens and spend, refreshed while it runs. */
  tokens: number;
  costUsd: number;
  /** Set by `turn.finished`; the settle that follows it reads the final usage. */
  endedAt: number | null;
  status: TurnStatus | null;
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
  /**
   * The job the user has OPENED, with its whole retained buffer — null when none is.
   *
   * The rail said `⚙ dev server  4m12s · npm run dev` and that was the end of it:
   * the only way to see what a background job had printed was to ask the model to
   * call `bashOutput` and read the answer through a round of the LLM. This is the
   * user's own door to the same buffer, read through `GET
   * /sessions/:id/jobs/:jobId/output`, which does NOT move the model's cursor —
   * watching a job must never eat output the next round was going to be given.
   */
  jobView: JobViewState | null;
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
  /**
   * The permanent record of what was destroyed and what each turn cost, every
   * session, oldest first. Never cleared by a session switch — see `TranscriptMark`.
   */
  marks: TranscriptMark[];
  /** The open session's turn accounting, live. Null between turns. */
  turn: TurnMeter | null;
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
    jobView: null,
    workflows: [],
    workflowLogs: {},
    workflowSeq: 0,
    replay: null,
    background: null,
    marks: [],
    turn: null,
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
  /** Open, refresh, or (with `view: null`) close the job output view. */
  | { type: "jobView"; view: JobViewState | null }
  | { type: "workflows"; sessionId: string; workflows: WorkflowSummary[] }
  | { type: "replay"; replay: ReplayReport | null }
  | { type: "notice"; notice: string | null }
  /**
   * A destructive outcome, recorded permanently. Raised by `record()`, which ALSO
   * sets the notice — the two are one call precisely so that a future caller cannot
   * do the reasonable half and leave the other undone (see `TranscriptMark`).
   */
  | { type: "mark"; sessionId: string; at: number; text: string }
  /**
   * Live usage for a session, polled while its turn runs. Updates the meter and,
   * when the turn is this session's, the turn's own delta.
   */
  | { type: "usage"; sessionId: string; usage: SessionSnapshot["usage"] }
  /**
   * The turn is over AND its final usage has landed: compute the delta and write
   * the settled mark. Separate from `turn.finished` because the numbers are only
   * final after the refetch that event triggers.
   */
  | { type: "turn.settle"; at: number }
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
    case "workflow":
      return `workflow:${part.id}`;
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

/**
 * The tokens a session is charged for: what it sent, what it got back, what it
 * thought. Cache reads and writes are accounted separately by the provider and are
 * already inside `inputTokens` for billing purposes, so adding them here would
 * count the same tokens twice on the one line where a doubled number is a lie.
 */
export function totalTokens(usage: {
  inputTokens: number;
  outputTokens: number;
  reasoningTokens: number;
}): number {
  return usage.inputTokens + usage.outputTokens + usage.reasoningTokens;
}

/**
 * Fold fresh usage totals in, re-deriving the running turn's own delta.
 *
 * Every path that learns a new total goes through here — the poll AND the snapshot —
 * because the settled line reads the delta the LAST of them left behind, and a
 * snapshot that updated `usage` without the meter reported a turn as free.
 */
function withUsage(state: TuiState, usage: SessionSnapshot["usage"]): TuiState {
  const turn = state.turn && state.turn.sessionId === state.currentId
    ? {
      ...state.turn,
      tokens: Math.max(0, totalTokens(usage) - state.turn.baseTokens),
      costUsd: Math.max(0, usage.costUsd - state.turn.baseCostUsd),
    }
    : state.turn;
  return { ...state, usage, turn };
}

/** Append a mark, oldest first, capped. Marks are a ledger, not a log. */
function appendMark(marks: TranscriptMark[], mark: TranscriptMark): TranscriptMark[] {
  const next = [...marks, mark];
  return next.length > MARK_LIMIT ? next.slice(next.length - MARK_LIMIT) : next;
}

/**
 * How a settled turn reads: `✓ 14s · 3.2k tok · $0.021`.
 *
 * The glyph carries the outcome because a turn that was interrupted and one that
 * finished cost the same and mean opposite things. Zero tokens are omitted rather
 * than printed as `0 tok` — a provider that reports usage only at the end of a
 * stream leaves a real zero here, and an honest silence beats a wrong number.
 */
export function settledLine(turn: TurnMeter, endedAt: number): string {
  const glyph = turn.status === "error"
    ? "✗"
    : turn.status === "interrupted"
    ? "⏹"
    : turn.status === "orphaned"
    ? "⚠"
    : "✓";
  // Elapsed and tokens, NOT cost. A per-turn dollar figure is noise at the density
  // a transcript is read at — what a turn cost is only ever interesting as part of
  // what the SESSION has cost, and that number is on the status row, live. Asked
  // for directly.
  const bits = [fmtDuration(Math.max(0, endedAt - turn.startedAt))];
  if (turn.tokens > 0) bits.push(`${fmtTokens(turn.tokens)} tok`);
  if (turn.status === "interrupted") bits.push("interrupted");
  if (turn.status === "error") bits.push("failed");
  return `${glyph} ${bits.join(" · ")}`;
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
      // A pending message in the open session IS the turn starting. There is no
      // `turn.started` event, and the clock the meter needs is the event's own `ts`
      // rather than a wall clock the reducer is not allowed to read.
      const startTurn = m.pending && (state.turn === null || state.turn.endedAt !== null);
      const turn: TurnMeter | null = startTurn
        ? {
          sessionId: m.sessionId,
          startedAt: event.ts,
          baseTokens: state.usage ? totalTokens(state.usage) : 0,
          baseCostUsd: state.usage?.costUsd ?? 0,
          tokens: 0,
          costUsd: 0,
          endedAt: null,
          status: null,
        }
        : state.turn;
      return { ...state, sessions, thread, turn };
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
      // Stamped, not settled: the tokens this turn cost are only final after the
      // usage refetch this event triggers, and a settled line that under-reports by
      // the last round is the number a user would act on.
      const turn = state.turn?.sessionId === sessionId && state.turn.endedAt === null
        ? { ...state.turn, endedAt: event.ts, status }
        : state.turn;
      return {
        ...state,
        turn,
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
        // The open job belonged to the session being left, and its buffer is fetched
        // per session — carrying it across would paint another conversation's shell.
        jobView: null,
        workflows: [],
        replay: null,
        // The meter belongs to the turn you were watching. `marks` deliberately do
        // NOT reset: they are keyed by session and a record you lose by looking away
        // is the record this whole mechanism exists to stop losing.
        turn: null,
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
      return withUsage({
        ...state,
        session,
        thread: merged,
        streaming,
        effectiveModel: effectiveModel ?? state.effectiveModel,
        contextLimit: contextLimit ?? state.contextLimit,
        reconciledAt: { ...state.reconciledAt, [session.id]: action.at },
      }, usage);
    }

    case "questions":
      return { ...state, asks: action.questions };

    case "ask.settled":
      return { ...state, asks: state.asks.filter((q) => q.id !== action.id) };

    case "changes":
      return action.sessionId === state.currentId ? { ...state, changes: action.changes } : state;

    case "jobs":
      return action.sessionId === state.currentId ? { ...state, jobs: action.jobs } : state;

    case "jobView":
      return { ...state, jobView: action.view };

    case "workflows":
      return action.sessionId === state.currentId
        ? { ...state, workflows: action.workflows }
        : state;

    case "replay":
      return { ...state, replay: action.replay };

    case "notice":
      return { ...state, notice: action.notice };

    case "mark":
      return {
        ...state,
        marks: appendMark(state.marks, {
          id: `mark:${action.at}:${state.marks.length}`,
          sessionId: action.sessionId,
          at: action.at,
          kind: "destructive",
          text: action.text,
        }),
      };

    case "usage":
      return action.sessionId === state.currentId ? withUsage(state, action.usage) : state;

    case "turn.settle": {
      const turn = state.turn;
      // Only a turn that has ENDED settles. A stray settle mid-turn would print a
      // "✓" under a spinner that is still going.
      if (!turn || turn.endedAt === null) return state;
      return {
        ...state,
        turn: null,
        marks: appendMark(state.marks, {
          id: `mark:${turn.sessionId}:${turn.startedAt}`,
          sessionId: turn.sessionId,
          at: turn.endedAt,
          kind: "turn",
          text: settledLine(turn, turn.endedAt),
        }),
      };
    }

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

/**
 * The marks belonging to one session, oldest first — what the transcript interleaves.
 *
 * Filtered rather than stored per session because a mark is written against the
 * session that was open when it happened, and that session can be switched away from
 * and back to. The list is short by construction (`MARK_LIMIT`).
 */
export function marksFor(state: TuiState, sessionId: string | null): TranscriptMark[] {
  if (!sessionId) return [];
  return state.marks.filter((m) => m.sessionId === sessionId);
}

/**
 * One thing running on this session's behalf, with its own numbers.
 *
 * SPEC §5: nothing runs invisibly, and every unit is attributed SEPARATELY. Three
 * kinds of work were each half-visible in a different way — a background shell only
 * while you were scrolled to the tail of the transcript, a subagent as a rail row
 * that said `⋯ working` and nothing more, a workflow as a row in a tab you had to
 * open. This is the one shape all three reduce to, so one rail can hold them and one
 * key can stop them.
 */
export interface LiveUnit {
  kind: "shell" | "subagent" | "workflow";
  /** The job id, the session id, the run id. Unique across kinds by construction. */
  id: string;
  /**
   * The session a stop is addressed to: a shell's owner, the subagent itself, the
   * run's id. `stopUnit` needs no branch beyond `kind` because of this field.
   */
  sessionId: string;
  /** Short, human: `bg_7`, `review app.ts`, `nightly bench`. */
  title: string;
  elapsedMs: number;
  /** This unit's own tokens. Null for a shell, which spends none. */
  tokens: number | null;
  costUsd: number | null;
  /**
   * Determinate progress, 0..1, when the unit can know it — a workflow knows how
   * many of its agents are done. Null everywhere else, and a null must render as no
   * bar rather than as an empty one: an invented percentage is the failure spec §9
   * is guarding against, not the fix for it.
   */
  progress: number | null;
  /** The command, the phase — whatever makes this unit identifiable at a glance. */
  detail: string | null;
}

/**
 * Everything running right now, as rows.
 *
 * PURE and parameterized rather than reading `TuiState`, because the caller already
 * holds the subagent list (`GET /sessions?originId=` lives in the composition root,
 * beside the tree it also feeds) and a second copy in the store would be a second
 * thing to keep fresh. Ordered oldest-first within kind, and shells before agents
 * before runs, so a row does not move under the cursor while it works.
 */
export function liveUnits(opts: {
  jobs: readonly BackgroundJob[];
  /** The open session's delegated children — `liveSubagents` is applied here. */
  subagents: readonly SessionRow[];
  workflows: readonly WorkflowSummary[];
  now: number;
}): LiveUnit[] {
  const { jobs, subagents, workflows, now } = opts;
  const shells: LiveUnit[] = jobs
    .filter((j) => j.status === "running")
    .sort((a, b) => a.startedAt - b.startedAt)
    .map((j) => ({
      kind: "shell" as const,
      id: j.id,
      sessionId: j.sessionId,
      // The NAME the job was started under (`hostfn/jobs.ts` refuses a blank one),
      // falling back to the id for a row from a server that predates names. `bg_7`
      // beside a clipped command identified a shell only to someone who had read the
      // round that started it.
      title: oneLine(j.name || j.id),
      elapsedMs: Math.max(0, now - j.startedAt),
      tokens: null,
      costUsd: null,
      progress: null,
      // ONE LINE, always. A rail row is one row, and `App` sizes the transcript by
      // subtracting `units.length` — so a multi-line command (a `for` loop, a heredoc)
      // painted extra rows and pushed the composer and the status line off theirs.
      detail: oneLine(j.command),
    }));
  const agents: LiveUnit[] = subagents
    .filter((s) => s.busy)
    .sort((a, b) => a.createdAt - b.createdAt)
    .map((s) => ({
      kind: "subagent" as const,
      id: s.id,
      sessionId: s.id,
      title: oneLine(s.title || "subagent"),
      elapsedMs: Math.max(0, now - s.createdAt),
      tokens: s.tokens ?? null,
      costUsd: s.costUsd ?? null,
      progress: null,
      detail: null,
    }));
  const runs: LiveUnit[] = workflows
    .filter((w) => w.status === "running" || w.status === "paused")
    .sort((a, b) => a.createdAt - b.createdAt)
    .map((w) => ({
      kind: "workflow" as const,
      id: w.id,
      sessionId: w.id,
      title: oneLine(w.name || "workflow"),
      elapsedMs: Math.max(0, now - w.createdAt),
      tokens: null,
      costUsd: null,
      // The one unit that knows how far along it is. Replays count as done: they
      // are answers, and the bar measures progress, not spend.
      progress: w.agents.total > 0
        ? Math.min(1, (w.agents.done + w.agents.cached) / w.agents.total)
        : null,
      detail: w.status === "paused"
        ? `paused · ${w.currentPhase ?? "no phase"}`
        : w.currentPhase ?? null,
    }));
  return [...shells, ...agents, ...runs];
}

// ---------------------------------------------------------------------------
// The shell — the only part of this file that performs I/O
// ---------------------------------------------------------------------------

/**
 * How long a notice holds its pinned row before it expires.
 *
 * A notice is a SNAPSHOT OF A MOMENT — "0 replayed, 2 ran live, 1 still going",
 * "✓ that session finished" — printed on a row that is pinned above the composer
 * and therefore reads as a currently-true fact (spec §4). Seconds later it is
 * false, and it was set-only: `notice` had no expiry at all, so a run that had
 * long since reached 4/4 still had a row underneath it claiming one step was in
 * flight, and a finished-in-the-background toast held its row for a quarter of an
 * hour. Esc still dismisses one early; this is what happens when nobody presses it.
 *
 * One duration for every notice deliberately: two tiers would mean deciding, at
 * each of the four call sites, which kind of news this is — and a wrong answer
 * there is exactly the stale row this fixes.
 */
const NOTICE_TTL_MS = 10_000;

/**
 * How often the running turn re-reads its spend.
 *
 * Slow enough that a long turn costs a handful of loopback GETs, fast enough that
 * the number on the running line is never more than a few seconds stale — and it is
 * only ever a floor anyway, since `turn/runner.ts` writes usage per ROUND and a
 * round can take a minute. Live only while a turn runs: a poll that outlives what it
 * measures is a wakeup per interval forever, which is the mistake the spinner clock
 * already learned not to make.
 */
const USAGE_POLL_MS = 3_000;

/**
 * One background job, opened for reading.
 *
 * `output` is the WHOLE retained buffer rather than a delta: this is a screen the
 * user scrolls, and a view that could only ever show what arrived since it opened
 * would answer "what has the dev server printed" with "nothing yet".
 */
export interface JobViewState {
  id: string;
  /** The session that owns the shell — a subagent's job is not the open session's. */
  sessionId: string;
  /** The row, re-read with the output so the header's status cannot go stale. */
  job: BackgroundJob | null;
  output: string;
  /** Why the buffer is not on screen. Null once one has been read. */
  error: string | null;
}

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
   * Focus nothing, so the next message starts a fresh root conversation.
   *
   * Deliberately does NOT create a session up front: an empty one that never got a
   * message would still be a row in the tree, and the tree is the record of work
   * done. The `open` action already clears everything belonging to the session
   * being left, so this is that same transition with no destination.
   */
  newConversation(): void;
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
  /**
   * Stop one unit of running work — a background shell, a subagent, a workflow run.
   *
   * ONE method for three kinds because the caller is a rail row and the rail does
   * not care: a key that stops what the cursor is on must not need the component to
   * know that a shell is killed, a subagent is interrupted and a run is stopped by
   * three different routes. It also guarantees the record — every stop lands in the
   * transcript through `record`, because a destructive act that only produced a
   * ten-second toast is exactly the trail this store lost once already.
   */
  stopUnit(unit: LiveUnit): Promise<void>;
  /**
   * Pin (or clear) the open session's model and thinking depth (spec §4).
   *
   * `PATCH /sessions/:id` has existed since the schema was frozen, and the picker
   * called nothing: a model chosen in the panel lived in component state and died
   * with the process, under a note claiming the route did not exist. Absent field =
   * leave alone, explicit `null` = clear the pin.
   */
  setModel(patch: { model?: string | null; effort?: Effort | null }): Promise<void>;
  refreshChanges(): Promise<void>;
  /** Re-read spend for the open session. Silent on failure — see `USAGE_POLL_MS`. */
  refreshUsage(): Promise<void>;
  refreshJobs(): Promise<void>;
  /**
   * Open one job for reading and fetch its buffer. Idempotent: calling it again for
   * the job already open is the refresh, so the poller and the keypress are one path.
   */
  openJob(id: string, sessionId: string): Promise<void>;
  /** Re-read the open job's buffer. No-op when none is open. */
  refreshJob(): Promise<void>;
  closeJob(): void;
  refreshWorkflows(): Promise<void>;
  /** Spec §8: replay is always reported. This is what fetches the counts. */
  refreshReplay(runId: string): Promise<void>;
  /** Re-fetch everything the stream would have carried while it was down. */
  resync(): Promise<void>;
  /** A transient aside. Expires — see `NOTICE_TTL_MS`. */
  notify(message: string): void;
  /**
   * A destructive outcome: said now AND written into the transcript for good.
   *
   * THE SEAM. Reverting a file went through `notify`, and notices expire, so ten
   * seconds after deleting a file there was no record anywhere that it had happened.
   * Every destructive path — revert, kill, stop — calls THIS, and it does both
   * halves, so no future call site can do the reasonable half and drop the other.
   */
  record(message: string): void;
  dismissNotice(): void;
}

export function createStore(deps: StoreDeps = {}): Store {
  const api = deps.api ?? defaultApi;
  const connect = deps.connect ?? connectEvents;
  const now = deps.now ?? Date.now;

  let state = initialState();
  const listeners = new Set<(state: TuiState) => void>();
  let stream: EventStream | null = null;
  let usageTimer: ReturnType<typeof setInterval> | null = null;

  /**
   * (Re)start the pinned notice's expiry.
   *
   * Armed from the STATE TRANSITION rather than from `notify`, because a notice is
   * set on four unrelated paths — `notify`, `fail`, `interrupt`, and the reducer's
   * own `message.retry` arm — and the only thing they have in common is that
   * `state.notice` changed. One place, and a path added later cannot forget.
   */
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  function armNoticeExpiry(notice: string | null): void {
    if (noticeTimer !== null) clearTimeout(noticeTimer);
    noticeTimer = null;
    if (notice === null) return;
    const timer = setTimeout(() => {
      noticeTimer = null;
      dispatch({ type: "notice", notice: null });
    }, NOTICE_TTL_MS);
    // A pending toast must never be the reason a process — or a test run — stays up.
    (timer as { unref?: () => void }).unref?.();
    noticeTimer = timer;
  }

  /**
   * The spend poll runs exactly while a turn does.
   *
   * Armed from the STATE TRANSITION for the same reason the notice's expiry is: a
   * turn starts on a `message.started` the reducer sees and ends on a
   * `turn.finished` it also sees, and a timer started at either call site is a timer
   * some third path forgets to stop.
   */
  function armUsagePoll(live: boolean): void {
    if (live === (usageTimer !== null)) return;
    if (!live) {
      if (usageTimer !== null) clearInterval(usageTimer);
      usageTimer = null;
      return;
    }
    const timer = setInterval(() => void refreshUsage(), USAGE_POLL_MS);
    // A poll must never be the reason a process — or a test run — stays up.
    (timer as { unref?: () => void }).unref?.();
    usageTimer = timer;
  }

  const turnRunning = (s: TuiState) => s.turn !== null && s.turn.endedAt === null;

  function dispatch(action: StoreAction): void {
    const previous = state;
    const next = reduce(state, action);
    if (next === state) return;
    state = next;
    if (next.notice !== previous.notice) armNoticeExpiry(next.notice);
    if (turnRunning(next) !== turnRunning(previous)) armUsagePoll(turnRunning(next));
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

  /**
   * Open (or re-read) one job's buffer.
   *
   * A failure is kept IN the view rather than raised as a notice: the screen it
   * belongs to is on top, and a job whose fetch failed must say so where the output
   * would have been instead of leaving the last good buffer on screen looking live.
   * The previous output survives a failed refresh for the same reason — losing
   * everything printed so far because one poll missed is worse than a stale tail.
   */
  const openJob = async (id: string, sessionId: string) => {
    const previous = state.jobView?.id === id ? state.jobView : null;
    try {
      const { output, job } = await api.jobOutput(sessionId, id);
      dispatch({ type: "jobView", view: { id, sessionId, job, output, error: null } });
    } catch (error) {
      dispatch({
        type: "jobView",
        view: {
          id,
          sessionId,
          job: previous?.job ?? null,
          output: previous?.output ?? "",
          error: error instanceof Error ? error.message : String(error),
        },
      });
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

  /**
   * Spend, without the thread. Deliberately SILENT on failure: it runs on a timer
   * against a route a slightly older server may not have, and a notice per poll
   * would turn a missing number into a wall of banners. A stale meter says less; a
   * banner every three seconds says nothing at all.
   */
  const refreshUsage = async () => {
    const id = state.currentId;
    if (!id) return;
    try {
      const { usage, tree } = await api.sessionUsage(id);
      dispatch({ type: "usage", sessionId: id, usage: { ...usage, tree } });
    } catch {
      return;
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

  /** Said now, and kept. See `Store.record`. */
  function record(message: string): void {
    dispatch({ type: "notice", notice: message });
    const id = state.currentId;
    if (id) dispatch({ type: "mark", sessionId: id, at: now(), text: message });
  }

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
              // Settle AFTER the refetch, never before: the settled line's tokens
              // are the ones the last round wrote, and a turn reported as free is
              // worse than one reported late.
              void snapshot(finished.data.sessionId)
                .catch(() => refreshUsage())
                .finally(() => dispatch({ type: "turn.settle", at: now() }));
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
      armNoticeExpiry(null);
      armUsagePoll(false);
      dispatch({ type: "connection", connected: false });
    },

    reload,
    open,

    newConversation() {
      dispatch({ type: "open", sessionId: null });
    },

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

    async stopUnit(unit) {
      try {
        if (unit.kind === "shell") await api.killJob(unit.sessionId, unit.id);
        else if (unit.kind === "subagent") await api.interrupt(unit.sessionId);
        else await api.stopWorkflow(unit.id);
      } catch (error) {
        fail(error);
        return;
      }
      // The scope, in the past tense and in full — spec §7: a destructive action
      // says what it did, and says it somewhere that outlives a toast.
      record(
        unit.kind === "shell"
          ? `killed ${unit.title} — ${unit.detail ?? "background shell"}`
          : unit.kind === "subagent"
          ? `stopped subagent ${unit.title}`
          : `stopped workflow ${unit.title}`,
      );
      if (unit.kind === "shell") await refreshJobs();
      if (unit.kind === "workflow") await refreshWorkflows();
    },

    async setModel(patch) {
      try {
        // TWO SCOPES, ONE CHOICE. Patching the session alone was the whole of this
        // and it is why picking a model looked broken: `ctx.model` is `BOUGH_MODEL`
        // frozen at server start, so the pin died with the conversation and the next
        // one reverted to the built-in default. The install default is written
        // FIRST, so if the second call fails the durable half has still landed —
        // the reverse order loses exactly the part the user was complaining about.
        await api.putModelSettings(patch);
        // The open conversation, so the choice applies to what is on screen rather
        // than only to the next one. The row comes back on `session.updated`, so
        // nothing is reconciled here.
        const id = state.currentId;
        if (id) await api.patchSession(id, patch);
      } catch (error) {
        fail(error);
      }
    },

    refreshChanges,
    refreshUsage,
    refreshJobs,
    openJob,
    refreshJob: () => {
      const open = state.jobView;
      return open ? openJob(open.id, open.sessionId) : Promise.resolve();
    },
    closeJob: () => dispatch({ type: "jobView", view: null }),
    refreshWorkflows,
    refreshReplay,
    resync,

    notify: (message: string) => dispatch({ type: "notice", notice: message }),
    record,
    dismissNotice: () => dispatch({ type: "notice", notice: null }),
  };
}

/** Re-exported so a component imports its state shape from one place. */
export type { EventType };
