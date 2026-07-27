/**
 * The turn's persisted state machine, and the boot recovery that depends on it.
 *
 * THE INVARIANT: **a session is never busy forever.** `busySessionIds()` reads
 * `turns WHERE status = 'running'`, so a `running` row is what blocks a session from
 * accepting a new turn. If the process dies mid-turn that row survives the process
 * that was executing it, and the session is wedged: every later post queues behind a
 * turn that no longer exists, and the transcript ends on a `pending` supervisor
 * message that will never finish. Recovery at boot is the only thing that can
 * observe this, because from inside a run there is no difference between "still
 * working" and "the machine it was working on is gone."
 *
 * That is why the turn is checkpointed at all. `step` is not telemetry — it is the
 * evidence a restart reads. The runner writes it after each API round and each tool
 * result, so an orphaned row can say *where* it died, and the user is told the
 * server restarted rather than left staring at a spinner.
 *
 * **Orphan-and-surface, not resume.** A checkpoint is deliberately not enough to
 * re-enter the loop from. A turn's last step may have been a program that wrote
 * files, pushed a commit, or spawned a subagent; re-running it because a checkpoint
 * says it started would duplicate every one of those side effects, and nothing in
 * the checkpoint distinguishes "the program was about to run" from "it ran and the
 * result was not written yet". Surfacing the interruption is the honest answer: the
 * user sees what happened and decides.
 *
 * Ported from `src/supervisor/turns.ts`. Deltas are marked `NOTE:`.
 */
import type { Bus, Db, Usage } from "../types.ts";
import type { Part, Turn, TurnStatus } from "../schema/parts.ts";

/** A turn that has ended, whatever the outcome. */
export type FinalTurnStatus = Exclude<TurnStatus, "running">;

/** The checkpoint a turn starts on, before the first round. */
export const INITIAL_STEP = "start";

/**
 * What an orphaned turn's message ends on.
 *
 * It says the SERVER restarted, not that the turn failed: the distinction is the
 * whole point of the status. Work the turn had already done — files written,
 * commands run, commits made — still stands, and a user told only "failed" will
 * redo it.
 */
export const ORPHAN_NOTE =
  "⚠︎ Interrupted: the server restarted before this turn finished. Anything it had " +
  "already done (files written, commands run) still stands — check the changes, then " +
  "continue.";

/** The `error` recorded on an orphaned turn row. */
export const ORPHAN_ERROR = "the server restarted while this turn was running";

// ---------------------------------------------------------------------------
// Checkpointing
// ---------------------------------------------------------------------------

/**
 * Open a turn row, `running`, against the pending supervisor message.
 *
 * NOTE: `now` is a parameter where the port called `Date.now()` inline. Every
 * timestamp in the new tree comes from an injected clock (plan §0), and a turn's
 * `createdAt` is one a test wants to pin.
 */
export function startTurn(
  db: Db,
  sessionId: string,
  messageId: string,
  now: () => number = Date.now,
): Turn {
  const at = now();
  return db.createTurn({
    id: crypto.randomUUID(),
    sessionId,
    messageId,
    status: "running",
    step: INITIAL_STEP,
    createdAt: at,
    updatedAt: at,
    error: null,
  });
}

/**
 * Record progress. Every call bumps `updated_at` (the db does it from its own
 * clock), which is the part that matters: a checkpoint's job is to say *when* the
 * turn last moved.
 *
 * `usage` REPLACES the row's usage rather than accumulating — the runner carries the
 * turn's running total and hands the whole of it over each time. Accumulating here
 * as well would double-count every round after the first.
 */
export function checkpoint(db: Db, turnId: string, step: string, usage?: Usage): void {
  db.updateTurn(turnId, { step, ...(usage ? { usage } : {}) });
}

/**
 * Close a turn. `error` is written on every path, so a turn that fails and is later
 * re-driven does not keep a stale message from the previous attempt.
 */
export function finishTurn(
  db: Db,
  turnId: string,
  status: FinalTurnStatus,
  opts: { error?: string | null; usage?: Usage; step?: string } = {},
): void {
  db.updateTurn(turnId, {
    status,
    error: opts.error ?? null,
    ...(opts.step ? { step: opts.step } : {}),
    ...(opts.usage ? { usage: opts.usage } : {}),
  });
}

// ---------------------------------------------------------------------------
// Boot recovery
// ---------------------------------------------------------------------------

/** One recovered turn, for the caller's log line and for M4's failure matrix. */
export interface OrphanedTurn {
  turnId: string;
  sessionId: string;
  messageId: string;
  /** The checkpoint the turn died on — where it got to. */
  step: string;
  /** True when the supervisor message was still `pending` and has now been closed. */
  closedMessage: boolean;
}

export interface RecoverOptions {
  /**
   * Called once per orphaned turn, after the row and its message are settled.
   *
   * The seam exists because a stranded **subagent** has a spawner waiting on a
   * result that lives in a process-memory map which died with the process — the
   * parent has to be told, distinguishably, that its child was orphaned (plan T4.4's
   * failure matrix). That notice is M4's to write and needs M4's note-delivery
   * rules, so recovery calls out instead of reaching across the boundary. A hook
   * that throws is isolated: one unnotifiable parent must not abandon the remaining
   * orphans, which would leave those sessions wedged — exactly the failure this
   * whole module exists to prevent.
   */
  onOrphan?: (orphan: OrphanedTurn) => void;
  /** Where a throwing `onOrphan` is reported. Defaults to `console.error`. */
  onHookError?: (error: unknown, orphan: OrphanedTurn) => void;
}

/**
 * Mark every still-`running` turn `orphaned`, close its pending message, and
 * announce both. Returns what was recovered. Call once at server start, before the
 * listener binds — a client that connects first would otherwise fetch a session that
 * looks busy and render a turn in flight.
 *
 * Idempotent: a second call finds nothing, because the first left no `running` rows.
 */
export function recoverOrphanedTurns(
  db: Db,
  bus: Bus,
  opts: RecoverOptions = {},
): OrphanedTurn[] {
  const onHookError = opts.onHookError ??
    ((err: unknown, o: OrphanedTurn) =>
      console.error(`orphan hook threw for turn ${o.turnId}:`, err));

  const stranded = db.turnsByStatus("running");
  const recovered: OrphanedTurn[] = [];

  for (const turn of stranded) {
    // The row first. Until this lands the session is still busy, and every step
    // below can fail without re-wedging it.
    finishTurn(db, turn.id, "orphaned", { error: ORPHAN_ERROR });

    const message = db.getMessage(turn.messageId);
    let closedMessage = false;
    if (message?.pending) {
      const note: Part = { type: "text", text: ORPHAN_NOTE };
      const parts = [...message.parts, note];
      db.updateMessage(message.id, parts, false);
      bus.publish({
        type: "message.part",
        sessionId: message.sessionId,
        data: { messageId: message.id, part: note },
      });
      bus.publish({
        type: "message.finished",
        sessionId: message.sessionId,
        data: { messageId: message.id },
      });
      closedMessage = true;
    }

    // Emitted even when the message was already closed: `turn.finished` is what a
    // client keys its "is this session busy" state off, and a turn that ends in a
    // state nobody is told about is the same hang in the UI instead of the db.
    bus.publish({
      type: "turn.finished",
      sessionId: turn.sessionId,
      data: {
        turnId: turn.id,
        sessionId: turn.sessionId,
        status: "orphaned",
        error: ORPHAN_ERROR,
      },
    });

    const orphan: OrphanedTurn = {
      turnId: turn.id,
      sessionId: turn.sessionId,
      messageId: turn.messageId,
      step: turn.step,
      closedMessage,
    };
    recovered.push(orphan);
    if (opts.onOrphan) {
      try {
        opts.onOrphan(orphan);
      } catch (err) {
        onHookError(err, orphan);
      }
    }
  }

  return recovered;
}
