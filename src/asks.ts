/**
 * ask() holds — the mid-task question primitive. A run_steps program calls
 * `await ask("Which environment?", {options: ["dev", "prod"]})` and its turn parks
 * here:
 *
 *   raiseAsk ──▶ register + emit `ask.question` (status "pending")
 *           └──▶ block until answerAsk / declineAsk (POST /sessions/:id/questions/:qid)
 *                or the turn's interrupt signal — then re-emit the SAME id with its
 *                final status and settle the program's promise.
 *
 * Deliberately memory-only: a pending
 * ask only means anything while its turn is alive (the hold dies with the process),
 * and the settled Q/A persists as an AskPart on the supervisor message (turn.ts).
 * A freshly-attached TUI rebuilds the hold card from GET /questions and live-updates
 * from the bus; a server restart leaves nothing stale to heal.
 *
 * Module-level registry (turn.ts `running`-map precedent): both the turn runner and
 * the HTTP routes reach it without threading an instance through AppCtx.
 */
import type { Bus } from "./bus.ts";
import type { AskQuestion } from "./schema/parts.ts";

interface PendingAsk {
  record: AskQuestion;
  settle: (status: "answered" | "declined" | "interrupted", answer?: string) => void;
}

const pending = new Map<string, PendingAsk>();

/**
 * Raise one question and park until it settles. Returns the live record (its
 * `status` mutates as it settles — the turn runner reads it to label the AskPart)
 * plus the promise the program awaits: resolves with the answer, rejects with a
 * catchable "user declined" error on decline, and rejects on `signal` abort so the
 * program unwinds like every other host function when the user stops the turn.
 * No timeout — a question is user-paced by design.
 */
export function raiseAsk(
  bus: Bus,
  q: { sessionId: string; messageId: string; question: string; options?: string[] },
  signal?: AbortSignal,
): { record: AskQuestion; answer: Promise<string> } {
  const record: AskQuestion = {
    id: crypto.randomUUID(),
    sessionId: q.sessionId,
    messageId: q.messageId,
    question: q.question,
    ...(q.options?.length ? { options: q.options } : {}),
    status: "pending",
    ts: Date.now(),
  };
  const answer = new Promise<string>((resolve, reject) => {
    const onAbort = () => settle("interrupted");
    const settle: PendingAsk["settle"] = (status, ans) => {
      if (!pending.delete(record.id)) return; // already settled
      signal?.removeEventListener("abort", onAbort);
      record.status = status;
      if (ans !== undefined) record.answer = ans;
      // Re-emit on the same id so the hold card updates in place.
      bus.publish({ type: "ask.question", sessionId: record.sessionId, data: { ...record } });
      if (status === "answered") resolve(ans!);
      else if (status === "declined") {
        reject(new Error(`user declined to answer: ${record.question}`));
      } else reject(new Error("ask() interrupted — the turn was stopped before an answer"));
    };
    // Register BEFORE announcing so a listener that answers synchronously (tests,
    // a same-process client) finds the hold.
    pending.set(record.id, { record, settle });
    bus.publish({ type: "ask.question", sessionId: record.sessionId, data: { ...record } });
    if (signal?.aborted) return settle("interrupted");
    signal?.addEventListener("abort", onAbort, { once: true });
  });
  return { record, answer };
}

/** Settle a pending question with the user's answer. False if the id isn't waiting. */
export function answerAsk(id: string, answer: string): boolean {
  const hold = pending.get(id);
  if (!hold) return false;
  hold.settle("answered", answer);
  return true;
}

/** Decline a pending question — ask() rejects with a catchable "user declined" error. */
export function declineAsk(id: string): boolean {
  const hold = pending.get(id);
  if (!hold) return false;
  hold.settle("declined");
  return true;
}

/** A pending question by id (route lookup), or undefined. */
export function getAsk(id: string): AskQuestion | undefined {
  return pending.get(id)?.record;
}

/** Questions currently awaiting an answer, oldest first (optionally per-session). */
export function pendingAsks(sessionId?: string): AskQuestion[] {
  return [...pending.values()]
    .map((h) => h.record)
    .filter((r) => sessionId === undefined || r.sessionId === sessionId)
    .sort((a, b) => a.ts - b.ts);
}

/**
 * Interrupt-and-clear parked questions whose turn is gone — for one session, or all
 * (sessionId undefined). Without this, a program that dies
 * without unwinding its ask (wall-clock timeout terminates the worker, not the host
 * promise) leaves a hold card haunting every session. Returns the count.
 */
export function expireAsks(sessionId?: string): number {
  let n = 0;
  for (const hold of [...pending.values()]) {
    if (sessionId !== undefined && hold.record.sessionId !== sessionId) continue;
    hold.settle("interrupted");
    n++;
  }
  return n;
}
