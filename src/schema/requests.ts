/**
 * Request bodies for every route the spec describes. One Zod schema per body,
 * parsed at the router edge and nowhere else — a handler receives data that is
 * already the right shape, so no handler contains validation logic and no domain
 * module re-checks its inputs.
 *
 * The invariant worth stating: these schemas are the ONLY place a 400 is decided.
 * `parseBody` turns a failed parse into an `HttpError(400)` that the router's one
 * catch renders; semantic 400s (a fork point that isn't the session's own message,
 * a schedule spec that doesn't parse) belong to the domain module, not here,
 * because they need state this layer does not have.
 *
 * Naming rule: `<Verb><Noun>Body`, exported alongside an inferred type of the
 * same name so callers write `body: CreateSessionBody`.
 */
import { z } from "zod";
import { SessionKind } from "./parts.ts";

// ---- shared -----------------------------------------------------------------

/**
 * One message selected out of a thread, optionally narrowed to specific part
 * indexes. Part indexes are what let extract copy a turn's prose without dragging
 * its tool calls along (spec §14). Absent `parts` = the whole message.
 */
export const PartPick = z.object({
  messageId: z.string(),
  parts: z.array(z.number().int().nonnegative()).min(1).optional(),
});
export type PartPick = z.infer<typeof PartPick>;

// ---- sessions ---------------------------------------------------------------

/** POST /sessions */
export const CreateSessionBody = z.object({
  /** Absent = the session is created untitled and the cheap tier names it. */
  title: z.string().optional(),
  parentId: z.string().nullable().optional(),
  kind: SessionKind.optional(),
  /** The checkout the session operates on. Must exist at creation time. */
  workspace: z.string().optional(),
  /** Per-session pins; absent = the global defaults. */
  model: z.string().optional(),
  effort: z.string().optional(),
});
export type CreateSessionBody = z.infer<typeof CreateSessionBody>;

/**
 * POST /sessions/:id/messages. A message posted while a turn runs is queued and
 * drains into a fresh turn — it is never dropped and never races the running one
 * (spec §5).
 */
export const PostMessageBody = z.object({
  text: z.string(),
  /**
   * Composer attachments, already copied under ~/.bough/attachments/ by the
   * caller. Absent = a text-only message.
   */
  images: z.array(z.object({
    path: z.string(),
    mediaType: z.string(),
    name: z.string(),
    size: z.number(),
  })).optional(),
});
export type PostMessageBody = z.infer<typeof PostMessageBody>;

/** PUT /sessions/:id/draft — `null` clears the prefilled composer text. */
export const SetDraftBody = z.object({
  draft: z.string().nullable(),
});
export type SetDraftBody = z.infer<typeof SetDraftBody>;

/**
 * PATCH /sessions/:id — the per-session `model` / `effort` overrides (spec §4).
 *
 * Absent and `null` mean different things and both are needed: an absent field leaves
 * the override alone, an explicit `null` clears it so the session falls back to the
 * global default. A picker that can pin but not unpin is only half a control.
 */
export const PatchSessionBody = z.object({
  model: z.string().min(1).nullable().optional(),
  effort: z.enum(["low", "medium", "high", "xhigh", "max"]).nullable().optional(),
});
export type PatchSessionBody = z.infer<typeof PatchSessionBody>;

/**
 * PUT /model-settings — what a NEW conversation runs on, for the whole install.
 *
 * The same shape as a session pin and deliberately so: the picker commits one
 * choice to two scopes, this session and the next one. An absent key is left alone;
 * an explicit `null` clears the pin.
 */
export const PutModelSettingsBody = z.object({
  model: z.string().min(1).nullable().optional(),
  effort: z.enum(["low", "medium", "high", "xhigh", "max"]).nullable().optional(),
});
export type PutModelSettingsBody = z.infer<typeof PutModelSettingsBody>;

/**
 * POST /sessions/:id/questions/:qid — `{answer}` settles the hold; `{decline:
 * true}` rejects the program's `ask()` with a catchable "user declined" so the
 * program can proceed on a stated default or stop cleanly (spec §6).
 */
export const AnswerQuestionBody = z.object({
  answer: z.string().optional(),
  decline: z.boolean().optional(),
});
export type AnswerQuestionBody = z.infer<typeof AnswerQuestionBody>;

// ---- history operations (spec §14) ------------------------------------------

/**
 * POST /sessions/:id/fork. `editedText` makes it "edit & resend": the branch
 * replaces that turn's user message and runs a fresh turn. Limited to the
 * session's OWN messages — a fork point in ancestor history is a 400 telling the
 * user to operate on the ancestor.
 */
export const ForkBody = z.object({
  atMessageId: z.string(),
  /** Cut inside the at-message: keep `parts[0..atPart]` of it. */
  atPart: z.number().int().nonnegative().optional(),
  editedText: z.string().optional(),
  /**
   * Cut BEFORE the at-message rather than including it — for the plain
   * branch-point case where the caller intends to re-send it itself.
   */
  exclusive: z.boolean().optional(),
  /**
   * Carry a summary of the ABANDONED tail onto the branch — pi's
   * branch-summary-on-switch. The abandoned path is everything from the fork point
   * to the end of the source, which is exactly what you stop being able to see
   * once you branch. Off by default: it costs an LLM call.
   */
  summarizeAbandoned: z.boolean().optional(),
});
export type ForkBody = z.infer<typeof ForkBody>;

/**
 * POST /sessions/:id/compact. Each maximal contiguous run of picks collapses to
 * one summary in place; unselected messages between runs are copied verbatim.
 * Own messages only, same rule as fork.
 */
export const CompactBody = z.object({
  picks: z.array(PartPick).min(1),
  /** Steers what the summary keeps. Absent = the default summarization prompt. */
  instructions: z.string().optional(),
});
export type CompactBody = z.infer<typeof CompactBody>;

/**
 * POST /sessions/:id/sections. Stateless and read-only: the client sends one gist
 * per turn in thread order, and index i of the reply is turn i. Sending gists
 * rather than ids guarantees the returned ranges align with what the user sees,
 * whatever the client's own turn grouping is.
 */
export const SectionsBody = z.object({
  turns: z.array(z.object({ gist: z.string().max(500) })).min(1).max(500),
});
export type SectionsBody = z.infer<typeof SectionsBody>;

/**
 * POST /sessions/:id/extract — copy picked messages into a fresh ROOT. Picks may
 * be any message in the visible thread, ancestors included.
 */
export const ExtractBody = z.object({
  picks: z.array(PartPick).min(1),
});
export type ExtractBody = z.infer<typeof ExtractBody>;

/**
 * POST /sessions/:id/move-into — append copies of `sourceId`'s picked messages
 * onto THIS session. A copy, never a move: the source keeps its turns.
 */
export const MoveBody = z.object({
  sourceId: z.string(),
  picks: z.array(PartPick).min(1),
});
export type MoveBody = z.infer<typeof MoveBody>;

/**
 * POST /sessions/:id/handoff — draft the opening prompt for a fresh root from a
 * stated goal. The goal drives what the draft keeps and drops; the source session
 * is never mutated.
 */
export const HandoffBody = z.object({
  goal: z.string().min(1),
});
export type HandoffBody = z.infer<typeof HandoffBody>;

// ---- changes (spec §13) ------------------------------------------------------

/**
 * POST /sessions/:id/changes/revert. Revert is the only mutation the Changes rail
 * offers: restore tracked paths from the base sha and delete untracked ones, PER
 * PATH, never touching anything the session did not change. Empty/absent `paths`
 * reverts the session's whole change set.
 */
export const RevertChangesBody = z.object({
  paths: z.array(z.string()).optional(),
});
export type RevertChangesBody = z.infer<typeof RevertChangesBody>;

// ---- schedules (spec §9) -----------------------------------------------------

/** POST /schedules. `spec` grammar is validated by the schedules module, not here. */
export const CreateScheduleBody = z.object({
  title: z.string().min(1),
  prompt: z.string().min(1),
  workspace: z.string().min(1).optional(),
  spec: z.string(),
  enabled: z.boolean().optional(),
});
export type CreateScheduleBody = z.infer<typeof CreateScheduleBody>;

/** PATCH /schedules/:id — every field optional; `workspace: null` clears it. */
export const PatchScheduleBody = z.object({
  title: z.string().min(1).optional(),
  prompt: z.string().min(1).optional(),
  workspace: z.string().min(1).nullable().optional(),
  spec: z.string().optional(),
  enabled: z.boolean().optional(),
});
export type PatchScheduleBody = z.infer<typeof PatchScheduleBody>;

// ---- workflows (spec §8) -----------------------------------------------------

/** POST /workflows */
export const CreateWorkflowBody = z.object({
  sessionId: z.string().min(1),
  script: z.string().min(1),
  /** Handed to the script as `args`, verbatim. */
  args: z.unknown().optional(),
});
export type CreateWorkflowBody = z.infer<typeof CreateWorkflowBody>;

/**
 * POST /workflows/:id/rerun. With `script`, the edited source replaces the
 * original and only calls whose journal key changed re-run.
 */
export const RerunWorkflowBody = z.object({
  script: z.string().min(1).optional(),
  args: z.unknown().optional(),
});
export type RerunWorkflowBody = z.infer<typeof RerunWorkflowBody>;

// ---- artifacts and comments (spec §11) ---------------------------------------

/**
 * POST /sessions/:id/comments — one pinned note on a served artifact page. The
 * sidecar these accumulate in lives OUTSIDE the artifact directory, or listing
 * walks them (plan §6.12).
 */
export const PostCommentBody = z.object({
  /** The artifact the note is pinned to, relative to the session's artifact dir. */
  artifact: z.string().min(1),
  text: z.string().min(1),
  /** Free-form anchor the injected widget records (selector, offsets, …). */
  anchor: z.unknown().optional(),
});
export type PostCommentBody = z.infer<typeof PostCommentBody>;

/**
 * POST /sessions/:id/comments/send — deliver the pending batch as one
 * `[artifact comments]` system message for the agent to act on.
 */
export const SendCommentsBody = z.object({
  ids: z.array(z.string()).optional(),
});
export type SendCommentsBody = z.infer<typeof SendCommentsBody>;

// ---- config, keys, theme (spec §12, §16) -------------------------------------

/**
 * PATCH /config. Switching the model moves the default that NEW sessions start
 * on and pins `sessionId` when given; every other existing session keeps what it
 * was on (spec §12).
 */
export const PatchConfigBody = z.object({
  model: z.string().optional(),
  effort: z.string().optional(),
  /** Pin the change to one session instead of moving the global default. */
  sessionId: z.string().optional(),
});
export type PatchConfigBody = z.infer<typeof PatchConfigBody>;

/** PUT /config/keys — provider API keys, written to the launcher env file. */
export const PutKeysBody = z.record(z.string(), z.string());
export type PutKeysBody = z.infer<typeof PutKeysBody>;

/**
 * PUT /theme — a NAMED PARTIAL palette. `colors` is intentionally an open record
 * here: the semantic token set is owned by the theme module, which rejects
 * unknown tokens with a message naming them. A theme is pure data; no rebuild.
 */
export const PutThemeBody = z.object({
  name: z.string().trim().min(1).max(80),
  colors: z.record(z.string(), z.string()),
});
export type PutThemeBody = z.infer<typeof PutThemeBody>;

// ---- MCP (spec §10) ----------------------------------------------------------

/**
 * PUT /mcp/servers/:name — one registry entry, local (stdio subprocess) or remote
 * (Streamable HTTP). Kept as a union of two open shapes: the MCP module owns the
 * full validation, including `${VAR}` secret references it must not mangle.
 */
export const PutMcpServerBody = z.union([
  z.object({
    command: z.string().min(1),
    args: z.array(z.string()).optional(),
    env: z.record(z.string(), z.string()).optional(),
  }),
  z.object({
    url: z.string().url(),
    headers: z.record(z.string(), z.string()).optional(),
  }),
]);
export type PutMcpServerBody = z.infer<typeof PutMcpServerBody>;

/** POST /mcp/servers/:name/enable — grant the server to a session for this turn onward. */
export const McpActivationBody = z.object({
  sessionId: z.string(),
  /** e.g. "2h"; absent = until revoked. */
  ttl: z.string().optional(),
});
export type McpActivationBody = z.infer<typeof McpActivationBody>;

// ---- search (spec §17: keyword only) -----------------------------------------

/** GET /search?q=… — keyword (SQLite FTS) over transcripts. No embeddings. */
export const SearchQuery = z.object({
  q: z.string().min(1),
  sessionId: z.string().optional(),
  limit: z.number().int().positive().max(200).optional(),
});
export type SearchQuery = z.infer<typeof SearchQuery>;
