/**
 * Fork-at-message — the backend for the UI's "edit any past turn to fork" affordance.
 * Cut a session's thread at one of its turns and branch a new sibling session from
 * there, optionally replacing the turn's user message with an edited one and running a
 * fresh turn ("edit & resend"). Reuses the sibling-branch seeding from branch.ts (same
 * mechanism as compaction) and the shared user-turn path from turn.ts.
 *
 * Semantics:
 *   - `atMessageId` must be one of the target session's OWN messages (same constraint as
 *     compaction — an ancestor turn is 400: fork the ancestor session instead).
 *   - The fork is a SIBLING (parent = target.parentId) and inherits the target's
 *     workspace, so #9's turn machinery forks the jj change off the shared parent's tip
 *     on the fork's first turn (kind=fork).
 *   - Seed the fork with copies of the target's own messages STRICTLY BEFORE atMessageId.
 *   - With `editedText`: append a new user message carrying it and run a real turn from
 *     there (streams message.started/delta/part/finished over the bus). `editedText` may
 *     only replace a user message (editing a supervisor turn is 400).
 *   - Without `editedText`: also copy atMessageId itself — a plain branch point left
 *     ready for new input, no turn run.
 *
 * Note: the fork carries the *conversation* prefix; the jj file base is the shared
 * parent's tip (#9), so file edits made by the copied prefix turns are not replayed —
 * v1 forks history, and the workspace re-derives from the parent snapshot.
 */
import { z } from "zod";
import type { Message, Session } from "./schema/parts.ts";
import { openBranch } from "./branch.ts";
import { startUserTurn, type TurnCtx } from "./turn.ts";

export const ForkBody = z.object({
  atMessageId: z.string(),
  editedText: z.string().optional(),
});
export type ForkBody = z.infer<typeof ForkBody>;

/** 400 for a bad fork point, 404 for an unknown session. */
export class ForkError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "ForkError";
  }
}

export interface ForkResult {
  session: Session;
  /** Present when editedText ran a turn; resolves when it finishes (tests await it). */
  done?: Promise<void>;
}

/** Fork `sessionId` at `atMessageId`. Throws ForkError (400/404) on invalid input. */
export function fork(ctx: TurnCtx, sessionId: string, body: ForkBody): ForkResult {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new ForkError(404, "session not found");

  const own = ctx.db.messagesFor(sessionId);
  const atIdx = own.findIndex((m: Message) => m.id === body.atMessageId);
  if (atIdx < 0) {
    throw new ForkError(
      400,
      "atMessageId must be a message of this session (fork the ancestor session instead)",
    );
  }
  const edited = body.editedText !== undefined;
  if (edited && own[atIdx].role !== "user") {
    throw new ForkError(400, "editedText can only replace a user message");
  }

  const seeder = openBranch(ctx, {
    parentId: session.parentId,
    title: `fork · ${session.title}`,
    kind: "fork",
    workspace: session.workspace ?? null,
    originId: session.id, // lineage: the forked-from session…
    originMessageId: body.atMessageId, // …and the at-message
  });

  // Copy the prefix strictly before the fork point.
  for (const m of own.slice(0, atIdx)) seeder.copy(m);

  if (edited) {
    // Edit & resend: the new user message + a real turn, via the shared path.
    const { done } = startUserTurn(ctx, seeder.session.id, body.editedText!);
    return { session: seeder.session, done };
  }
  // Plain branch point: include the fork-point message, ready for new input.
  seeder.copy(own[atIdx]);
  return { session: seeder.session };
}
