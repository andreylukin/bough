/**
 * The supervisor state machine, persisted to the `turns` table. A turn is created
 * `running` when the runner starts, checkpointed after each API round and each tool
 * result, and finished `done`/`error` at the end. If the process dies mid-turn, the
 * row is left `running`; on the next boot `recoverOrphanedTurns` finds those rows,
 * marks them `orphaned`, and finishes their still-pending messages so the UI never
 * shows a turn stuck in flight forever.
 *
 * Shipped: orphan-and-surface. A resume() that re-enters the loop from the last
 * checkpoint is not shipped — re-running a partially-executed tool round risks
 * duplicate side effects, so surfacing the interruption is the safe default.
 */
import type { Db, Turn, TurnStatus } from "../db/db.ts";
import type { Bus } from "../bus.ts";
import type { Part } from "../schema/parts.ts";

export function startTurn(db: Db, sessionId: string, messageId: string): Turn {
  return db.createTurn({
    id: crypto.randomUUID(),
    sessionId,
    messageId,
    status: "running",
    step: "start",
    updatedAt: Date.now(),
  });
}

export function checkpoint(db: Db, turnId: string, step: string): void {
  db.updateTurn(turnId, { step });
}

export function finishTurn(db: Db, turnId: string, status: Exclude<TurnStatus, "running">): void {
  db.updateTurn(turnId, { status });
}

/**
 * Mark every still-`running` turn as orphaned and finish its pending message.
 * Returns the number recovered. Call once at server start (see main.ts).
 */
export function recoverOrphanedTurns(db: Db, bus: Bus): number {
  const stranded = db.turnsByStatus("running");
  for (const turn of stranded) {
    db.updateTurn(turn.id, { status: "orphaned" });
    const msg = db.getMessage(turn.messageId);
    if (!msg || !msg.pending) continue;
    const note: Part = {
      type: "text",
      text: "⚠︎ Interrupted: the server restarted before this turn finished.",
    };
    const parts = [...msg.parts, note];
    db.updateMessage(msg.id, parts, false);
    bus.publish({ type: "message.part", sessionId: msg.sessionId, data: { messageId: msg.id, part: note } });
    bus.publish({ type: "message.finished", sessionId: msg.sessionId, data: { messageId: msg.id } });
  }
  return stranded.length;
}
