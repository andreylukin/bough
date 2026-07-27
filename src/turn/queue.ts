/**
 * One turn per session, and the three things that fall out of it: interrupt,
 * queueing, and the round-level retry ring.
 *
 * THE INVARIANT: **a session runs at most one turn at a time, and no user input is
 * ever lost to that rule** (spec §5). Concurrency in bough comes from subagents and
 * workflows, each of which is its own session; two turns interleaving inside one
 * session would mean two writers on one transcript and two models editing one
 * checkout with no shared view of it.
 *
 * Three consequences, each of which is a thing the runner asks this module:
 *
 *   **Interrupt is not a flag the loop checks between rounds.** A stop has to reach
 *   *into* a running program — kill the children, wind the worker down — and it has
 *   to reach detached work that is no longer tied to the turn's own signal. So the
 *   registry holds the live `AbortController` per session and a set of cascade hooks
 *   a detached child registers. A normal turn ending does **not** fire those hooks:
 *   a detached spawn is supposed to outlive its spawner's turn (spec §7). Only an
 *   explicit stop cascades.
 *
 *   **A message posted mid-turn queues, it does not race.** The queue is *derived
 *   from the database*, not from an in-memory flag: `hasUnansweredInput` asks whether
 *   any user or system message lands after the session's last supervisor message.
 *   That matters because the HTTP handler that persists a queued message
 *   (`server/sessions.ts`) deliberately does not know this module — it checks
 *   `busySessionIds()` and returns 202. An in-memory flag would need it to, and would
 *   be lost across a restart, stranding the message forever. The explicit `enqueue`
 *   below is a *nudge*, for a caller that has already decided a drain is owed; the
 *   derived check is the truth.
 *
 *   **A round that fails is retried, not executed.** The specific case this exists
 *   for: a tool call whose input was cut off mid-stream. `llm/stream.ts` refuses to
 *   invent `{}` for it and throws a retryable error, and the answer here is to
 *   re-stream the round — because executing it would run *the wrong program* against
 *   the user's checkout (spec §5 Retry). A truncation retries immediately; a
 *   provider outage waits, because those are different failures with different
 *   right answers. Retries are bounded, and an exhausted one is a turn error.
 *
 * Ported from `src/turn.ts` (the `running`/`queued` maps and the turn-level ring).
 * Deltas are marked `NOTE:`.
 */
import { LlmError } from "../errors.ts";
import { errName, isRetryable } from "../llm/client.ts";
import type { Db } from "../types.ts";

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/**
 * Live turns, queued drains, and interrupt cascades — per session.
 *
 * NOTE: a class where the port used three module-level `Map`/`Set` globals. The
 * process still gets exactly one (`turns` below), but a test gets its own and two
 * tests in the same file stop sharing interrupt state. That was not a theoretical
 * problem in the port: a test that interrupted "the" session reached into whatever
 * else was mid-flight.
 */
export class TurnRegistry {
  readonly #running = new Map<string, AbortController>();
  readonly #queued = new Set<string>();
  readonly #hooks = new Map<string, Set<() => void>>();

  /** Is a turn in flight for this session? */
  isRunning(sessionId: string): boolean {
    return this.#running.has(sessionId);
  }

  /** Sessions with a live turn. The prompt's running-subagent note reads this. */
  get runningSessions(): string[] {
    return [...this.#running.keys()];
  }

  /**
   * Claim the session and return the turn's interrupt.
   *
   * Throws when one is already running rather than replacing it: silently
   * overwriting the controller would leave the first turn unstoppable — its abort
   * handle gone while it kept writing to the same message.
   */
  begin(sessionId: string): AbortController {
    if (this.#running.has(sessionId)) {
      throw new Error(`a turn is already running for session ${sessionId}`);
    }
    const controller = new AbortController();
    this.#running.set(sessionId, controller);
    return controller;
  }

  /**
   * Release the session. Identity-checked: a late `end` from a turn that was
   * already superseded must not unregister the turn that replaced it.
   */
  end(sessionId: string, controller: AbortController): void {
    if (this.#running.get(sessionId) === controller) this.#running.delete(sessionId);
  }

  /**
   * Stop the session's turn and cascade to its detached children.
   *
   * Returns false only when there was nothing to stop. Hooks fire even when the
   * session itself is idle — its turn may have ended while a detached subagent it
   * spawned runs on, and that child is otherwise unstoppable.
   */
  interrupt(sessionId: string): boolean {
    const controller = this.#running.get(sessionId);
    controller?.abort();
    const hooks = this.#hooks.get(sessionId);
    if (hooks) {
      // Snapshot: a hook that unregisters itself must not mutate the set mid-walk.
      for (const hook of [...hooks]) {
        try {
          hook();
        } catch {
          // A child that is already gone is not an error — it is the goal.
        }
      }
    }
    return Boolean(controller) || (hooks?.size ?? 0) > 0;
  }

  /** Register a cascade hook for `sessionId`; the thunk unregisters it. */
  onInterrupt(sessionId: string, hook: () => void): () => void {
    let set = this.#hooks.get(sessionId);
    if (!set) this.#hooks.set(sessionId, set = new Set());
    set.add(hook);
    return () => {
      set.delete(hook);
      if (set.size === 0) this.#hooks.delete(sessionId);
    };
  }

  /** Mark that a drain is owed for this session regardless of what the db says. */
  enqueue(sessionId: string): void {
    this.#queued.add(sessionId);
  }

  /** Take-and-clear the nudge. */
  drain(sessionId: string): boolean {
    return this.#queued.delete(sessionId);
  }

  /** Discard a pending nudge without acting on it. */
  clearQueued(sessionId: string): void {
    this.#queued.delete(sessionId);
  }
}

/** The process-wide registry. Injected everywhere; this is the production instance. */
export const turns: TurnRegistry = new TurnRegistry();

// ---------------------------------------------------------------------------
// The derived queue
// ---------------------------------------------------------------------------

/**
 * Does this session hold input nothing has answered yet?
 *
 * True when a `user` or `system` message lands after the session's last
 * `supervisor` message — which is exactly the shape a mid-turn post leaves, because
 * the supervisor placeholder is created *before* the queued message arrives and
 * messages order by `(created_at, rowid)`.
 *
 * Scoped to the session's OWN messages, not the inherited thread: an ancestor's
 * trailing user message was answered on the branch that owns it, and treating it as
 * unanswered here would make every fresh fork start a turn nobody asked for.
 *
 * This terminates. The drained turn appends its own supervisor message, so the next
 * check finds nothing after it — a turn that produces no answer at all is the one
 * case that could loop, and it cannot happen: the runner always closes its
 * supervisor message, on every exit path including a failure.
 */
export function hasUnansweredInput(db: Db, sessionId: string): boolean {
  const own = db.messagesFor(sessionId);
  for (let i = own.length - 1; i >= 0; i--) {
    const role = own[i].role;
    if (role === "supervisor") return false;
    if (role === "user" || role === "system") return true;
  }
  return false;
}

/**
 * Should a fresh turn start now that one has ended? The nudge is taken either way,
 * so a caller that decides not to drain does not leave it armed for later.
 */
export function shouldDrain(db: Db, sessionId: string, registry: TurnRegistry): boolean {
  const nudged = registry.drain(sessionId);
  return nudged || hasUnansweredInput(db, sessionId);
}

// ---------------------------------------------------------------------------
// The retry ring
// ---------------------------------------------------------------------------

/**
 * Re-attempts of one round, above whatever the provider client already does
 * internally. Two is enough to ride out a multi-minute network flap while a truly
 * dead network still fails the turn in minutes rather than hanging.
 */
export const MAX_ROUND_RETRIES = 2;

/**
 * How long to wait before re-attempting a round the provider could not deliver.
 * The client's own backoff has already spent ~30s by the time a failure reaches
 * here, so this is the "wait for the network to come back" tier.
 */
export const OUTAGE_DELAY_MS = 60_000;

/**
 * A tool call whose input never finished arriving.
 *
 * `llm/stream.ts` raises this rather than falling back to `{}`, and it is the
 * failure this ring exists for: the round's *content* was fine, the transport cut
 * it. Re-streaming immediately almost always lands it intact, and waiting a minute
 * for a connection that is not broken would be a minute of the user watching
 * nothing happen.
 */
export function isTruncatedToolCall(err: unknown): boolean {
  return err instanceof LlmError && /truncated mid-call/i.test(err.message);
}

/** True when the failure is the user's own stop, which is never retried. */
export function isAbort(err: unknown): boolean {
  const name = errName(err);
  return name === "AbortError" || name === "APIUserAbortError";
}

/** What to do about a round that failed. */
export interface RetryDecision {
  retry: boolean;
  /** Milliseconds to wait first. Zero for a truncation. */
  delayMs: number;
  /** One short line for `message.retry`, shown to the user as-is. */
  reason: string;
}

/**
 * Classify a failed round.
 *
 * `attempt` is 1-based and counts attempts already made. Aborts stop immediately —
 * a user interrupt is an answer, not an error — and so does anything the provider
 * layer classes as the caller's own mistake, because retrying a bad request six
 * times only delays the message that explains it.
 */
export function classifyRoundFailure(
  err: unknown,
  attempt: number,
  opts: { maxRetries?: number; outageDelayMs?: number } = {},
): RetryDecision {
  const maxRetries = opts.maxRetries ?? MAX_ROUND_RETRIES;
  const truncated = isTruncatedToolCall(err);
  const reason = truncated
    ? "the model's tool call was cut off mid-stream — re-running the round rather than " +
      "executing a truncated program"
    : shortReason(err);

  if (isAbort(err) || attempt > maxRetries || !(truncated || isRetryable(err))) {
    return { retry: false, delayMs: 0, reason };
  }
  return {
    retry: true,
    delayMs: truncated ? 0 : (opts.outageDelayMs ?? OUTAGE_DELAY_MS),
    reason,
  };
}

/** One line, no newlines, bounded — this goes straight into an event payload. */
export function shortReason(err: unknown, max = 120): string {
  const raw = (err as Error)?.message ?? String(err);
  const flat = raw.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/**
 * Sleep that a stop cuts short. Rejects with an `AbortError` when interrupted, so
 * the caller unwinds the turn instead of resuming a round the user cancelled — a
 * plain `setTimeout` here would make the stop button feel broken for a minute.
 */
export function abortableDelay(ms: number, signal?: AbortSignal): Promise<void> {
  if (ms <= 0) return signal?.aborted ? Promise.reject(abortError()) : Promise.resolve();
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(abortError());
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortError());
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function abortError(): DOMException {
  return new DOMException("interrupted while waiting to retry", "AbortError");
}
