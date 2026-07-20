/**
 * Per-session usability metrics, derived from what the server already persists
 * (messages, turns, net_events) plus the turns.first_output_at stamp (turn.ts).
 *
 * These operationalize the collaboration metrics from the HCI literature the
 * probes/ suite is built around:
 *   - time-to-first-output  — how long the user stares at a blank turn (HALIE's
 *     process metrics; Claude Code's "interrupt within 3s" design target)
 *   - approval prompts      — interruption cost of the Claw Patrol hold-and-ask gate
 *   - turns-to-done         — refinement convergence across a session (TCR)
 *   - interruptions         — how often the user had to pull the brake
 *
 * Everything here is a pure read of the Db — no live-loop coupling. Exposed over
 * GET /sessions/:id/metrics (app.ts) and read directly by probes/report.sh.
 */
import type { Db } from "./db/db.ts";

/** count/median/max over a duration sample, absent when the sample is empty. */
export interface DurationStats {
  count: number;
  medianMs: number;
  maxMs: number;
}

export interface SessionMetrics {
  sessionId: string;
  /** Prompts the user sent (turns-to-done for a finished task). */
  userTurns: number;
  /** Supervisor replies (≈ turns run). */
  assistantTurns: number;
  toolCalls: number;
  /** Turns the user stopped mid-flight (Esc / interrupt endpoint). */
  interrupted: number;
  /** Turns that ended in error or were orphaned by a restart. */
  failed: number;
  /** Net-gate holds that asked a human (resolved or still pending). */
  approvalPrompts: number;
  /** user message → first visible output of the turn it started. */
  firstOutput: DurationStats | null;
  /** user message → turn finished. */
  turnDuration: DurationStats | null;
  /** First message → last message activity in the session. */
  wallClockMs: number;
  /** Cumulative session tokens (input includes cache reads+writes; the cache
   * splits let callers price at discounted rates). costUsd is priced per round
   * at the round's model (pricing.ts) — 0 for catalog-unknown models. */
  usage: {
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens: number;
    cacheWriteTokens: number;
    costUsd: number;
  };
}

function stats(samples: number[]): DurationStats | null {
  if (samples.length === 0) return null;
  const sorted = [...samples].sort((a, b) => a - b);
  return {
    count: sorted.length,
    medianMs: sorted[Math.floor(sorted.length / 2)],
    maxMs: sorted[sorted.length - 1],
  };
}

/**
 * A net event that parked (or is parked) for HUMAN approval, recognized by the
 * gate's outcome reasons (gate.ts / main.ts boot sweep). Reason-string matching
 * is a heuristic — the gate doesn't persist a "was held" flag — but every string
 * here is emitted only on the hold path, so false positives can't come from
 * plain allow/deny rules.
 */
const HOLD_REASONS = [
  "approved by human",
  "denied by human",
  "approved by chain:",
  "held for approval",
  "approved — retry to proceed",
  "expired — turn ended before approval",
  "expired — server restarted before approval",
];

export function sessionMetrics(db: Db, sessionId: string): SessionMetrics {
  const messages = db.messagesFor(sessionId);
  const userTurns = messages.filter((m) => m.role === "user").length;
  const supervisor = messages.filter((m) => m.role === "supervisor");
  const toolCalls = supervisor.reduce(
    (n, m) => n + m.parts.filter((p) => p.type === "tool_call").length,
    0,
  );

  const turns = db.turnsForSession(sessionId);
  const started = new Map(messages.map((m) => [m.id, m.createdAt]));
  const firstOutputSamples: number[] = [];
  const durationSamples: number[] = [];
  let interrupted = 0;
  let failed = 0;
  for (const t of turns) {
    const at = started.get(t.messageId);
    if (t.status === "interrupted") interrupted++;
    if (t.status === "error" || t.status === "orphaned") failed++;
    if (at === undefined) continue;
    if (t.firstOutputAt !== null) firstOutputSamples.push(t.firstOutputAt - at);
    if (t.status !== "running") durationSamples.push(t.updatedAt - at);
  }

  const approvalPrompts = db.recentNetEvents(sessionId, 10_000).filter((e) =>
    e.verdict === "pending" ||
    (e.reason !== undefined && HOLD_REASONS.some((r) => e.reason!.includes(r)))
  ).length;

  const wallClockMs = messages.length < 2
    ? 0
    : messages[messages.length - 1].createdAt - messages[0].createdAt;

  return {
    sessionId,
    userTurns,
    assistantTurns: supervisor.length,
    toolCalls,
    interrupted,
    failed,
    approvalPrompts,
    firstOutput: stats(firstOutputSamples),
    turnDuration: stats(durationSamples),
    wallClockMs,
    usage: (({ inputTokens, outputTokens, cacheReadTotal, cacheWriteTotal, costUsd }) => ({
      inputTokens,
      outputTokens,
      cacheReadTokens: cacheReadTotal,
      cacheWriteTokens: cacheWriteTotal,
      costUsd,
    }))(db.sessionUsage(sessionId)),
  };
}
