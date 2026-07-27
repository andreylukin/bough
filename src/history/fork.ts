/**
 * Fork-at-message, and "edit & resend" — the backend for the UI's "edit any past turn
 * to branch from it" affordance (spec §14).
 *
 * THE INVARIANT THIS HOLDS: **the source session is byte-identical afterwards.**
 * History is a tree and nothing is ever destructively rewritten (spec §2.4), so a
 * fork is only ever a sequence of WRITES TO THE BRANCH: it reads the target's own
 * messages, copies them into a session that did not exist a moment ago, and never
 * updates, deletes or re-parents a single row of the thing it forked. "Edit &
 * resend" is the case that makes this worth stating in capitals — the user's mental
 * model there is "change what I said and try again", which reads as a mutation and
 * is implemented as one nowhere: the edited text is a NEW message on a NEW session,
 * and the original turn, its answer, and everything after it are still exactly where
 * they were. That is what makes the affordance safe to offer on any turn, however
 * far back.
 *
 * The second thing this file encodes is **why a fork is a SIBLING rather than a
 * child.** The branch is parented at `target.parentId`, not at the target, so
 * `db.threadFor` hands it every shared ancestor for free and only the target's OWN
 * turns are ever copied (`history/branch.ts`, spec §14). Parenting it at the target
 * would inherit the very messages the fork exists to cut away.
 *
 * That is also the whole reason a fork point outside the session's own messages is a
 * **400 and not a deeper walk**: the branch cannot cut into inherited history,
 * because the ancestor is a different session's rows and the fork does not own them.
 * The error says so and names the move that works — fork the ancestor instead.
 *
 * WHAT THE BRANCH INHERITS. The workspace, verbatim: a fork is a second conversation
 * about the SAME checkout, worked in place (spec §3.3) — there is no per-branch
 * worktree and nothing to merge. With it come `originDir` (which project this is) and
 * `base` (the sha the Changes rail measures from, or a fork of a session with work in
 * the tree would show no changes), and the model/effort pins, because a resend that
 * silently answered on a different model would be the one comparison the user is
 * running and cannot see.
 *
 * WHAT IT DOES NOT INHERIT: the workspace's *files* are not rewound. The fork carries
 * the CONVERSATION prefix; the checkout stays as it is. v1 forks history, and the
 * user's tree is the user's tree.
 *
 * The four cuts, all of which seed the copies STRICTLY BEFORE `atMessageId` first:
 *
 *   - `editedText`      — append the replacement as a new user message and run a real
 *                         turn from there. It may only replace a USER message: an
 *                         edited supervisor turn would put words in the model's mouth
 *                         and replay them as if it had said them (400).
 *   - (nothing)         — also copy the at-message itself: a branch point sitting
 *                         ready for new input, with no turn run.
 *   - `exclusive`       — skip that copy; the branch ends strictly before the
 *                         at-message, for a caller that intends to re-send it itself.
 *   - `atPart`          — cut INSIDE the at-message: copy it truncated to
 *                         `parts[0..atPart]` (history up to, say, a failed tool
 *                         result). Here `editedText` is a fresh user message appended
 *                         after the cut — the "don't try it that way" move — so any
 *                         at-message role is allowed.
 *
 * `exclusive` is meaningful only for the plain branch-point case, and is a no-op
 * otherwise rather than a third error: with `editedText` the at-message is replaced
 * and with `atPart` it is truncated, so in both the caller has already said what
 * becomes of it.
 *
 * A cut that strands a `tool_call` without its `tool_result` is legal and expected —
 * `atPart` exists precisely to cut mid-message — and it is `turn/replay.ts` that
 * makes it replayable: an unpaired call gets a synthetic "(interrupted)" result,
 * because every provider rejects a thread with the pair left open.
 *
 * Ported from `src/fork.ts`. Deltas from that port are marked `NOTE:`.
 */
import { ForkError, NotFoundError } from "../errors.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import { ForkBody } from "../schema/requests.ts";
import type { AppCtx } from "../types.ts";
import { baseTitle, openBranch } from "./branch.ts";
import { summarizeSpan } from "./compact.ts";
import { DEFAULT_MODEL } from "../turn/runner.ts";
import { type Handler, json, parseBody } from "../server/http.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/**
 * How the resend's turn is started.
 *
 * NOTE: the port called `startUserTurn` from `turn.ts` directly. Here the seam is read
 * off the ctx, structurally — this is `server/sessions.ts`'s `TurnStarter`, restated
 * rather than imported, exactly as `agents/notes.ts` and `hostfn/schedule.ts` restate
 * it. Two reasons, and the second is not optional: nothing outside `server/` should
 * depend on `server/`, and `server/app.ts` ⇄ `server/sessions.ts` is an evaluation
 * cycle that only resolves because `app.ts` evaluates first — a module reached FROM
 * the route table that imported `sessions.ts` would read its exports in the temporal
 * dead zone and throw at init.
 */
export type ForkStarter = (ctx: AppCtx, session: Session, message: Message) => unknown;

/** The ctx field boot assigns. `AppCtx` (T-1) is frozen, so it is declared here. */
interface WithStarter {
  startTurn?: ForkStarter;
}

export interface ForkDeps {
  /** Absent = `ctx.startTurn`. Absent there too = the branch is seeded, not run. */
  start?: ForkStarter;
}

export interface ForkResult {
  /** The branch, as storage kept it (pins included). */
  session: Session;
  /** The branch's own messages, in the order they were seeded. */
  messages: Message[];
  /**
   * A real turn was started on the branch. False for every cut that only seeds — and
   * false, deliberately, when `editedText` was given but no starter is wired: the
   * edited message is on the branch either way, and the caller must not be told a
   * turn is coming that is not.
   */
  turnStarted: boolean;
  /**
   * Present when the starter returned a promise — tests await it. Production's
   * starters return `void`: the turn outlives the request that asked for it.
   */
  done?: Promise<unknown>;
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

/**
 * Fork `sessionId` at `body.atMessageId`. Throws `ForkError` (400) for a fork point
 * this session cannot cut at, `NotFoundError` (404) for an unknown session.
 *
 * Every validation happens BEFORE `openBranch`, and that ordering is load-bearing:
 * the seeder announces `session.created` the moment it is opened, so a check that
 * ran afterwards would leave an empty half-seeded branch in the user's session list
 * every time a client sent a bad fork point.
 */
export function fork(
  ctx: AppCtx,
  sessionId: string,
  body: ForkBody,
  deps: ForkDeps = {},
): ForkResult {
  const source = ctx.db.getSession(sessionId);
  if (!source) throw new NotFoundError(`session ${sessionId} not found`);

  // The session's OWN messages. Not `threadFor`: inherited ancestors are a different
  // session's rows, and a branch cannot cut into them (see the header).
  const own = ctx.db.messagesFor(sessionId);
  const atIdx = own.findIndex((m) => m.id === body.atMessageId);
  if (atIdx < 0) throw new ForkError(400, badForkPoint(ctx, source, body.atMessageId));
  const at = own[atIdx];

  const edited = body.editedText !== undefined;
  // Trimmed like the HTTP post path (`server/sessions.ts`), and empty is refused for
  // the same reason it is there: an empty user message is a turn asked to answer
  // nothing, and it replays as an empty text block several providers reject outright.
  const editedText = body.editedText?.trim() ?? "";
  if (edited && !editedText) {
    throw new ForkError(
      400,
      "editedText is empty — send the replacement text, or omit editedText entirely " +
        "to branch at that message and leave the composer ready for new input.",
    );
  }
  // Without `atPart`, `editedText` REPLACES the at-message, which is only coherent for
  // a user turn: an edited supervisor message would be a sentence the model never
  // wrote, replayed to it next turn as though it had. With `atPart` it is a fresh
  // message appended after the cut, so any role is fine.
  if (edited && body.atPart === undefined && at.role !== "user") {
    throw new ForkError(
      400,
      `editedText can only replace a user message, and ${body.atMessageId} is a ` +
        `${at.role} message. Fork it without editedText to branch from it, or pass ` +
        `the user turn you meant to edit.`,
    );
  }
  if (body.atPart !== undefined && body.atPart >= at.parts.length) {
    throw new ForkError(
      400,
      `atPart ${body.atPart} is out of range for message ${body.atMessageId}, which ` +
        `has ${at.parts.length} part(s) — the last cut point is ${at.parts.length - 1}.`,
    );
  }

  // Titled after the branch point so several forks of one session stay tellable apart
  // in the pickers, falling back to the source's BASE title — a fork of a fork must
  // not compound into "fork · fork · X" (`branch.ts`).
  const seeder = openBranch(ctx, {
    parentId: source.parentId,
    title: `fork · ${excerptOf(at) || baseTitle(source.title)}`,
    kind: "fork",
    workspace: source.workspace ?? null,
    originDir: source.originDir ?? null,
    // NOTE: not in the port, which snapshotted workspaces. The Changes rail is
    // `git diff <base>` (spec §13), so a branch sharing its target's checkout must
    // share the sha that checkout's change set is measured from.
    base: source.base ?? null,
    originId: source.id, // lineage: the forked-from session…
    originMessageId: body.atMessageId, // …and the at-message
  });
  const branch = inheritPins(ctx, source, seeder.session);

  // The prefix, strictly before the fork point. Every mode starts here.
  const messages = own.slice(0, atIdx).map((m) => seeder.copy(m));

  if (body.atPart !== undefined) {
    // Mid-message cut: the at-message survives truncated to the cut point.
    messages.push(seeder.copy({ ...at, parts: at.parts.slice(0, body.atPart + 1) }));
  } else if (!edited && !body.exclusive) {
    // Plain branch point: include the fork-point message, ready for new input —
    // unless the caller asked for an exclusive cut to re-send it itself.
    messages.push(seeder.copy(at));
  }

  if (!edited) return { session: branch, messages, turnStarted: false };

  // Edit & resend. The user message goes through the seeder like every other seeded
  // message — announced and indexed the same way — and then the ordinary turn path
  // runs it. A branch created microseconds ago cannot be busy, so there is no queue
  // check here; the ordering that keeps this message after the copies is the seeder's
  // (plan §6.1).
  const user = seeder.add("user", [{ type: "text", text: editedText }]);
  messages.push(user);

  const start = deps.start ?? (ctx as AppCtx & WithStarter).startTurn;
  // An unwired starter degrades to "the branch exists carrying the edited message",
  // never to a crash — the same shape `server/sessions.ts` and `schedules.ts` accept.
  if (!start) return { session: branch, messages, turnStarted: false };

  const running = start(ctx, branch, user);
  const done = running instanceof Promise ? running : undefined;
  // Handled here so a caller that discards `done` (the HTTP path does — the turn
  // outlives the 201) cannot produce an unhandled rejection that takes the process
  // down. Attaching to a DERIVED promise leaves the original rejecting for a test
  // that awaits it.
  done?.catch((err) => console.error(`fork turn failed [${branch.id}]:`, err));

  return { session: branch, messages, turnStarted: true, done };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Why this message cannot be a fork point, in terms of the move that works.
 *
 * The interesting case is the one spec §14 calls out: the id is real and the user can
 * see it in this session's transcript, because the transcript is ancestors ++ own. A
 * bare "not found" would send them looking for a client bug; naming the ancestor names
 * the session they should be forking.
 */
function badForkPoint(ctx: AppCtx, source: Session, atMessageId: string): string {
  const message = ctx.db.getMessage(atMessageId);
  if (!message) return `no message ${atMessageId} exists`;
  if (message.sessionId === source.id) {
    // Belongs here but is not in `messagesFor` — an ordering or storage defect, not a
    // user error. Say what was actually observed rather than blaming the request.
    return `message ${atMessageId} is not in session ${source.id}'s message list`;
  }
  const inherited = ctx.db.ancestorChain(source.id)
    .some((s) => s.id === message.sessionId);
  return inherited
    ? `message ${atMessageId} belongs to ancestor session ${message.sessionId}, whose ` +
      `history this session inherits but does not own — fork ${message.sessionId} instead`
    : `message ${atMessageId} belongs to session ${message.sessionId}, not ${source.id} ` +
      `— fork a session at one of its own messages`;
}

/** The at-message's first line of text, for the branch title. */
function excerptOf(at: Message): string {
  const text = at.parts.find((p): p is Extract<Part, { type: "text" }> => p.type === "text");
  return text ? text.text.split("\n")[0].trim().slice(0, 48) : "";
}

/**
 * Carry the source's per-session model/effort pins onto the branch.
 *
 * NOTE: not in the port, and not something `BranchSpec` can express (it is T8.1's
 * file). It matters most for exactly the operation this module exists for: "edit &
 * resend" is a controlled comparison — same history, one changed message — and a
 * branch that fell back to the global default would answer it on a different model
 * with nothing in the UI saying so (spec §12: switching the default leaves existing
 * sessions alone).
 *
 * Announced as `session.updated` rather than folded into the create, because
 * `openBranch` has already published `session.created`; a client reconciles by id and
 * ends up with the same row either way.
 */
function inheritPins(ctx: AppCtx, source: Session, branch: Session): Session {
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
 * `POST /sessions/:id/fork` — `{session, thread, turnStarted}`, 201.
 *
 * The thread rides along for the same reason `GET /sessions/:id` carries it: the
 * client is about to switch to this branch, and a create that answered with a bare
 * session id would force an immediate second fetch to render anything at all.
 *
 * 201 with the turn possibly still running is deliberate — the BRANCH is what was
 * created and it is complete; the turn reports over `/events` like every other turn,
 * and `turnStarted` says whether to expect one.
 */
export const forkSessionH: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, ForkBody);
  const { session, turnStarted } = fork(ctx, params.id, body);
  // pi's branch-summary-on-switch. The abandoned path is everything from the fork
  // point to the end of the SOURCE — precisely what you stop being able to see the
  // moment you branch — so the essence of it is carried onto the new path as a
  // system note rather than lost. Best-effort by construction: a summariser that
  // fails must not fail the branch, which already exists and is already correct.
  if (body.summarizeAbandoned) {
    const own = ctx.db.messagesFor(params.id);
    const at = own.findIndex((m) => m.id === body.atMessageId);
    const abandoned = at < 0 ? [] : own.slice(at);
    if (abandoned.length > 0) {
      try {
        const model = session.model ?? ctx.model ?? DEFAULT_MODEL;
        const text = await summarizeSpan(ctx, model, abandoned);
        // Seeded exactly the way `branch.ts` seeds: complete on arrival, never
        // `pending` (nothing exists to close it), announced so an open client
        // renders it with no new reducer.
        const note = ctx.db.createMessage({
          id: crypto.randomUUID(),
          sessionId: session.id,
          role: "system",
          parts: [{
            type: "text",
            text: `Summary of the path this branch left behind:\n\n${text}`,
          }],
          pending: false,
          createdAt: (ctx.now ?? Date.now)(),
        });
        ctx.bus.publish({ type: "message.started", sessionId: session.id, data: note });
      } catch (error) {
        console.error(`branch summary failed [${session.id}]:`, error);
      }
    }
  }
  return json({ session, thread: ctx.db.threadFor(session.id), turnStarted }, 201);
};
