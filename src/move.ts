/**
 * Move-to-branch: append copies of hand-picked messages from one session's thread
 * onto an EXISTING target session (extract's sibling — extract lands the picks in a
 * NEW root; move lands them at the end of a branch you already have). bough never
 * mutates history in place, so this is a copy: the source keeps its turns. The target
 * gains them as new messages (fresh ids), announced per copy like a seeded branch.
 */
import { z } from "zod";
import type { Session } from "./schema/parts.ts";
import { type BranchCtx, mergePicks, PartPick, pickParts } from "./branch.ts";

export const MoveBody = z.object({
  /** The session to copy FROM. */
  sourceId: z.string(),
  /** Messages of the source's thread to append to the target, any subset. */
  picks: z.array(PartPick).min(1),
});
export type MoveBody = z.infer<typeof MoveBody>;

export class MoveError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "MoveError";
  }
}

/** Append the picked source messages to `targetId`. Returns the target session. */
export function move(ctx: BranchCtx, targetId: string, args: MoveBody): Session {
  const target = ctx.db.getSession(targetId);
  if (!target) throw new MoveError(404, "target session not found");
  if (!ctx.db.getSession(args.sourceId)) throw new MoveError(404, "source session not found");
  if (args.sourceId === targetId) {
    throw new MoveError(400, "source and target are the same session");
  }

  const thread = ctx.db.threadFor(args.sourceId);
  const index = new Map(thread.map((m, i) => [m.id, i]));
  const picked = [...mergePicks(args.picks)]
    .map(([id, sel]) => {
      const i = index.get(id);
      if (i === undefined) throw new MoveError(400, "picks must be messages of the source thread");
      const parts = pickParts(thread[i], sel);
      if (parts === undefined) {
        throw new MoveError(400, `part index out of range for message ${id}`);
      }
      return { idx: i, view: { ...thread[i], parts } };
    })
    .sort((a, b) => a.idx - b.idx);

  // Append in thread order, each a fresh message on the target announced over the bus.
  for (const p of picked) {
    const msg = {
      id: crypto.randomUUID(),
      sessionId: targetId,
      role: p.view.role,
      parts: JSON.parse(JSON.stringify(p.view.parts)),
      pending: false,
      createdAt: Date.now(),
    };
    ctx.db.createMessage(msg);
    ctx.bus.publish({ type: "message.started", sessionId: targetId, data: msg });
  }
  return target;
}
