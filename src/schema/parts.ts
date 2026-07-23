/**
 * The wire contract — Zod schemas for everything that crosses the server↔UI (and
 * server↔db) boundary. This is the source of truth for the wire shapes the TUI and
 * headless CLI consume; any change here must keep round-tripping (see parts.test.ts).
 *
 * Design notes:
 *   - Parts are a discriminated union on `type` (text/reasoning/tool_call/tool_result/image)
 *     so the UI can switch on it and so new part kinds are additive.
 *   - A Message carries a `parts[]` array plus a `pending` flag: a message is created
 *     pending (the supervisor is still streaming) and flipped to done when finished.
 *   - BoughEvent is the SSE envelope. `data` is left as unknown here because its shape
 *     is per-event-type; the typed payloads live below as *EventData schemas and the
 *     bus stamps `seq`/`ts`. Validation of `data` is the emitter's job, not the wire's.
 */
import { z } from "zod";

// ---- roles & kinds ---------------------------------------------------------

// "system" = harness-injected notes (e.g. a background subagent's finished report);
// they render distinctly in the UI and replay to the model as user-side text.
export const Role = z.enum(["user", "supervisor", "worker", "system"]);
export type Role = z.infer<typeof Role>;

export const SessionKind = z.enum(["root", "fork", "worker", "compaction", "subagent"]);
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
  // The call was stopped by a user interrupt, not completed — the output holds
  // whatever partial work it produced. The UI renders this distinctly from done.
  interrupted: z.boolean().optional(),
});
// A user-attached image (composer `@shot.png`). The bytes live OUTSIDE the parts
// JSON: composing the message copies the referenced file to
// ~/.bough/attachments/<uuid>.<ext> and the part stores only that `path` — the db
// row stays small and the message replays even after the workspace file moves.
// `name` is the reference as the user typed it; `size` (bytes, at attach time)
// feeds the UI's placeholder line.
export const ImagePart = z.object({
  type: z.literal("image"),
  path: z.string(),
  mediaType: z.string(),
  name: z.string(),
  size: z.number(),
});
export type ImagePart = z.infer<typeof ImagePart>;
// A settled ask() hold (asks.ts): the question the program raised mid-turn and how
// it ended. Appended only once resolved — never "pending" — so replay can render it
// as plain text and can never re-block. `id` joins the row to its ask.question events.
export const AskPart = z.object({
  type: z.literal("ask"),
  id: z.string(),
  question: z.string(),
  options: z.array(z.string()).optional(),
  status: z.enum(["answered", "declined", "interrupted"]),
  answer: z.string().optional(),
});

// The turn's marked-up answer, published via the prose() host function: markdown
// the UI renders with full styling, visually set off from tool chatter. Appended
// mid-program (between a tool_call and its tool_result) like an ask part. Dropped
// on replay — the text already replays verbatim inside its program's tool_call
// input, so echoing it would double-bill the answer.
export const ProsePart = z.object({ type: z.literal("prose"), text: z.string() });

export const Part = z.discriminatedUnion("type", [
  TextPart,
  ReasoningPart,
  ToolCallPart,
  ToolResultPart,
  ImagePart,
  AskPart,
  ProsePart,
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
  // Changes API (#10) exposes/sets this over HTTP
  // adds it as an optional field to match.
  workspace: z.string().nullish(),
  // The user's project directory the session was created on (additive). The
  // workspace column is repointed at the internal shadow worktree on the first
  // turn, so this is the stable record of WHICH project the session is for —
  // the UI shows it in the status bar and on session-tree rows.
  originDir: z.string().nullish(),
  // Lineage (additive; set only on branched sessions). For a fork: the session it was
  // forked from + the at-message. For a compaction: the compacted session + the span-end
  // message. Root/plain sessions omit them. Gives the map real lineage edges.
  originId: z.string().nullish(),
  originMessageId: z.string().nullish(),
  // Set when a branch is deprecated — hidden by default in the tree views, shown
  // on toggle. Distinct from archive (which removes it from the list entirely).
  deprecatedAt: z.number().nullish(),
  // Set when a session is archived (soft delete). Absent from the default
  // GET /sessions; ?archived=1 returns these rows so a UI can reveal/restore them.
  archivedAt: z.number().nullish(),
  // Per-session model override (additive). Absent = the process-global default.
  // Set by the model picker: switching models pins THIS session and moves the
  // default new sessions start on; other existing sessions keep theirs.
  model: z.string().nullish(),
  // Per-session thinking-depth override (additive; same pinning semantics as
  // model). One of low/medium/high/xhigh/max; absent = the global default.
  effort: z.string().nullish(),
  // Per-session system-prompt override dir (additive). When set, the turn runner
  // resolves the supervisor prompt's section files from this directory instead of
  // the checked-in default — so prompt variants can be swapped per session with NO
  // server restart (used by `bough exec --prompt-dir` and the tuner). Absent = the
  // process default (BOUGH_PROMPT_DIR env, else the built-in ./prompt dir).
  promptDir: z.string().nullish(),
  // Prompt-cache visibility (additive; stamped after each turn's last LLM round).
  // contextTokens = that round's full prompt size; cachedTokens = the share of it
  // served from / written to the provider's prompt cache; lastLlmAt = when the round
  // finished. The UI derives warm/cold from lastLlmAt + the provider TTL (~5 min,
  // sliding) — cache state is a time-decaying property, so it's computed client-side.
  contextTokens: z.number().nullish(),
  cachedTokens: z.number().nullish(),
  lastLlmAt: z.number().nullish(),
  // A drafted-but-unsent opening prompt (additive; set by handoff). The UI prefills
  // the composer with it; posting the session's first message clears it server-side.
  draft: z.string().nullish(),
  // Delegation outcome (additive; stamped on a subagent when its spawned turn
  // finishes). The blocking agent() result returns in-band to the parent PROGRAM
  // only — persisting {ok, checkPassed} here lets the UI render failed/check-failed
  // branches. Absent for non-subagents and legacy rows.
  outcomeOk: z.boolean().nullish(),
  outcomeCheckPassed: z.boolean().nullish(),
});
export type Session = z.infer<typeof Session>;

// ---- network ---------------------------------------------------------------

// One outbound request row for the Network rail. Mirrors NetRequest in the UI;
// the net gate owner emits these as `net.request` events.
export const NetRequest = z.object({
  id: z.string(),
  /** Branch that owns this egress; absent for pre-attribution rows. */
  sessionId: z.string().optional(),
  host: z.string(),
  verb: z.string().optional(),
  /** Request path (+query) — the approval card must show WHAT is being fetched,
   * not just the hostname (user-testing: a bare host isn't enough to decide on). */
  path: z.string().optional(),
  action: z.string(),
  verdict: z.enum(["allowed", "denied", "pending"]),
  reason: z.string().optional(),
  requestedBy: z.string().optional(),
  /** Facet fields — the classifier's parsed view (e.g. k8s resource/namespace). */
  fields: z.record(z.string(), z.unknown()).optional(),
  /** Local-worker one-liner ("Creates a fork of repo X") — advisory, may lag. */
  annotation: z.string().optional(),
  /** Request headers with credential values redacted — the approval card's
   * "show me the raw request" detail view (user-testing: skeptics won't approve
   * what they can't inspect). */
  headers: z.record(z.string(), z.string()).optional(),
  /** First bytes of the request body, if any (clipped at the gate). */
  bodyPreview: z.string().optional(),
  ts: z.number(),
});
export type NetRequest = z.infer<typeof NetRequest>;

// ---- ask() questions -------------------------------------------------------

// One mid-task question a run_steps program raised via ask() (asks.ts). The hold
// mirror of NetRequest: emitted as `ask.question` when raised (status "pending")
// and re-emitted on the same id with its final status. Memory-only server-side —
// the settled record persists as an AskPart on the supervisor message.
export const AskQuestion = z.object({
  id: z.string(),
  sessionId: z.string(),
  /** The supervisor message whose turn raised it (the transcript anchor). */
  messageId: z.string(),
  question: z.string(),
  /** Pick-one choices; absent = free-text only. Free text is always possible. */
  options: z.array(z.string()).optional(),
  status: z.enum(["pending", "answered", "declined", "interrupted"]),
  answer: z.string().optional(),
  ts: z.number(),
});
export type AskQuestion = z.infer<typeof AskQuestion>;

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
  // Optional model pin (same semantics as the picker's per-session pin). Used by
  // `bough exec -m`; absent = the process-global default.
  model: z.string().optional(),
  // Optional system-prompt override dir (section .md files). Pins a prompt variant
  // on this session with no server restart. Used by `bough exec --prompt-dir` and
  // the prompt tuner; absent = the process default.
  promptDir: z.string().optional(),
});
export type CreateSessionBody = z.infer<typeof CreateSessionBody>;

export const PostMessageBody = z.object({ text: z.string() });
export type PostMessageBody = z.infer<typeof PostMessageBody>;

// POST /sessions/:id/questions/:qid — {answer} settles the hold; {decline: true}
// rejects the program's ask() with a "user declined" error it can catch.
export const AnswerQuestionBody = z.object({
  answer: z.string().optional(),
  decline: z.boolean().optional(),
});
export type AnswerQuestionBody = z.infer<typeof AnswerQuestionBody>;

// ---- typed event payloads --------------------------------------------------
// The shapes the TUI store reduces — `BoughEvent.data` per event name. Recorded
// as prose, not schemas: nothing validates the bus, so a schema here would be an
// unenforced second source of truth. The `bus.publish` calls in turn.ts are
// canonical; src/tui/store.ts reduces them.
//
//   session.created  → Session (the full row)
//   session.updated  → Session (the full row after a change, e.g. the title worker)
//   message.started  → Message (created pending)
//   message.delta    → { messageId, delta } — incremental text for a streaming message
//   message.retry    → { messageId } — the LLM round failed transiently and is being
//                      re-attempted; the message re-streams from the top, so UIs
//                      drop their streaming buffer
//   message.part     → { messageId, part: Part } — a finalized Part appended
//   tool.log         → { messageId, callId, line } — one console.* line from a
//                      running program, keyed to its tool_call
//   message.finished → { messageId } — the message is complete (pending → false)
//   ask.question     → AskQuestion (pending on raise; re-emitted with its final status)
//   workflow.updated → WorkflowRun (db/db.ts) — a run's row after any change
//   workflow.agent   → WorkflowAgent — one agent() call's journal row after any change
//   workflow.log     → { runId, line } — one narrator log() line from a running script
