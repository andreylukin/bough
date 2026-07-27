/**
 * The wire contract. Every shape that crosses server↔client, server↔db, or
 * server↔worker is declared here once, as a Zod schema, so there is exactly one
 * definition of "what a Message is" for the router, the TUI store, the CLI and the
 * database layer to agree on.
 *
 * The invariant this module holds is *derived visibility*: a Session carries its
 * lineage (`kind`, `parentId`, `originId`) and nothing else. There is no
 * `archivedAt`, no `deprecatedAt`, no hidden flag — a subagent collapses under its
 * origin because of what it IS, not because something marked it. Any code that
 * wants to hide a session computes it from these fields; adding a column here to
 * store the answer would put two sources of truth in the tree view (spec §4).
 *
 * Second invariant: parts are a discriminated union on `type`. The UI switches on
 * it exhaustively and replay maps each arm to a provider block, so a new part kind
 * is an additive change that both sides are forced by the compiler to handle.
 *
 * Third: image bytes never live in the parts JSON. An `image` part stores a path
 * under ~/.bough/attachments/, so message rows stay small and a replay survives the
 * user moving the original file.
 */
import { z } from "zod";

// ---- roles & kinds ---------------------------------------------------------

/**
 * `system` = harness-injected notes (a detached subagent's report, a background
 * job's exit, artifact comments). They render distinctly in the UI and replay to
 * the model as user-side text — they are not a provider role.
 */
export const Role = z.enum(["user", "supervisor", "system"]);
export type Role = z.infer<typeof Role>;

/**
 * Visibility is derived from this plus lineage: `subagent` and `workflow_agent`
 * collapse under their `originId` and surface only on drill-in; `root`, `fork` and
 * `compaction` are always listed (spec §4).
 */
export const SessionKind = z.enum([
  "root",
  "fork",
  "compaction",
  "subagent",
  "workflow_agent",
]);
export type SessionKind = z.infer<typeof SessionKind>;

// ---- parts -----------------------------------------------------------------

/** Model prose. The only part kind a turn is *required* to produce (spec §5). */
export const TextPart = z.object({ type: z.literal("text"), text: z.string() });
export type TextPart = z.infer<typeof TextPart>;

/**
 * Summarized thinking, persisted for DISPLAY ONLY. Reasoning parts are dropped on
 * replay (plan §6.4) — there are no signed thinking blocks to echo back, so
 * re-sending them would be both wrong and expensive.
 */
export const ReasoningPart = z.object({ type: z.literal("reasoning"), text: z.string() });
export type ReasoningPart = z.infer<typeof ReasoningPart>;

/** One `run_steps` / `stop` call. `input` is unknown here; the tool's own schema validates it. */
export const ToolCallPart = z.object({
  type: z.literal("tool_call"),
  id: z.string(),
  name: z.string(),
  input: z.unknown(),
});
export type ToolCallPart = z.infer<typeof ToolCallPart>;

export const ToolResultPart = z.object({
  type: z.literal("tool_result"),
  callId: z.string(),
  output: z.unknown(),
  isError: z.boolean(),
  /**
   * The call was stopped by a user interrupt rather than completing — `output`
   * holds whatever partial work survived. Distinct from `isError`, and rendered
   * distinctly, because "you stopped it" and "it failed" are different facts
   * for both the user and the next round (spec §5, §6).
   */
  interrupted: z.boolean().optional(),
});
export type ToolResultPart = z.infer<typeof ToolResultPart>;

/**
 * An image the model can see: a user attachment from the composer, or one the
 * program handed over with `image()`. The bytes live at `path` under
 * ~/.bough/attachments/, never inline. `name` is the reference as the user typed
 * it; `size` (bytes at attach time) feeds the UI placeholder. A lost file replays
 * as placeholder text rather than failing the turn (plan T2.2).
 */
export const ImagePart = z.object({
  type: z.literal("image"),
  path: z.string(),
  mediaType: z.string(),
  name: z.string(),
  size: z.number(),
});
export type ImagePart = z.infer<typeof ImagePart>;

/**
 * A SETTLED `ask()` hold. Appended only once resolved — never while pending — so
 * replay can render it as plain text and it can never re-block (plan §6.5). `id`
 * joins the row to the `ask.question` events that announced it live.
 */
export const AskPart = z.object({
  type: z.literal("ask"),
  id: z.string(),
  question: z.string(),
  options: z.array(z.string()).optional(),
  status: z.enum(["answered", "declined", "interrupted"]),
  answer: z.string().optional(),
});
export type AskPart = z.infer<typeof AskPart>;

/** The six part kinds of spec §4. Discriminated on `type`. */
export const Part = z.discriminatedUnion("type", [
  TextPart,
  ReasoningPart,
  ToolCallPart,
  ToolResultPart,
  ImagePart,
  AskPart,
]);
export type Part = z.infer<typeof Part>;

// ---- messages --------------------------------------------------------------

/**
 * `pending` is the streaming flag: a supervisor message is created pending and
 * flipped when `message.finished` lands. Ordering is `(createdAt, rowid)` — see
 * plan §6.1: seeding uses a real clock so a turn started afterwards always sorts
 * after the seed, even within the same millisecond.
 */
export const Message = z.object({
  id: z.string(),
  sessionId: z.string(),
  role: Role,
  parts: z.array(Part),
  pending: z.boolean(),
  createdAt: z.number(),
});
export type Message = z.infer<typeof Message>;

// ---- sessions --------------------------------------------------------------

/**
 * One conversation. Note what is absent: no archive, deprecate, hide or purge
 * field. Visibility is derived from `kind` + `originId` (spec §4, §17).
 */
export const Session = z.object({
  id: z.string(),
  title: z.string(),
  kind: SessionKind,
  createdAt: z.number(),
  /**
   * Thread inheritance: a session's thread is its ancestors' messages ++ its own.
   * Fork and compaction parent at the TARGET's parent, so shared ancestors come
   * for free and only the branch's own seeded messages are copied (spec §14).
   * A subagent has `parentId: null` — a fresh, task-only thread (spec §7).
   */
  parentId: z.string().nullable(),
  /** Lineage edge for the tree view: what this branched from, and at which message. */
  originId: z.string().nullish(),
  originMessageId: z.string().nullish(),
  /** The checkout the session operates on, edited in place. */
  workspace: z.string().nullish(),
  /**
   * The project directory the session was created on. Mirrors `workspace` at
   * creation and is never rewritten, so it stays the stable record of WHICH
   * project this session is for.
   */
  originDir: z.string().nullish(),
  /**
   * The git sha the session started from — the Changes rail is
   * `git diff <base>` plus untracked files. Absent for a non-git workspace,
   * which therefore has no change set and no revert (spec §13).
   */
  base: z.string().nullish(),
  /** Per-session pins; absent = the global default (spec §12). */
  model: z.string().nullish(),
  effort: z.string().nullish(),
  /** Prefilled composer text, set by handoff. Cleared server-side by the first post. */
  draft: z.string().nullish(),
  /**
   * Status-bar context/cost display (spec §5 Usage, §15). `contextTokens` is the
   * last round's full prompt size; `cachedTokens` the share of it served from or
   * written to the provider cache; `lastLlmAt` when that round finished — cache
   * warmth is a time-decaying property the client derives from it, not a stored
   * boolean.
   */
  contextTokens: z.number().nullish(),
  cachedTokens: z.number().nullish(),
  lastLlmAt: z.number().nullish(),
  /**
   * Delegation outcome, stamped on a `subagent`/`workflow_agent` session when its
   * turn finishes, so the tree can render a failed branch. There is no acceptance
   * gate — this records whether the turn errored, not whether the work was
   * accepted (spec §5, §17).
   */
  outcomeOk: z.boolean().nullish(),
});
export type Session = z.infer<typeof Session>;

// ---- turns -----------------------------------------------------------------

/**
 * `orphaned` is what a `running` turn becomes when the server restarts under it:
 * the checkpoint tells recovery the turn cannot be resumed, and the session
 * unblocks instead of hanging on a pending message forever (spec §4, plan T2.3).
 */
export const TurnStatus = z.enum(["running", "done", "error", "interrupted", "orphaned"]);
export type TurnStatus = z.infer<typeof TurnStatus>;

/** Per-round provider usage, summed across the turn and aggregated per session. */
export const Usage = z.object({
  inputTokens: z.number(),
  outputTokens: z.number(),
  /** Tracked separately from input/output — spec §5 Usage. */
  reasoningTokens: z.number().nullish(),
  cacheReadTokens: z.number().nullish(),
  cacheWriteTokens: z.number().nullish(),
  costUsd: z.number().nullish(),
});
export type Usage = z.infer<typeof Usage>;

/**
 * The persisted state machine covering everything after a user message lands.
 * Checkpointed as it progresses (`step`) so a restart can find turns still marked
 * `running` and orphan them.
 */
export const Turn = z.object({
  id: z.string(),
  sessionId: z.string(),
  /** The pending supervisor message this turn is producing. */
  messageId: z.string(),
  status: TurnStatus,
  /** Last checkpoint, human-readable. Written after each API round and tool result. */
  step: z.string(),
  createdAt: z.number(),
  updatedAt: z.number(),
  /** Present when status is `error`; the message names the limit or the failure. */
  error: z.string().nullish(),
  usage: Usage.nullish(),
});
export type Turn = z.infer<typeof Turn>;

// ---- ask() questions -------------------------------------------------------

/**
 * One live `ask()` hold. Memory-only server-side (spec §6: "the hold dies with the
 * turn") — the durable record is the settled `AskPart` on the supervisor message.
 * Emitted as `ask.question` on raise and re-emitted on the same `id` with its final
 * status, so a reconnecting client rebuilds the card from `GET /questions` rather
 * than from replayed events.
 */
export const AskQuestion = z.object({
  id: z.string(),
  sessionId: z.string(),
  /** The supervisor message whose turn raised it — the transcript anchor. */
  messageId: z.string(),
  question: z.string(),
  /** Pick-one choices; absent = free text only. Free text is always possible. */
  options: z.array(z.string()).optional(),
  status: z.enum(["pending", "answered", "declined", "interrupted"]),
  answer: z.string().optional(),
  ts: z.number(),
});
export type AskQuestion = z.infer<typeof AskQuestion>;

// ---- background jobs -------------------------------------------------------

/**
 * An auto-backgrounded (`bash` past 60s) or explicit (`bashBg`) shell. Tracked per
 * session and outliving the turn, but NOT persisted: a job's process dies with the
 * server, so a stored row would always be a lie after a restart (spec §9).
 */
export const BackgroundJob = z.object({
  id: z.string(),
  sessionId: z.string(),
  pid: z.number(),
  command: z.string(),
  status: z.enum(["running", "exited"]),
  exitCode: z.number().nullish(),
  startedAt: z.number(),
  exitedAt: z.number().nullish(),
});
export type BackgroundJob = z.infer<typeof BackgroundJob>;

// ---- schedules -------------------------------------------------------------

/**
 * A recurring run. `nextRunAt` advances FROM NOW at fire time, never from the
 * stale stored value — a server down through N missed slots fires once on the
 * first tick after boot, then resumes cadence (spec §9, plan §6.8).
 */
export const Schedule = z.object({
  id: z.string(),
  title: z.string(),
  prompt: z.string(),
  workspace: z.string().nullable(),
  /** `every:<N><m|h|d>` (N ≥ 1) or `daily@HH:MM` (local wall clock). Stored verbatim. */
  spec: z.string(),
  enabled: z.boolean(),
  createdAt: z.number(),
  lastRunAt: z.number().nullable(),
  nextRunAt: z.number(),
});
export type Schedule = z.infer<typeof Schedule>;

// ---- workflows -------------------------------------------------------------

export const WorkflowStatus = z.enum([
  "running",
  "paused",
  "done",
  "error",
  "stopped",
  "orphaned",
]);
export type WorkflowStatus = z.infer<typeof WorkflowStatus>;

/** From the script's `meta` literal, extracted host-side before the body runs. */
export const WorkflowPhase = z.object({
  title: z.string(),
  detail: z.string().optional(),
});
export type WorkflowPhase = z.infer<typeof WorkflowPhase>;

/**
 * One detached orchestration run. The script text is persisted verbatim (and
 * mirrored to ~/.bough/workflows/<id>.js for out-of-band editing) so a rerun can
 * diff against it.
 */
export const WorkflowRun = z.object({
  id: z.string(),
  sessionId: z.string(),
  name: z.string(),
  description: z.string(),
  script: z.string(),
  phases: z.array(WorkflowPhase),
  status: WorkflowStatus,
  currentPhase: z.string().nullable(),
  /** The script's return value (status `done`). */
  result: z.unknown(),
  error: z.string().nullable(),
  /** The run's input value, handed to the script as `args` verbatim. */
  args: z.unknown(),
  /** The run whose journal this rerun replays from. */
  resumeOf: z.string().nullable(),
  createdAt: z.number(),
  finishedAt: z.number().nullable(),
});
export type WorkflowRun = z.infer<typeof WorkflowRun>;

export const WorkflowAgentStatus = z.enum([
  "queued",
  "running",
  "done",
  "error",
  "stopped",
  /** Replayed from the source run's journal — no live agent call was made. */
  "cached",
]);
export type WorkflowAgentStatus = z.infer<typeof WorkflowAgentStatus>;

/**
 * One `agent()` call's journal row — the unit a rerun replays. `key` is
 * `hash(prompt + opts)`: a rerun replays every hit instantly and re-runs only the
 * calls whose key changed, which is why workflow scripts must be deterministic
 * (plan §6.15).
 */
export const WorkflowAgent = z.object({
  id: z.string(),
  runId: z.string(),
  /** Call order within the run. */
  idx: z.number(),
  key: z.string(),
  label: z.string(),
  phase: z.string().nullable(),
  prompt: z.string(),
  model: z.string().nullable(),
  status: WorkflowAgentStatus,
  /** The agent's report — raw text, or the JSON of a `{schema}` call. */
  result: z.string().nullable(),
  /** Present when the call failed; the message names what went wrong. */
  error: z.string().nullable(),
  /** The subagent session backing this call. Absent for cached replays. */
  sessionId: z.string().nullable(),
  startedAt: z.number(),
  finishedAt: z.number().nullable(),
});
export type WorkflowAgent = z.infer<typeof WorkflowAgent>;
