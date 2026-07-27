/**
 * Move-into — append copies of hand-picked messages from one session's thread onto an
 * EXISTING session. Extract's sibling: extract lands the picks in a new root, move-into
 * lands them at the end of a branch you already have.
 *
 * THE INVARIANT THIS HOLDS: **"move" is a lie the name tells and the implementation
 * never does. It is a COPY.** bough never rewrites history in place (spec §2.4, §14), so
 * nothing is deleted from the source, nothing is re-parented, and the source's rows come
 * out of this byte-identical — the AC asserts exactly that. The target gains new messages
 * with fresh ids, announced one by one like any seeded branch, and the user ends up with
 * the picks in both places. That is the honest reading of the operation, and the reason
 * it is safe to offer on a transcript someone is still working in.
 *
 * The picks are resolved against the SOURCE'S VISIBLE THREAD (`db.threadFor` — ancestors
 * root→parent, then own), the same reach extract has and for the same reason: this
 * operation writes copies and reconstructs nothing through parent-chain math, so an
 * inherited turn is as copyable as an own one. Part indexes narrow a message to some of
 * its sections (a turn's prose without its tool calls); order is restored to thread
 * order, because a client sends a selection, not a sequence (`history/branch.ts`).
 *
 * THREE THINGS IT REFUSES, all for the same reason — the copies land at the END of the
 * target's own messages, and an append is only sound when nothing else is deciding what
 * belongs there:
 *
 *   - **Into itself.** A session cannot append its own turns to its own tail; the result
 *     is a transcript that says everything twice and no operation the user asked for.
 *   - **Into a session running a turn** (409). One turn per session (spec §5): a live
 *     turn is appending to that same tail, and interleaving seeded copies into it
 *     produces a transcript whose order neither party chose — and which the turn will
 *     then replay to the model as though it had been there all along.
 *   - **Into an ANCESTOR of the source** (400). The target's messages come BEFORE the
 *     source's own in `threadFor`, so appending to an ancestor silently rewrites the
 *     middle of the source's visible thread. The source's rows would still be untouched
 *     and the invariant above still technically true, which is precisely what makes this
 *     the dangerous case rather than the obvious one.
 *
 * Ported from `src/move.ts`. Deltas from that port are marked `NOTE:`.
 */
import { MoveError } from "../errors.ts";
import type { Message, Session } from "../schema/parts.ts";
import { MoveBody } from "../schema/requests.ts";
import type { AppCtx, Bus, Db } from "../types.ts";
import { json, parseBody } from "../server/http.ts";
import { type PartPick, resolvePicks, Seeder } from "./branch.ts";

/**
 * What move-into needs from the world. Structurally satisfied by `AppCtx`, so a handler
 * passes the ctx it already has; declared narrowly so a test hands over a database and a
 * bus and nothing else (plan §0). No LLM: move-into copies, it does not summarize.
 */
export interface MoveCtx {
  db: Db;
  bus: Bus;
  /** Injected clock, forwarded to the seeder. Absent = `Date.now`. */
  now?: () => number;
}

export interface MoveResult {
  /** The target, unchanged as a row — only its message list grew. */
  session: Session;
  /** The copies appended, in the order they were seeded (thread order). */
  messages: Message[];
}

/**
 * Append copies of `args.sourceId`'s picked thread messages to `targetId`.
 *
 * Throws `MoveError` — 404 for an unknown target or source, 400 for a target that
 * cannot receive this source's picks or a pick outside the source's thread, 409 for a
 * target that is running a turn.
 *
 * Every check runs before the first `copy`, so a refused move writes nothing at all
 * rather than leaving half a selection appended with no way to finish it.
 */
export function move(ctx: MoveCtx, targetId: string, args: MoveBody): MoveResult {
  const target = ctx.db.getSession(targetId);
  if (!target) throw new MoveError(404, `target session ${targetId} not found`);
  const source = ctx.db.getSession(args.sourceId);
  if (!source) throw new MoveError(404, `source session ${args.sourceId} not found`);

  if (args.sourceId === targetId) {
    throw new MoveError(
      400,
      `source and target are both ${targetId} — move-into COPIES the picks onto the ` +
        `end of the target, so this would append the session's own turns to itself. ` +
        `Pick a different target, or extract the selection into a new root instead.`,
    );
  }
  // The ancestor case: `threadFor(source)` is ancestors first, so an append here lands
  // in the MIDDLE of what the source displays and replays (see the header).
  if (ctx.db.ancestorChain(args.sourceId).some((s) => s.id === targetId)) {
    throw new MoveError(
      400,
      `session ${targetId} is an ancestor of ${args.sourceId}: ${args.sourceId} inherits ` +
        `its messages, so appending there would insert turns into the middle of the ` +
        `source's own thread. Extract the selection into a new root instead.`,
    );
  }
  // NOTE: not in the port, which had no turn state to consult. One turn per session
  // (spec §5) — a running turn owns the tail this appends to.
  if (ctx.db.busySessionIds().has(targetId)) {
    throw new MoveError(
      409,
      `session ${targetId} is running a turn — move-into appends to the end of its ` +
        `transcript and would interleave with what the turn is writing there. Wait for ` +
        `the turn to finish (or interrupt it) and send this again.`,
    );
  }

  // THE SOURCE'S VISIBLE thread, ancestors included — same reach as extract.
  const thread = ctx.db.threadFor(args.sourceId);
  if (thread.length === 0) {
    throw new MoveError(
      400,
      `session ${args.sourceId} has an empty thread — there is nothing to copy`,
    );
  }
  assertThreadMessages(ctx.db, source, args.picks, thread);
  const picked = resolvePicks(thread, args.picks, (m) => new MoveError(400, m));

  // Appended in thread order, each a fresh message announced over the bus — the same
  // `Seeder` a branch uses, constructed directly because the session already exists
  // (`history/branch.ts`). Timestamps come from the real clock, never an advanced one,
  // so a turn started immediately afterwards still sorts after these (plan §6.1).
  const seeder = new Seeder(ctx, target);
  const messages = picked.map((p) => seeder.copy(p.view));
  return { session: target, messages };
}

/**
 * Reject a pick that is not a message of the source's visible thread.
 *
 * NOTE: the port answered "picks must be messages of the source thread" for every case.
 * Naming where the message actually lives is the difference between an error the user
 * can act on and one that reads as a client bug — the id is real and they are looking
 * at it somewhere.
 */
function assertThreadMessages(
  db: Db,
  source: Session,
  picks: readonly PartPick[],
  thread: readonly Message[],
): void {
  const ids = new Set(thread.map((m) => m.id));
  for (const p of picks) {
    if (ids.has(p.messageId)) continue;
    const foreign = db.getMessage(p.messageId);
    if (!foreign) throw new MoveError(400, `no message ${p.messageId} exists`);
    throw new MoveError(
      400,
      `message ${p.messageId} belongs to session ${foreign.sessionId}, which is not in ` +
        `source ${source.id}'s thread — pass ${foreign.sessionId} as sourceId, or pick ` +
        `messages the source can see (its own turns and its ancestors')`,
    );
  }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/**
 * `POST /sessions/:id/move-into` — 200 with the target and its thread.
 *
 * A `function` DECLARATION for the same cycle reason as `extract.ts`'s handler: this
 * module imports `json`/`parseBody` from `server/app.ts`, which imports this handler for
 * its route table. A hoisted declaration is readable from the moment the module is
 * instantiated; a `const` would be in its temporal dead zone whenever this file is
 * evaluated first.
 *
 * **200, not 201** — unlike every other history operation, this one creates no session.
 * The `:id` in the path is the TARGET (the session being appended to), matching the rest
 * of `/sessions/:id/*`; the source travels in the body, because it is the argument and
 * not the thing being acted on. The thread rides along so the client can re-render the
 * target it is looking at without a second fetch.
 */
export async function moveIntoH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  const body = await parseBody(req, MoveBody);
  const { session, messages } = move(ctx, params.id, body);
  return json({
    session,
    thread: ctx.db.threadFor(session.id),
    // How many copies landed. The client selected N picks but sent a SELECTION —
    // duplicate picks of one message merge (`history/branch.ts`) — so the count it
    // would otherwise assume can differ from the count that was written.
    appended: messages.length,
  });
}
