/**
 * Handoff — focused threads instead of endless compaction (the Amp pattern). The
 * user states a GOAL; an LLM reads the source session's whole thread and drafts the
 * opening prompt for a fresh conversation: the goal restated, only the context that
 * matters for it (decisions, constraints, current state), and the relevant file
 * paths. Nothing is summarized in place and no messages are copied — the distilled
 * context lives entirely in the draft.
 *
 * The new session is a fresh ROOT on the same workspace (like extract), with
 * lineage (originId/originMessageId) back to the source for the map. The draft is
 * persisted on the session (Session.draft): the UI prefills the composer with it,
 * the user edits and sends, and posting the message clears it (app.ts). The source
 * session is never mutated.
 */
import { HttpError } from "./errors.ts";
import { z } from "zod";
import type { Session } from "./schema/parts.ts";
import { anthropicClient, completeText, type LlmClient } from "./supervisor/llm.ts";
import { renderSpan } from "./compact.ts";
import { baseTitle, type BranchCtx, openBranch } from "./branch.ts";

export const HandoffBody = z.object({
  /** What the new conversation is for — drives what the draft keeps and drops. */
  goal: z.string().min(1),
});
export type HandoffBody = z.infer<typeof HandoffBody>;

export interface HandoffCtx extends BranchCtx {
  /** Injected for tests; defaults to the real Anthropic client. */
  llm?: LlmClient;
  model?: string;
}

/** 400 for an empty thread, 404 for an unknown session. */
export class HandoffError extends HttpError {}

const SYSTEM =
  "You are handing off work from one coding-agent conversation to a new, focused one. " +
  "Given the transcript and the user's goal for the new conversation, write the OPENING " +
  "PROMPT the user will send to start it. The new agent sees nothing but this prompt, so " +
  "make it self-contained: state the goal as the task; carry over only the context that " +
  "matters for it — decisions made, constraints, the current state of the work; list the " +
  "relevant file paths. Drop everything unrelated to the goal, including dead ends and " +
  "resolved back-and-forth. Write as direct instructions to the agent, in the user's " +
  "voice. Output only the prompt text.";

const MAX_TOKENS = 2048;

/**
 * Draft the handoff prompt for `sessionId` toward `goal`, open the new root session
 * with the draft attached, and return it. Throws HandoffError (400/404).
 */
export async function handoff(
  ctx: HandoffCtx,
  sessionId: string,
  args: HandoffBody,
): Promise<Session> {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new HandoffError(404, "session not found");
  const thread = ctx.db.threadFor(sessionId);
  if (thread.length === 0) {
    throw new HandoffError(400, "nothing to hand off: the thread is empty");
  }

  const llm = ctx.llm ?? anthropicClient();
  const model = ctx.model ?? Deno.env.get("BOUGH_MODEL") ?? "claude-opus-4-8";
  const prompt = `${renderSpan(thread)}\n\nGoal for the new conversation: ${args.goal}`;
  const draft = (await completeText(llm, { model, system: SYSTEM, maxTokens: MAX_TOKENS, prompt }))
    .trim();
  if (!draft) throw new HandoffError(502, "the model returned an empty draft");

  // A fresh root on the same workspace, lineage back to the source (same shape as
  // extract). No messages are seeded — the draft carries all the context.
  const seeder = openBranch(ctx, {
    parentId: null,
    title: `handoff · ${baseTitle(session.title)}`,
    kind: "root",
    workspace: session.workspace ?? null,
    originId: session.id,
    originMessageId: thread[thread.length - 1].id,
  });
  ctx.db.setSessionDraft(seeder.session.id, draft);
  const created = { ...seeder.session, draft };
  // openBranch already announced session.created (pre-draft); announce the draft so
  // live tree views carry it without a refetch.
  ctx.bus.publish({ type: "session.updated", sessionId: created.id, data: created });
  return created;
}
