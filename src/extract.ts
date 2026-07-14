/**
 * Extract-to-conversation: copy hand-picked messages ("nodes") of a session's thread
 * into a fresh ROOT conversation — no summarizing, no parent link. Unlike compaction
 * and fork, which branch SIBLINGS and reconstruct the thread through parent-chain
 * math (so they're limited to the session's own messages), the extract is standalone:
 * any message in the visible thread — inherited ancestor turns included — is fair
 * game, and only the picked messages carry over, in thread order regardless of the
 * order they were selected in. A pick may carry `parts` (indexes into the message's
 * parts) to copy just those sections — e.g. a turn's prose without its tool calls.
 *
 * The new session keeps the source's workspace so work continues in the same repo
 * (as a root, its first turn branches jj off the current working copy, not the
 * source's tip) and records lineage (originId/originMessageId) for the map.
 *
 * Events: session.created for the new conversation and message.started per copy —
 * the UI's existing reducers render it with no changes (same contract as compact).
 */
import { z } from "zod";
import type { Session } from "./schema/parts.ts";
import {
  baseTitle,
  type BranchCtx,
  mergePicks,
  openBranch,
  PartPick,
  pickParts,
} from "./branch.ts";

export const ExtractBody = z.object({
  /** Messages of the session's thread (own or inherited) to copy, any subset. */
  picks: z.array(PartPick).min(1),
  /**
   * Take the source's place in the lineage: reuse its title and hang the new
   * session off the source's OWN origin instead of the source. For delete-range,
   * which archives the source right after — pointing at the archived session
   * would strand the replacement as a disconnected top-level root in the tree.
   */
  replaceSource: z.boolean().optional(),
});
export type ExtractBody = z.infer<typeof ExtractBody>;

/** 400 for an unknown message, 404 for an unknown session. */
export class ExtractError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "ExtractError";
  }
}

/**
 * Copy the selected thread messages of `sessionId` into a new root conversation and
 * return it. Throws ExtractError (400/404) on invalid input.
 */
export function extract(ctx: BranchCtx, sessionId: string, args: ExtractBody): Session {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new ExtractError(404, "session not found");

  const thread = ctx.db.threadFor(sessionId);
  const index = new Map(thread.map((m, i) => [m.id, i]));
  const picked = [...mergePicks(args.picks)]
    .map(([id, sel]) => {
      const i = index.get(id);
      if (i === undefined) {
        throw new ExtractError(400, "picks must be messages of this session's thread");
      }
      const parts = pickParts(thread[i], sel);
      if (parts === undefined) {
        throw new ExtractError(400, `part index out of range for message ${id}`);
      }
      return { idx: i, view: { ...thread[i], parts } };
    })
    .sort((a, b) => a.idx - b.idx);

  const seeder = openBranch(ctx, {
    parentId: null,
    title: args.replaceSource ? session.title : `extract · ${baseTitle(session.title)}`,
    kind: "root",
    workspace: session.workspace ?? null,
    // Lineage: normally the source session + the last picked node; a replacement
    // instead inherits the source's own origin link (see ExtractBody.replaceSource).
    originId: args.replaceSource ? session.originId ?? null : session.id,
    originMessageId: args.replaceSource
      ? session.originMessageId ?? null
      : thread[picked[picked.length - 1].idx].id,
  });
  for (const p of picked) seeder.copy(p.view);
  return seeder.session;
}
