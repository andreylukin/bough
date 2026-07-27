/**
 * The SSE envelope and the closed set of event names.
 *
 * The invariant: **events are display transport, never the source of truth.**
 * `seq` is a process-monotonic counter that resets on server restart, so it is a
 * dedupe key and NOT a resume cursor (plan §6.16). A reconnecting client re-fetches
 * `GET /sessions/:id` and reconciles by message id; nothing replays from a seq.
 * The practical rule that falls out: any state a client cannot rebuild from a
 * fresh fetch is a bug in the event design, not something to fix with replay.
 *
 * The name list is closed here so the TUI store can switch on it exhaustively.
 * Payload shapes are declared as types rather than validated on the wire: the bus
 * hands listeners the object it published, in-process, so a Zod parse on the SSE
 * boundary would only re-check what the emitter already typed. The envelope IS
 * parsed, because the client reads it off a socket.
 */
import { z } from "zod";
import type {
  AskQuestion,
  BackgroundJob,
  Message,
  Part,
  Session,
  Turn,
  TurnStatus,
  WorkflowAgent,
  WorkflowRun,
} from "./parts.ts";

// ---- the closed event-name set ---------------------------------------------

/**
 * Spec §3's event list, plus `tool.log`.
 *
 * `tool.log` is not named in spec §3 but is required by spec §5.3 — "console.*
 * lines stream live to the UI **and** batch into the tool result". Streaming
 * console output has no other carrier: `message.delta` is model text, and the
 * batched copy only reaches the client when the tool result lands. It is declared
 * here rather than left for M3 to add, because this list is frozen.
 */
export const EVENT_TYPES = [
  "session.created",
  "session.updated",
  "session.activity",
  "message.started",
  "message.delta",
  "message.part",
  "message.finished",
  "message.retry",
  "tool.log",
  "turn.finished",
  "ask.question",
  "job.spawned",
  "job.exited",
  "workflow.updated",
  "workflow.agent",
  "workflow.log",
] as const;

export const EventType = z.enum(EVENT_TYPES);
export type EventType = z.infer<typeof EventType>;

// ---- the envelope ----------------------------------------------------------

/**
 * Every event carries a process-monotonic `seq` and a `ts`, both stamped by the
 * bus at publish time. `sessionId` is what `GET /events?sessionId=` filters on;
 * events with no session (none today) reach every subscriber.
 *
 * `data` is `unknown` on the envelope on purpose — its shape is per-`type`, and
 * the typed view is `BoughEventOf<T>` below. Validating `data` generically here
 * would mean one schema that is wrong for fifteen payloads.
 */
export const BoughEvent = z.object({
  type: EventType,
  sessionId: z.string().optional(),
  seq: z.number(),
  ts: z.number(),
  data: z.unknown(),
});
export type BoughEvent = z.infer<typeof BoughEvent>;

// ---- per-event payloads ----------------------------------------------------

/** `message.delta` — incremental model text for a streaming message. */
export interface MessageDeltaData {
  messageId: string;
  delta: string;
}

/** `message.part` — one finalized Part appended to a message. */
export interface MessagePartData {
  messageId: string;
  part: Part;
}

/** `message.finished` — the message is complete; `pending` is now false. */
export interface MessageFinishedData {
  messageId: string;
}

/**
 * `message.retry` — the round is being re-attempted (a tool call whose input was
 * cut off mid-stream, or a transient provider failure). The message re-streams
 * from the top, so a client drops its streaming buffer for it. `reason` says which
 * so the UI can show what happened rather than a bare spinner (spec §5 Retry).
 */
export interface MessageRetryData {
  messageId: string;
  /** 1-based; retries are bounded and an exhausted one surfaces as a turn error. */
  attempt: number;
  reason: string;
}

/**
 * `tool.log` — one `console.*` line from a running program, keyed to the
 * `tool_call` that produced it. Display-only: the same line is also batched into
 * the tool result the model receives, so dropping these changes nothing about
 * what the model sees.
 */
export interface ToolLogData {
  messageId: string;
  callId: string;
  line: string;
}

/**
 * `session.activity` — a cheap-tier blurb describing what the session is doing
 * right now. Fails silently and never blocks a turn; `activity: null` clears it
 * (spec §12, plan §6.11).
 */
export interface SessionActivityData {
  sessionId: string;
  activity: string | null;
}

/** `turn.finished` — emitted after `message.finished`, once per turn. */
export interface TurnFinishedData {
  turnId: string;
  sessionId: string;
  status: TurnStatus;
  /** Present when status is `error` — names the limit or the failure (spec §5). */
  error?: string;
}

/** `job.spawned` / `job.exited` — background shells, per spec §9. */
export type JobSpawnedData = BackgroundJob;
export type JobExitedData = BackgroundJob;

/** `workflow.log` — one narrator `log()` line from a running script. */
export interface WorkflowLogData {
  runId: string;
  line: string;
}

/**
 * The payload each event name carries. This is the map the TUI store reduces
 * over; a new arm here is a compile error in every exhaustive switch.
 */
export interface EventDataMap {
  "session.created": Session;
  "session.updated": Session;
  "session.activity": SessionActivityData;
  "message.started": Message;
  "message.delta": MessageDeltaData;
  "message.part": MessagePartData;
  "message.finished": MessageFinishedData;
  "message.retry": MessageRetryData;
  "tool.log": ToolLogData;
  "turn.finished": TurnFinishedData;
  "ask.question": AskQuestion;
  "job.spawned": JobSpawnedData;
  "job.exited": JobExitedData;
  "workflow.updated": WorkflowRun;
  "workflow.agent": WorkflowAgent;
  "workflow.log": WorkflowLogData;
}

/** A stamped event narrowed to one name — `BoughEventOf<"message.delta">`. */
export type BoughEventOf<T extends EventType> = Omit<BoughEvent, "type" | "data"> & {
  type: T;
  data: EventDataMap[T];
};

/** The discriminated union of every stamped event. */
export type AnyBoughEvent = { [K in EventType]: BoughEventOf<K> }[EventType];

/**
 * An event as published — everything except the bus-assigned stamp. The bus
 * (T0.3) assigns `seq`/`ts` and returns the stamped event.
 */
export type EventInput = { [K in EventType]: Omit<BoughEventOf<K>, "seq" | "ts"> }[EventType];

/** Re-exported so consumers of the turn's outcome need one import. */
export type { Turn };
