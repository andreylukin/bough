/**
 * The wire contract — Zod schemas for everything that crosses the server↔UI (and
 * server↔db) boundary. This is the source of truth; web/src/types.ts is a hand-kept
 * mirror of these shapes, so any change here must round-trip against it exactly (see
 * parts.test.ts, which asserts the mirror).
 *
 * Design notes:
 *   - Parts are a discriminated union on `type` (text/reasoning/tool_call/tool_result)
 *     so the UI can switch on it and so new part kinds are additive.
 *   - A Message carries a `parts[]` array plus a `pending` flag: a message is created
 *     pending (the supervisor is still streaming) and flipped to done when finished.
 *   - BoughEvent is the SSE envelope. `data` is left as unknown here because its shape
 *     is per-event-type; the typed payloads live below as *EventData schemas and the
 *     bus stamps `seq`/`ts`. Validation of `data` is the emitter's job, not the wire's.
 */
import { z } from "zod";

// ---- roles & kinds ---------------------------------------------------------

export const Role = z.enum(["user", "supervisor", "worker"]);
export type Role = z.infer<typeof Role>;

export const SessionKind = z.enum(["root", "fork", "worker", "compaction"]);
export type SessionKind = z.infer<typeof SessionKind>;

// ---- parts -----------------------------------------------------------------

export const TextPart = z.object({ type: z.literal("text"), text: z.string() });
export const ReasoningPart = z.object({ type: z.literal("reasoning"), text: z.string() });
export const ToolCallPart = z.object({
  type: z.literal("tool_call"),
  id: z.string(),
  name: z.string(),
  input: z.unknown(),
});
export const ToolResultPart = z.object({
  type: z.literal("tool_result"),
  callId: z.string(),
  output: z.unknown(),
  isError: z.boolean(),
});

export const Part = z.discriminatedUnion("type", [
  TextPart,
  ReasoningPart,
  ToolCallPart,
  ToolResultPart,
]);
export type Part = z.infer<typeof Part>;

// ---- message & session -----------------------------------------------------

export const Message = z.object({
  id: z.string(),
  sessionId: z.string(),
  role: Role,
  parts: z.array(Part),
  pending: z.boolean(),
  createdAt: z.number(),
});
export type Message = z.infer<typeof Message>;

export const Session = z.object({
  id: z.string(),
  parentId: z.string().nullable(),
  title: z.string(),
  kind: SessionKind,
  createdAt: z.number(),
  // The session's read-write root. Optional/additive: absent for sessions with no
  // configured workspace (the turn runner falls back to BOUGH_WORKSPACE/cwd). The
  // Changes API (#10) exposes/sets this over HTTP; the mirror in web/src/types.ts
  // adds it as an optional field to match.
  workspace: z.string().nullish(),
  // Lineage (additive; set only on branched sessions). For a fork: the session it was
  // forked from + the at-message. For a compaction: the compacted session + the span-end
  // message. Root/plain sessions omit them. Gives the map real lineage edges.
  originId: z.string().nullish(),
  originMessageId: z.string().nullish(),
});
export type Session = z.infer<typeof Session>;

// ---- network ---------------------------------------------------------------

// One outbound request row for the Network rail. Mirrors NetRequest in the UI;
// the net gate owner emits these as `net.request` events.
export const NetRequest = z.object({
  id: z.string(),
  host: z.string(),
  verb: z.string().optional(),
  action: z.string(),
  verdict: z.enum(["allowed", "denied", "pending"]),
  reason: z.string().optional(),
  requestedBy: z.string().optional(),
  ts: z.number(),
});
export type NetRequest = z.infer<typeof NetRequest>;

// ---- event envelope --------------------------------------------------------

// The SSE envelope. `type` names the event (also the SSE `event:` field); `seq` is a
// process-monotonic counter stamped by the bus; `data` is the per-type payload.
export const BoughEvent = z.object({
  type: z.string(),
  sessionId: z.string().optional(),
  seq: z.number(),
  ts: z.number(),
  data: z.unknown(),
});
export type BoughEvent = z.infer<typeof BoughEvent>;

// ---- request bodies (route validation) -------------------------------------

export const CreateSessionBody = z.object({
  // Optional: when absent the session is created as "untitled" and the title worker
  // names it from the first user message (see supervisor/title.ts).
  title: z.string().optional(),
  parentId: z.string().nullable().optional(),
  kind: SessionKind.optional(),
  // Optional read-write root for the session; persisted and returned on the Session.
  workspace: z.string().optional(),
});
export type CreateSessionBody = z.infer<typeof CreateSessionBody>;

export const PostMessageBody = z.object({ text: z.string() });
export type PostMessageBody = z.infer<typeof PostMessageBody>;

// ---- typed event payloads --------------------------------------------------
// The shapes the store reduces (web/src/store.ts). Kept here so emitters can be
// checked against the same contract the UI consumes. `data` of each named event:

/** `session.created` → a Session (the full row). */
export const SessionCreatedData = Session;
/** `session.updated` → a Session (the full row after a change, e.g. the title worker). */
export const SessionUpdatedData = Session;
/** `message.started` → a Message (created pending). */
export const MessageStartedData = Message;
/** `message.delta` → an incremental text chunk for a streaming message. */
export const MessageDeltaData = z.object({ messageId: z.string(), delta: z.string() });
/** `message.part` → a finalized Part appended to a message. */
export const MessagePartData = z.object({ messageId: z.string(), part: Part });
/** `message.finished` → the message is complete (flip pending → false). */
export const MessageFinishedData = z.object({ messageId: z.string() });
