/**
 * Extract — copy hand-picked messages of a session's VISIBLE thread into a fresh ROOT
 * conversation.
 *
 * THE INVARIANT THIS HOLDS: **extract is the one selection op that is not bounded by
 * the session's own messages.** Fork and compaction reconstruct a thread through
 * parent-chain math — they branch a SIBLING at `target.parentId` and let
 * `db.threadFor` re-supply the shared ancestors — so a pick reaching into ancestor
 * history is a 400 there, because those rows belong to a different session and the
 * branch cannot cut them out (`history/fork.ts`, `history/compact.ts`, spec §14).
 * Extract has no such math to satisfy: the new session is a ROOT with `parentId: null`,
 * inheriting nothing, and every message it will ever have is a copy this operation
 * writes. So the picks are resolved against `db.threadFor` — ancestors root→parent,
 * then own — and any message the user can SEE in the transcript is fair game. That is
 * the whole point of the operation, and it is the thing fork cannot do: "these four
 * turns, three of which came from the conversation this one was forked out of, are
 * their own piece of work now."
 *
 * SECOND: **only the picked messages carry over, in THREAD ORDER.** The client sends a
 * selection, not a sequence — a user shift-clicking upward would otherwise seed the new
 * root with its turns reversed — so `resolvePicks` restores order and merges duplicate
 * picks of the same message (`history/branch.ts`).
 *
 * A pick may carry `parts` indexes to copy just those sections of a message: a turn's
 * prose without its tool calls, which is the common case a fresh root wants (the
 * conclusion, not the 40KB of `rg` output that produced it). The server stays agnostic
 * about which part types those are — it copies the indexes it was given.
 *
 * WHAT THE NEW ROOT INHERITS. The source's workspace, verbatim: the extracted
 * conversation is about the same code and continues in the SAME checkout, edited in
 * place (spec §3.3, §14). With it come `originDir` (which project this is), `base` (the
 * sha the Changes rail measures from — without it a root extracted from a session with
 * work in the tree shows no changes at all, spec §13) and the model/effort pins.
 *
 * WHAT IT IS NOT: a move. The source keeps every one of its turns, untouched — no row
 * of it is updated, deleted or re-parented, which is why the AC asserts the source is
 * JSON-identical afterwards. Nothing here is destructive (spec §2.4).
 *
 * Lineage (`originId`/`originMessageId`) points back at the source and its last picked
 * message, so the tree can draw the edge even though the new session is a root and
 * inherits no thread through it.
 *
 * Ported from `src/extract.ts`. Deltas from that port are marked `NOTE:`.
 */
import { ExtractError } from "../errors.ts";
import type { Message, Session } from "../schema/parts.ts";
import { ExtractBody } from "../schema/requests.ts";
import type { AppCtx, Bus, Db } from "../types.ts";
import { json, parseBody } from "../server/http.ts";
import { baseTitle, openBranch, type PartPick, resolvePicks } from "./branch.ts";

/**
 * What extract needs from the world. Structurally satisfied by `AppCtx`, so a handler
 * passes the ctx it already has; declared narrowly so a test hands over a database and
 * a bus and nothing else (plan §0, DI over globals). No LLM: extract copies, it does
 * not summarize.
 */
export interface ExtractCtx {
  db: Db;
  bus: Bus;
  /** Injected clock, forwarded to the seeder. Absent = `Date.now`. */
  now?: () => number;
}

export interface ExtractResult {
  /** The new root, as storage kept it (pins included). */
  session: Session;
  /** Its messages, in the order they were seeded — thread order, not pick order. */
  messages: Message[];
}

/**
 * Copy the picked messages of `sessionId`'s thread into a new root conversation.
 *
 * Throws `ExtractError` — 404 for an unknown session, 400 for a pick that is not a
 * message of the visible thread or a part index out of range.
 *
 * Every validation runs BEFORE `openBranch`, and that ordering is load-bearing: the
 * seeder publishes `session.created` the moment it opens, so a check that ran afterwards
 * would leave an empty half-seeded root in the user's session list every time a client
 * sent a bad pick.
 */
export function extract(
  ctx: ExtractCtx,
  sessionId: string,
  args: ExtractBody,
): ExtractResult {
  const source = ctx.db.getSession(sessionId);
  if (!source) throw new ExtractError(404, `session ${sessionId} not found`);

  // THE VISIBLE thread — ancestors root→parent, then own. Not `messagesFor`: the whole
  // difference between this operation and fork is that an inherited turn is pickable.
  const thread = ctx.db.threadFor(sessionId);
  if (thread.length === 0) {
    throw new ExtractError(
      400,
      `session ${sessionId} has an empty thread — there is nothing to extract`,
    );
  }
  assertThreadMessages(ctx.db, source, args.picks, thread);
  const picked = resolvePicks(thread, args.picks, (m) => new ExtractError(400, m));

  const runtime = ctx.db.getSessionRuntime(sessionId);
  const seeder = openBranch(ctx, {
    // A ROOT. Not a sibling and not a child: the new conversation inherits no thread,
    // which is precisely what lets it carry an ancestor's turns without carrying the
    // ancestor (see the header).
    parentId: null,
    title: `extract · ${baseTitle(source.title)}`,
    kind: "root",
    // NOTE: `base` is not in the port, which snapshotted workspaces. Here the Changes
    // rail is `git diff <base>` (spec §13); a root sharing the source's checkout must
    // share the sha its change set is measured from.
    workspace: runtime.workspace,
    base: runtime.base,
    originDir: source.originDir ?? null,
    // Lineage for the tree: the session picked FROM, and the last picked message.
    // NOTE: the port also had a `replaceSource` mode that re-pointed this at the
    // source's OWN origin, for a delete-range flow that archived the source right
    // after. There is no archive, deprecate or purge in this system (spec §17), so the
    // mode has nothing to serve and `ExtractBody` (frozen) does not carry the flag.
    originId: source.id,
    originMessageId: thread[picked[picked.length - 1].idx].id,
  });

  const messages = picked.map((p) => seeder.copy(p.view));
  return { session: inheritPins(ctx, source, seeder.session), messages };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Reject a pick that is not a message of the visible thread, in terms of the move that
 * works.
 *
 * NOTE: the port answered "picks must be messages of the source thread" for every case.
 * The distinction that matters here is the mirror image of fork's: fork's interesting
 * rejection is a message the user CAN see (an ancestor's) which fork cannot use, and
 * extract accepts exactly those. What extract rejects is a message from somewhere else
 * in the tree entirely — a sibling fork, a subagent — and the move that works is to run
 * the extract from a session whose thread contains it.
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
    if (!foreign) throw new ExtractError(400, `no message ${p.messageId} exists`);
    throw new ExtractError(
      400,
      `message ${p.messageId} belongs to session ${foreign.sessionId}, which is not in ` +
        `${source.id}'s thread — extract can copy any message the session can SEE ` +
        `(its own turns and its ancestors'), so run the extract from ${foreign.sessionId} ` +
        `or from a session that inherits it`,
    );
  }
}

/**
 * Carry the source's per-session model/effort pins onto the new root.
 *
 * NOTE: not in the port, which had no per-session pins to carry. Exported because
 * handoff (`history/handoff.ts`) opens the same kind of derived root and inherits for
 * the same reason: a model id IS a provider routing decision (`llm/client.ts`), so a
 * session pinned to an OpenAI or OpenRouter model belongs to a user who may hold only
 * that provider's key. Falling back to the global default would answer the extracted
 * conversation on a different vendor at a different price, with nothing saying so —
 * and possibly with no key at all. Spec §12 pins per session; a session derived from a
 * pinned one is a continuation of it, not a fresh choice.
 *
 * Announced as `session.updated` rather than folded into the create, because
 * `openBranch` has already published `session.created`; a client reconciles by id and
 * ends up with the same row either way.
 */
export function inheritPins(
  ctx: { db: Db; bus: Bus },
  source: Session,
  branch: Session,
): Session {
  if (!source.model && !source.effort) return branch;
  if (source.model) ctx.db.setSessionModel(branch.id, source.model);
  if (source.effort) ctx.db.setSessionEffort(branch.id, source.effort);
  const stored = ctx.db.getSession(branch.id);
  if (!stored) return branch;
  ctx.bus.publish({ type: "session.updated", sessionId: stored.id, data: stored });
  return stored;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/**
 * `POST /sessions/:id/extract` — 201 with the new root and its thread.
 *
 * A `function` DECLARATION, not a `const` arrow, and that is load-bearing: this module
 * and `server/app.ts` form an import cycle (app.ts imports this handler for its route
 * table; this file imports app.ts's `json`/`parseBody`). Hoisted declarations exist from
 * module instantiation, so the route table can always read this binding; a `const` would
 * sit in its temporal dead zone whenever this module is evaluated first — which is
 * exactly what `extract.test.ts` does.
 *
 * 201 because an extract CREATES a session, the same as `POST /sessions`. The thread
 * rides along for the same reason `GET /sessions/:id` carries it: the client is about to
 * switch to this root, and a create answering with a bare session would force an
 * immediate second fetch to render anything at all. It is `threadFor` rather than the
 * seeded messages for symmetry with fork and compact — for a root the two are the same
 * list, and the client should not have to know which endpoint returns which.
 */
export async function extractH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  const body = await parseBody(req, ExtractBody);
  const { session } = extract(ctx, params.id, body);
  return json({ session, thread: ctx.db.threadFor(session.id) }, 201);
}
