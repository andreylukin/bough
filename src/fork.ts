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
 *     workspace, so #9's turn machinery forks the shadow worktree off the shared parent's tip
 *     on the fork's first turn (kind=fork).
 *   - Seed the fork with copies of the target's own messages STRICTLY BEFORE atMessageId.
 *   - With `editedText`: append a new user message carrying it and run a real turn from
 *     there (streams message.started/delta/part/finished over the bus). `editedText` may
 *     only replace a user message (editing a supervisor turn is 400).
 *   - Without `editedText`: also copy atMessageId itself — a plain branch point left
 *     ready for new input, no turn run. With `exclusive`: skip that copy — the branch
 *     ends strictly before atMessageId (the UI's "rewind to edit this message" cut).
 *   - With `atPart`: cut INSIDE the at-message — copy it truncated to parts[0..atPart]
 *     (e.g. history up to a failed tool result) as the branch's last seeded message.
 *     Here `editedText` is a NEW user message appended after the cut (any at-message
 *     role), the "don't try it that way" move; without it, a plain branch point.
 *     Replay already tolerates a cut that strands a tool_call: turn.ts synthesizes an
 *     "(interrupted)" tool_result for any call left without one.
 *
 * Note: the fork carries the *conversation* prefix; the snapshot file base is the shared
 * parent's tip (#9), so file edits made by the copied prefix turns are not replayed —
 * v1 forks history, and the workspace re-derives from the parent snapshot.
 */
import { HttpError } from "./errors.ts";
import { z } from "zod";
import type { Message, Session } from "./schema/parts.ts";
import { baseTitle, openBranch } from "./branch.ts";
import { startUserTurn, type TurnCtx } from "./turn.ts";

export const ForkBody = z.object({
  atMessageId: z.string(),
  /** Cut inside the at-message: keep parts[0..atPart] of it (see module doc). */
  atPart: z.number().int().nonnegative().optional(),
  editedText: z.string().optional(),
  /** Cut BEFORE the at-message: don't copy it into the branch — the caller intends
   * to re-send it (possibly edited) itself. Only for the plain-branch-point case
   * (no editedText, no atPart), where the at-message would otherwise be copied. */
  exclusive: z.boolean().optional(),
});
export type ForkBody = z.infer<typeof ForkBody>;

/** 400 for a bad fork point, 404 for an unknown session. */
export class ForkError extends HttpError {}

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
  // Without atPart, editedText REPLACES the at-message, which only makes sense for a
  // user turn. With atPart it's a fresh user message after the cut — any role works.
  if (edited && body.atPart === undefined && own[atIdx].role !== "user") {
    throw new ForkError(400, "editedText can only replace a user message");
  }
  if (body.atPart !== undefined && body.atPart >= own[atIdx].parts.length) {
    throw new ForkError(400, "atPart out of range for the at-message");
  }

  // Title the fork after its branch point so several forks of one session stay
  // tellable-apart in the pickers; fall back to the source's base title (a fork
  // of a fork must not compound into "fork · fork · X").
  const atText = own[atIdx].parts.find((p) => p.type === "text");
  const excerpt = atText && "text" in atText ? atText.text.split("\n")[0].trim().slice(0, 48) : "";
  const seeder = openBranch(ctx, {
    parentId: session.parentId,
    title: `fork · ${excerpt || baseTitle(session.title)}`,
    kind: "fork",
    workspace: session.workspace ?? null,
    originDir: session.originDir ?? null,
    originId: session.id, // lineage: the forked-from session…
    originMessageId: body.atMessageId, // …and the at-message
  });

  // Copy the prefix strictly before the fork point.
  for (const m of own.slice(0, atIdx)) seeder.copy(m);

  if (body.atPart !== undefined) {
    // Mid-message cut: the at-message survives truncated to the cut point — history
    // up to (say) a failed tool result — then the user's correction, if any, runs.
    const at = own[atIdx];
    seeder.copy({ ...at, parts: at.parts.slice(0, body.atPart + 1) });
    if (edited) {
      const { done } = startUserTurn(ctx, seeder.session.id, body.editedText!);
      return { session: seeder.session, done };
    }
    return { session: seeder.session };
  }

  if (edited) {
    // Edit & resend: the new user message + a real turn, via the shared path.
    const { done } = startUserTurn(ctx, seeder.session.id, body.editedText!);
    return { session: seeder.session, done };
  }
  // Plain branch point: include the fork-point message, ready for new input —
  // unless the caller asked for an exclusive cut to re-send it itself.
  if (!body.exclusive) seeder.copy(own[atIdx]);
  return { session: seeder.session };
}
