/**
 * Harness-injected system notes, and the wake rule that decides when one costs a
 * turn.
 *
 * THE INVARIANT THIS HOLDS: **a note reaches the session exactly once, and never as
 * a second concurrent turn.** A session runs at most one turn at a time (spec §5),
 * and the things that post here — a detached subagent finishing, a background shell
 * exiting, an artifact comment batch — all arrive from *outside* any turn, at a
 * moment nobody chose. So every post lands in one of two states and there is no
 * third:
 *
 *   - **The spawner is idle.** The note starts a fresh turn, because otherwise the
 *     report sits in the transcript with nothing to read it: the model is not
 *     polling, and the user asked for delegated work, not for a mailbox.
 *   - **A turn is already in flight.** The note rides the queued drain (spec §7).
 *     It is persisted and announced immediately — so the UI shows it the instant it
 *     happens — and `turn/queue.ts`'s derived check (`hasUnansweredInput`: a `user`
 *     or `system` message after the session's last supervisor message) makes the
 *     running turn's own drain pick it up when it ends. Nothing here starts a turn
 *     in that state, and the registry would refuse it anyway.
 *
 * That second path is why the note is *persisted before* it is decided upon. The
 * queue is derived from the database rather than from an in-memory flag, so a note
 * that lands one microsecond before a turn ends is already in the transcript that
 * the drain check reads; a flag would have to be handed across that boundary and
 * would be lost by a restart.
 *
 * TWO PLACES A NOTE DELIBERATELY DOES NOT WAKE:
 *
 *   1. **A stop stays stopped.** An explicit interrupt cascades into a session's
 *      detached children (`turn/queue.ts`), so the user's stop produces exactly the
 *      completion notes it just caused. Waking an idle spawner on those would
 *      restart, in seconds, the work the user just stopped. So a note is recorded
 *      without waking when the session's own last turn ended `interrupted`.
 *   2. **Boot recovery.** A subagent stranded by a restart owes its spawner a note
 *      (plan T4.4's failure matrix), but `turn/state.ts` is explicit that recovery
 *      surfaces rather than resumes: a server coming back must not immediately spend
 *      tokens on sessions nobody has returned to. The note is written into the
 *      thread, and the model reads it on the next turn the user starts.
 *
 * WHY THE STARTER IS READ OFF THE CTX. Waking a session must produce the same turn a
 * posted user message would: the same delegation tier, the same granted host
 * functions, the same `survivingJobs` seam. `server/sessions.ts` already reads that
 * one seam (`ctx.startTurn`) off the ctx, and boot wires it once — so this module
 * borrows it rather than constructing a second, subtly different turn. It is typed
 * structurally here, never imported: `agents/` does not depend on `server/`, and
 * `server/sessions.ts` ↔ `server/app.ts` is a cycle that only resolves one way.
 *
 * Ported from `src/subagent.ts` (`formatNote`) and `src/turn.ts` (`postSystemNote`).
 * Deltas from that port are marked `NOTE:`.
 */
import type { Message, Part, Session } from "../schema/parts.ts";
import type { AppCtx, Db, TurnCtx } from "../types.ts";
import type { OrphanedTurn } from "../turn/state.ts";
import { TurnRegistry, turns as defaultRegistry } from "../turn/queue.ts";
import { buildResult, type SubagentResult } from "./subagent.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/**
 * How a session's turn is started. `beginTurn`'s starter (`createTurnStarter`, and
 * the delegating one that supersedes it at boot) satisfies this.
 *
 * Structural on purpose — see the module header: this is `server/sessions.ts`'s
 * `TurnStarter`, restated rather than imported.
 */
export type NoteStarter = (ctx: AppCtx, session: Session, message: Message) => unknown;

/** The ctx field boot assigns. `AppCtx` (T-1) is frozen, so it is declared here. */
interface WithStarter {
  startTurn?: NoteStarter;
}

/**
 * What the post did about the note, beyond persisting it.
 *
 *   - `started`  — the session was idle; a fresh turn was asked for.
 *   - `queued`   — a turn is in flight; the note drains into the next one.
 *   - `recorded` — written and announced, waking nothing (see the module header's
 *                  two cases, and the no-starter case: an unwired seam degrades to
 *                  "the model reads it next turn", never to a lost note).
 *   - `dropped`  — no such session. Nothing was written.
 */
export type WakeOutcome = "started" | "queued" | "recorded" | "dropped";

/** What a post reports back. Tests assert on it; production ignores it. */
export interface NoteDelivery {
  /** The persisted note, or `null` when the session was gone. */
  message: Message | null;
  wake: WakeOutcome;
}

export interface NoteDeps {
  /** Absent = the process registry, which is what the turn runner defaults to too. */
  registry?: TurnRegistry;
  /** Injected clock. Absent = `ctx.now`, then `Date.now`. */
  now?: () => number;
  /** Absent = `ctx.startTurn`. Absent there too = the note is recorded, not woken. */
  start?: NoteStarter;
  /**
   * `auto` (default) applies the wake rule; `never` records the note and wakes
   * nothing. Boot recovery passes `never` — see the module header.
   */
  wake?: "auto" | "never";
  /** Extra parts riding with the note. `image()` (T6.4) attaches a picture this way. */
  extra?: Part[];
  /** Where a failed wake is reported. Tests pass a collector. */
  reportError?: (error: unknown, sessionId: string) => void;
}

// ---------------------------------------------------------------------------
// The post
// ---------------------------------------------------------------------------

/**
 * Persist a system note into `sessionId`, announce it, and apply the wake rule.
 *
 * **This never throws.** Every caller is a completion callback — a child's `.then`,
 * a shell's exit handler, a comment POST — with no round-trip to report a failure
 * on. A throw there would surface as an unhandled rejection and take the process
 * down, losing every other session with it, so a session that has gone missing is
 * reported as `dropped` and the note is not written (the row would fail the foreign
 * key anyway).
 */
export function postSystemNote(
  ctx: AppCtx,
  sessionId: string,
  text: string,
  deps: NoteDeps = {},
): NoteDelivery {
  const { db, bus } = ctx;
  const session = db.getSession(sessionId);
  if (!session) return { message: null, wake: "dropped" };

  const now = deps.now ?? ctx.now ?? Date.now;
  const message = db.createMessage({
    id: crypto.randomUUID(),
    sessionId,
    role: "system",
    parts: [{ type: "text", text }, ...(deps.extra ?? [])],
    // Complete when it lands: `pending` is the supervisor's streaming flag, and a
    // note left pending is a session the UI shows as busy forever.
    pending: false,
    createdAt: now(),
  });
  indexQuietly(db, message);
  bus.publish({ type: "message.started", sessionId, data: message });

  return { message, wake: wakeFor(ctx, session, message, deps) };
}

/**
 * The wake rule, and the only place it is decided.
 *
 * Order matters. The busy check comes first because it is the one that must never be
 * wrong: starting a turn on a session that already has one is the failure this whole
 * module is written around, and the registry — not the database — is the authority on
 * it (a turn claims the session synchronously in `beginTurn`, before its row exists).
 */
function wakeFor(
  ctx: AppCtx,
  session: Session,
  message: Message,
  deps: NoteDeps,
): WakeOutcome {
  if (deps.wake === "never") return "recorded";

  const registry = deps.registry ?? defaultRegistry;
  if (registry.isRunning(session.id)) {
    // The derived check would find this note on its own — it is already a `system`
    // message after the last supervisor one. The explicit nudge is belt and braces
    // for the case the derivation cannot see: a note whose session is busy with a
    // turn that has not yet written its supervisor placeholder.
    registry.enqueue(session.id);
    return "queued";
  }

  if (endedOnAnInterrupt(ctx.db, session.id)) return "recorded";

  const start = deps.start ?? (ctx as AppCtx & WithStarter).startTurn;
  if (!start) return "recorded";

  const report = deps.reportError ??
    ((err: unknown, id: string) => console.error(`failed to wake session ${id} with a note:`, err));
  try {
    const started = start(ctx, session, message);
    if (started instanceof Promise) started.catch((err) => report(err, session.id));
  } catch (err) {
    // A turn claimed the session between the check above and this call. The note is
    // already persisted, so the running turn's drain will find it — mark the nudge
    // and say so rather than losing the report to a race.
    registry.enqueue(session.id);
    report(err, session.id);
    return "queued";
  }
  return "started";
}

/**
 * Did this session's last turn end because the user stopped it?
 *
 * NOTE: a delta from the port, which woke on every unclaimed detached result. An
 * explicit stop cascades into detached children (`turn/queue.ts`), so those children
 * finish `interrupted` moments later and their notes would have restarted the very
 * work the stop just ended — the stop button appearing not to work, in the most
 * expensive possible way.
 *
 * KNOWN WINDOW: this reads the session's *last finished* turn, so a note that lands
 * while the interrupted turn is still winding down takes the `queued` path above and
 * the runner's own drain answers it — one round of the model reading a report it
 * cannot act on. Closing it would mean the queue drain distinguishing a stopped turn
 * from a completed one, which is `turn/queue.ts`'s rule to make (T2.4), not this
 * module's to work around.
 */
function endedOnAnInterrupt(db: Db, sessionId: string): boolean {
  return db.turnsForSession(sessionId).at(-1)?.status === "interrupted";
}

// ---------------------------------------------------------------------------
// A detached subagent's report
// ---------------------------------------------------------------------------

/** The marker the UI and the model both key off. Stable text, not decoration. */
export const SUBAGENT_NOTE_PREFIX = "[subagent finished]";

/**
 * How the child ended, in words the parent can act on.
 *
 * Four distinct outcomes, four distinct first words (plan T4.4's failure matrix): a
 * bare "failed" would make an errored child, a stopped one and one the server
 * restarted under look identical, and they call for different moves. Each line also
 * says what survived, because a subagent works in the SAME checkout — partial work
 * from a child that died is already on disk, and a parent told only "it failed" will
 * either redo it or build on top of it without looking.
 *
 * NOTE: the port had a fifth distinction here, `checkPassed`. The acceptance gate is
 * gone (spec §17) — `ok` now says only whether the child's TURN completed.
 */
const STATUS_TEXT: Record<SubagentResult["status"], string> = {
  done: "finished",
  error: "FAILED — its turn errored, and the report below carries the error. Nothing " +
    "retried it. Whatever it had already written is in the checkout",
  interrupted: "STOPPED — it was interrupted (a user stop, or it hit its wall-clock " +
    "limit). Whatever it had already written is in the checkout",
  orphaned: "ORPHANED — the server restarted before it finished. Whatever it had " +
    "already written is in the checkout",
};

/**
 * The note a detached child's report becomes.
 *
 * The last line is not filler. The single most common wrong move after a delegated
 * report is looking for the merge step, and there isn't one: subagents share their
 * spawner's checkout (spec §7, §17), so the edits are already present and the parent
 * must read them before building on top.
 */
export function formatSubagentNote(result: SubagentResult): string {
  // NOTE: "not reported" rather than "none". `changedFiles` is empty until the
  // changes module (T8.8) is wired into the launch seam, and "none" would be a claim
  // the harness cannot back — the child's writes are in the checkout either way.
  const files = result.changedFiles.length > 0 ? result.changedFiles.join(", ") : "not reported";
  return [
    `${SUBAGENT_NOTE_PREFIX} "${result.title}" (${result.sessionId}) — ${
      STATUS_TEXT[result.status]
    }.`,
    `Changed files: ${files}.`,
    result.report ? `Report:\n${result.report}` : "No report.",
    "It worked in THIS session's checkout, so its edits are already here — read them " +
    "before building on top; there is nothing to merge.",
  ].join("\n");
}

/**
 * Deliver an unclaimed detached result to its spawner.
 *
 * The ctx is the SPAWNING turn's, so `ctx.sessionId` is the spawner — which is the
 * session that asked for the work and the only one that can act on it. Claimed
 * results never reach here: `hostfn/delegate.ts` checks `record.claimed` first,
 * because a `join()`ed report already went back in-band and a note as well would tell
 * the spawner the same thing twice.
 */
export function deliverSubagentNote(
  ctx: TurnCtx,
  result: SubagentResult,
  deps: NoteDeps = {},
): NoteDelivery {
  return postSystemNote(ctx, ctx.sessionId, formatSubagentNote(result), deps);
}

/** The `deliver` seam `hostfn/delegate.ts` takes, bound to these deps. */
export function createNoteDeliverer(
  deps: NoteDeps = {},
): (ctx: TurnCtx, result: SubagentResult) => void {
  return (ctx, result) => {
    deliverSubagentNote(ctx, result, deps);
  };
}

// ---------------------------------------------------------------------------
// Background job exits
// ---------------------------------------------------------------------------

/**
 * The poster `hostfn/jobs.ts` calls when a background shell exits (spec §9).
 *
 * The registry formats its own text — it is the only thing that knows the exit code
 * and how many lines of output are waiting — and posts through here so a job exit and
 * a subagent report obey exactly one wake rule. That is the point of the seam: an
 * exiting shell has no turn and no ctx of its own, and `hostfn/` may not import from
 * `server/`, so boot hands the process-wide registry this closure over the one ctx.
 */
export function createJobNotifier(
  ctx: AppCtx,
  deps: NoteDeps = {},
): (sessionId: string, text: string) => void {
  return (sessionId, text) => {
    postSystemNote(ctx, sessionId, text, deps);
  };
}

// ---------------------------------------------------------------------------
// Boot recovery: the child a restart stranded
// ---------------------------------------------------------------------------

/**
 * Tell a spawner that one of its subagents was orphaned by a restart.
 *
 * Without this the fourth case of the failure matrix is silent: the detached
 * register is memory-only and died with the process, so the parent holds a promise
 * that will never settle and a branch that simply stopped. The note is the only
 * record that reaches the thread.
 *
 * Recorded, never woken — see the module header. Returns `null` for an orphan that
 * owes nobody a note (a root's own turn, a workflow agent whose run journal reports
 * it, a subagent with no origin edge).
 */
export async function noteOrphanedSubagent(
  ctx: AppCtx,
  orphan: OrphanedTurn,
  deps: NoteDeps = {},
): Promise<NoteDelivery | null> {
  const child = ctx.db.getSession(orphan.sessionId);
  if (!child || child.kind !== "subagent" || !child.originId) return null;
  const result = await buildResult({ db: ctx.db }, orphan.sessionId, orphan.messageId);
  return postSystemNote(ctx, child.originId, formatSubagentNote(result), {
    ...deps,
    wake: "never",
  });
}

/**
 * The whole recovered batch. One failure must not abandon the rest — the same rule
 * `recoverOrphanedTurns` holds for its own hook, and for the same reason: a spawner
 * that cannot be notified is a bad outcome, and every *other* spawner losing its
 * notice as well is a worse one.
 */
export async function noteOrphanedSubagents(
  ctx: AppCtx,
  orphans: readonly OrphanedTurn[],
  deps: NoteDeps = {},
): Promise<NoteDelivery[]> {
  const posted: NoteDelivery[] = [];
  for (const orphan of orphans) {
    try {
      const delivery = await noteOrphanedSubagent(ctx, orphan, deps);
      if (delivery) posted.push(delivery);
    } catch (err) {
      (deps.reportError ??
        ((e: unknown, id: string) =>
          console.error(`failed to note the orphaned subagent ${id}:`, e)))(
          err,
          orphan.sessionId,
        );
    }
  }
  return posted;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Keyword search is maintained on insert (plan T8.9). A note that fails to index is
 * a degraded search, never a lost note — and never a thrown completion callback.
 */
function indexQuietly(db: Db, message: Message): void {
  try {
    db.indexMessage(message);
  } catch (err) {
    console.error(`failed to index system note ${message.id}:`, err);
  }
}
