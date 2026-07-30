/**
 * Handoff — a focused new thread instead of another round of compaction.
 *
 * The user states a GOAL. An LLM reads the source session's whole visible thread and
 * writes the OPENING PROMPT for a fresh conversation: the goal restated as the task,
 * only the context that still matters for it (decisions made, constraints, the state
 * the work is in), and the relevant file paths. Everything unrelated — the dead ends,
 * the resolved back-and-forth, the 40KB of tool output that produced one sentence —
 * is dropped, because the new agent will see nothing but this prompt.
 *
 * THE INVARIANT THIS HOLDS: **nothing is copied and nothing is mutated.** Compaction
 * seeds a branch with copies and one summary in place of a span; a handoff seeds NO
 * messages at all. The distilled context lives entirely in the new session's `draft`
 * (spec §4, §14), and the source session is not written to in any way — the AC asserts
 * it is JSON-identical afterwards. That is what makes handoff safe to run speculatively
 * against a conversation the user is still in the middle of: worst case they get a root
 * with a draft they discard.
 *
 * WHY A DRAFT RATHER THAN A SEEDED USER MESSAGE. The draft is *prefilled composer text*,
 * not a turn: the UI puts it in the composer, the user reads what the model decided to
 * carry over, edits it, and sends. A seeded user message would start the work on the
 * model's own account of what mattered, with no moment for the human to correct it —
 * and the whole reason handoff exists is that the human knows what the next thread is
 * for. Posting the message is what clears the draft, server-side, on the first post
 * (`server/sessions.ts`): the thing the user actually sent supersedes it, edited or not.
 *
 * THE NEW SESSION IS A ROOT (`parentId: null`) on the source's workspace — the same
 * checkout, worked in place (spec §3.3) — with lineage back to the source so the tree
 * can draw the edge. It inherits no thread, which is the point: a handoff whose new
 * session still replayed the old conversation would have distilled nothing.
 *
 * ORDER OF OPERATIONS: draft FIRST, session second. The LLM call completes before a
 * single row is written, so a failed or empty draft leaves no empty root behind for the
 * user to find and clean up — the same rule compaction follows for the same reason.
 *
 * Ported from `src/handoff.ts`. Deltas from that port are marked `NOTE:`.
 */
import { HandoffError } from "../errors.ts";
import { clientFor, completeText } from "../llm/client.ts";
import type { Session } from "../schema/parts.ts";
import { HandoffBody } from "../schema/requests.ts";
import { DEFAULT_MODEL } from "../turn/runner.ts";
import type { AppCtx, Bus, Db, LlmClient } from "../types.ts";
import { json, parseBody } from "../server/http.ts";
import { baseTitle, openBranch } from "./branch.ts";
import { renderSpan } from "./compact.ts";
import { inheritPins } from "./extract.ts";

/** A goal, shortened to something that fits a tree row. Word-boundary, not mid-word. */
function clipTitle(goal: string, max = 48): string {
  const one = goal.replace(/\s+/g, " ").trim();
  if (one.length <= max) return one;
  const cut = one.slice(0, max);
  const space = cut.lastIndexOf(" ");
  return `${(space > max / 2 ? cut.slice(0, space) : cut).trimEnd()}…`;
}

/**
 * What handoff needs from the world. Structurally satisfied by `AppCtx`, so a handler
 * passes the ctx it already has; declared narrowly so a test hands over a database, a
 * bus and a scripted `LlmClient` and nothing else (plan §0, DI over globals).
 */
export interface HandoffCtx {
  db: Db;
  bus: Bus;
  /** Injected in tests. Absent = the provider-routed client for the resolved model. */
  llm?: LlmClient;
  /** The global model default; a session's own pin wins over it. */
  model?: string;
  /** Injected clock, forwarded to the seeder. Absent = `Date.now`. */
  now?: () => number;
}

const SYSTEM =
  "You are handing off work from one coding-agent conversation to a new, focused one. " +
  "Given the transcript and the user's goal for the new conversation, write the OPENING " +
  "PROMPT the user will send to start it. The new agent sees nothing but this prompt, so " +
  "make it self-contained: state the goal as the task; carry over only the context that " +
  "matters for it — decisions made, constraints, the current state of the work; list the " +
  "relevant file paths. Drop everything unrelated to the goal, including dead ends and " +
  "resolved back-and-forth. Write as direct instructions to the agent, in the user's " +
  "voice. Output only the prompt text.\n\n" +
  // WITHOUT THIS, THE DRAFT ASKS THE USER A QUESTION. Observed: a short transcript plus
  // a goal it had no context for produced "Once you provide that, I can write a focused
  // opening prompt for the new conversation." — which lands verbatim in the user's
  // composer as if it were the distilled prompt, addressed to nobody. The transcript
  // being thin is not a reason to stop working; it is a reason for a shorter prompt.
  "NEVER reply to the user and never ask for more information: you are writing text " +
  "the user will SEND, not a message to them. Do not describe what you are doing, do " +
  "not offer alternatives, and do not preface the prompt. If the transcript holds " +
  "little or nothing relevant to the goal, say so in one line and then state the goal " +
  "as the task — a short prompt is a correct answer, a request for input is not. State " +
  "it as an INSTRUCTION (\"fix the coupon stacking in src/cart.py\"), never as a request " +
  "for details: whoever reads this prompt is the one who will do the work.";

const MAX_TOKENS = 2048;

/**
 * The model that drafts.
 *
 * NOTE: the port read `ctx.model ?? BOUGH_MODEL ?? <default>`, ignoring the session's
 * own pin. Resolved here exactly as the turn runner and compaction resolve it — session
 * pin, then the global default, then `DEFAULT_MODEL` — because a model id IS a provider
 * routing decision (`llm/client.ts`): a session pinned to an OpenAI or OpenRouter model
 * belongs to a user who may hold only that provider's key, and drafting on the Anthropic
 * default would fail the handoff with an auth error on a conversation that runs fine.
 */
function modelFor(ctx: HandoffCtx, session: Session): string {
  return session.model ?? ctx.model ?? DEFAULT_MODEL;
}

/**
 * Draft the handoff prompt for `sessionId` toward `args.goal`, open the new root with
 * the draft attached, and return it.
 *
 * Throws `HandoffError` — 404 for an unknown session, 400 for a thread with nothing in
 * it to hand off, 502 for a model that returned no text.
 */
export async function handoff(
  ctx: HandoffCtx,
  sessionId: string,
  args: HandoffBody,
): Promise<Session> {
  const source = ctx.db.getSession(sessionId);
  if (!source) throw new HandoffError(404, `session ${sessionId} not found`);

  // The VISIBLE thread — ancestors root→parent, then own. A handoff distills what the
  // user has been looking at, which in a forked session is mostly inherited.
  const thread = ctx.db.threadFor(sessionId);
  if (thread.length === 0) {
    throw new HandoffError(
      400,
      `session ${sessionId} has an empty thread — there is nothing to hand off. Start a ` +
        `new session directly instead.`,
    );
  }

  const model = modelFor(ctx, source);
  const llm = ctx.llm ?? clientFor(model);
  // `renderSpan` is compaction's transcript renderer, reused rather than reimplemented:
  // a second renderer would drift the moment a part kind is added, and it already clips
  // oversized tool payloads so one 200KB result cannot swallow the prompt.
  const prompt = `${renderSpan(thread)}\n\nGoal for the new conversation: ${args.goal}`;
  const draft = (await completeText(llm, {
    model,
    system: SYSTEM,
    maxTokens: MAX_TOKENS,
    prompt,
  })).trim();
  // An empty draft is not a handoff. The whole content of this operation is the draft —
  // seeding a root without one would hand the user an empty composer and a session they
  // did not ask for. Raised before anything is written, so that session never exists.
  if (!draft) {
    throw new HandoffError(
      502,
      `the model (${model}) returned no draft for a thread of ${thread.length} message(s) ` +
        `— nothing was written; retry, or state the goal more concretely`,
    );
  }

  const runtime = ctx.db.getSessionRuntime(sessionId);
  const seeder = openBranch(ctx, {
    parentId: null, // a ROOT: it inherits no thread, only the draft
    // The GOAL when the source has no title of its own. A conversation whose auto-title
    // never landed produced `handoff · ` — a prefix with nothing after it, which every
    // client then renders as "(untitled)" for the rest of its life, because the row is
    // only ever retitled by a first message and this session's first message is sitting
    // unsent in the composer. The goal is the one thing the user definitely wrote.
    title: `handoff · ${baseTitle(source.title) || clipTitle(args.goal)}`,
    kind: "root",
    // The same checkout, worked in place — with the sha its change set is measured from
    // (spec §13). NOTE: `base` is not in the port, which snapshotted workspaces.
    workspace: runtime.workspace,
    base: runtime.base,
    originDir: source.originDir ?? null,
    originId: source.id, // lineage: handed off FROM…
    originMessageId: thread[thread.length - 1].id, // …as of this message
  });
  const branch = inheritPins(ctx, source, seeder.session);

  ctx.db.setSessionDraft(branch.id, draft);
  // Read back rather than spread onto the local object: the row is what a later
  // `GET /sessions/:id` will answer with, and the event and the fetch must not be able
  // to disagree. `openBranch` already published `session.created` (pre-draft), so this
  // announces the draft for live tree views that would otherwise need a refetch.
  const created = ctx.db.getSession(branch.id) ?? { ...branch, draft };
  ctx.bus.publish({ type: "session.updated", sessionId: created.id, data: created });
  return created;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/**
 * `POST /sessions/:id/handoff` — 201 with the new root, draft attached.
 *
 * A `function` DECLARATION for the same cycle reason as the other history handlers:
 * this module imports `json`/`parseBody` from `server/app.ts`, which imports this
 * handler for its route table, and a hoisted declaration is readable from module
 * instantiation while a `const` would sit in its temporal dead zone whenever this file
 * is evaluated first (which is what `handoff.test.ts` does).
 *
 * No `thread` in the response, unlike fork/compact/extract: a handoff seeds no messages
 * and the new root inherits none, so the thread is empty by construction and sending it
 * would only suggest there is something to look at. The `draft` on the session is the
 * entire payload the client needs.
 */
export async function handoffH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  const body = await parseBody(req, HandoffBody);
  return json({ session: await handoff(ctx, params.id, body) }, 201);
}
