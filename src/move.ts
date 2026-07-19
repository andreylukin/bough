/**
 * Move-to-branch: append copies of hand-picked messages from one session's thread
 * onto an EXISTING target session (extract's sibling — extract lands the picks in a
 * NEW root; move lands them at the end of a branch you already have). bough never
 * mutates history in place, so this is a copy: the source keeps its turns. The target
 * gains them as new messages (fresh ids), announced per copy like a seeded branch.
 */
import { HttpError } from "./errors.ts";
import { z } from "zod";
import type { Session } from "./schema/parts.ts";
import { type BranchCtx, PartPick, resolvePicks, Seeder } from "./branch.ts";

export const MoveBody = z.object({
  /** The session to copy FROM. */
  sourceId: z.string(),
  /** Messages of the source's thread to append to the target, any subset. */
  picks: z.array(PartPick).min(1),
});
export type MoveBody = z.infer<typeof MoveBody>;

export class MoveError extends HttpError {}

/** Append the picked source messages to `targetId`. Returns the target session. */
export function move(ctx: BranchCtx, targetId: string, args: MoveBody): Session {
  const target = ctx.db.getSession(targetId);
  if (!target) throw new MoveError(404, "target session not found");
  if (!ctx.db.getSession(args.sourceId)) throw new MoveError(404, "source session not found");
  if (args.sourceId === targetId) {
    throw new MoveError(400, "source and target are the same session");
  }

  const thread = ctx.db.threadFor(args.sourceId);
  const picked = resolvePicks(thread, args.picks, (m) => new MoveError(400, m));

  // Append in thread order, each a fresh message on the target announced over the bus.
  const seeder = new Seeder(ctx, target);
  for (const p of picked) seeder.copy(p.view);
  return target;
}
