/**
 * The take-back: dropping a message the user retracts seconds after sending it,
 * IN PLACE, on the conversation they sent it in.
 *
 * WHY THIS IS NOT A FORK. Every other operation in this directory branches, and
 * that is right for every one of them: an edit to a turn from ten minutes ago is a
 * second attempt at a piece of work, and the first attempt is a thing the user may
 * want to read again. Escape within the take-back window is not that. It is the
 * user saying the message never should have left — a typo, the wrong conversation,
 * a half-finished sentence — and answering it with a branch produced a sibling
 * session, a `⑂` in the tree, and a conversation the user has to learn to ignore,
 * for a message that existed for three seconds. The gesture reads as "undo"; a
 * branch is not undo, it is a fork of a mistake.
 *
 * SO THE RULES ARE NARROW, and they are what makes deleting rows defensible in a
 * system whose whole premise is that history is a tree (spec §2.4, §14):
 *
 *   - Only the session's OWN messages. Ancestor history belongs to another
 *     session's rows, exactly as for a fork, and the answer is the same 400.
 *   - Only a USER message. The model's turns are not the user's to retract, and a
 *     supervisor message deleted out from under a running turn is a different
 *     feature with different hazards.
 *   - Only the LAST user message. Anything earlier is settled history with answers
 *     built on top of it, and reaching back into it is what fork is for. This is
 *     the rule that keeps "the take-back" from quietly becoming "delete anything".
 *
 * WHAT GOES WITH IT: the message and everything AFTER it, which in practice is the
 * partial answer the retracted message provoked. Keeping that would leave a reply
 * to a question nobody can see — worse than either alternative.
 *
 * THE RUNNING TURN IS STOPPED FIRST, here rather than in the client. Nobody takes a
 * message back and still wants to pay for the answer, and doing both halves in one
 * route is also what removes the race: two calls from a client can interleave with
 * the runner's own writes, one route cannot. The abort does not block (see
 * `server/turns.ts`), and it does not need to — the runner's late writes are
 * UPDATEs against rows that are gone, which SQLite answers by changing nothing,
 * and its late events name a message no client still holds.
 */
import { BadRequestError, NotFoundError } from "../errors.ts";
import type { Message, Session } from "../schema/parts.ts";
import { UnsendBody } from "../schema/requests.ts";
import { interruptTurn } from "../turn/runner.ts";
import { turns as globalTurns, type TurnRegistry } from "../turn/queue.ts";
import type { AppCtx } from "../types.ts";
import { type Handler, json, parseBody } from "../server/http.ts";

/** What the client gets back: enough to put the text in the composer and say so. */
export interface UnsendResult {
  sessionId: string;
  /** The retracted message's text, for the composer it is going back into. */
  text: string;
  /** Every message id removed — the retracted one, then whatever followed it. */
  removed: string[];
  /** True when a turn was running and has been signalled to stop. */
  interrupted: boolean;
}

/** The plain text of a user message, which is all a composer can hold. */
function textOf(message: Message): string {
  return message.parts
    .filter((p): p is Extract<typeof p, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("")
    .trim();
}

function registryOf(ctx: AppCtx): TurnRegistry {
  return (ctx as AppCtx & { turnRegistry?: TurnRegistry }).turnRegistry ?? globalTurns;
}

function requireSession(ctx: AppCtx, id: string): Session {
  const session = ctx.db.getSession(id);
  if (!session) throw new NotFoundError(`no session ${id}`);
  return session;
}

/**
 * `POST /sessions/:id/unsend` — retract `atMessageId` and everything after it.
 *
 * Every refusal names the operation that DOES work, because the caller reaching
 * this route with the wrong message is a client one release out of step, and "400"
 * on its own leaves the user with a key that silently does nothing.
 */
export const unsendMessageH: Handler = async (req, ctx, params) => {
  const session = requireSession(ctx, params.id);
  const body = await parseBody(req, UnsendBody);

  const own = ctx.db.messagesFor(session.id);
  const target = own.find((m) => m.id === body.atMessageId);
  if (!target) {
    // Either it never existed, or it is an ancestor's — one sentence covers both,
    // because from here they are the same fact: this session does not own that row.
    throw new BadRequestError(
      `message ${body.atMessageId} is not one of this session's own messages, so it ` +
        `cannot be taken back here — fork the session that owns it instead`,
    );
  }
  if (target.role !== "user") {
    throw new BadRequestError(
      `only a user message can be taken back; ${body.atMessageId} is a ${target.role} ` +
        `message — fork at it to branch away from what the model said`,
    );
  }
  const lastUser = [...own].reverse().find((m) => m.role === "user");
  if (lastUser?.id !== target.id) {
    throw new BadRequestError(
      `${body.atMessageId} is not the most recent thing you said, and taking it back ` +
        `would drop the turns built on top of it — fork at it to say it differently ` +
        `and keep this conversation intact`,
    );
  }

  // Stop first, delete second: a turn signalled after its message is gone would
  // spend the round it is in the middle of for an answer to a retracted question.
  const interrupted = interruptTurn(session.id, registryOf(ctx));
  const removed = ctx.db.deleteMessagesFrom(session.id, target.id);

  const result: UnsendResult = {
    sessionId: session.id,
    text: textOf(target),
    removed,
    interrupted,
  };
  return json(result);
};
