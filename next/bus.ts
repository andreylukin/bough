/**
 * In-process event fan-out. One `Bus` instance stamps every event and hands it to
 * every live subscriber — the SSE endpoint, and nothing else that matters.
 *
 * The invariant: **one bad subscriber cannot silence the others.** A listener that
 * throws is caught, reported, and stepped over; fan-out continues down the set
 * (plan §6.6). This is not defensive habit. Each subscriber is a client's SSE
 * connection, and a connection that dies between the socket closing and the
 * unsubscribe running will throw on enqueue. Letting that propagate would mean one
 * detached TUI stops every other client's stream mid-turn — the exact failure the
 * "a client can crash or detach without affecting a running turn" principle
 * (spec §2.5) forbids.
 *
 * Two consequences of that, both deliberate:
 *
 *   - `publish` never throws for a listener's reason, so callers do not wrap it and
 *     no emit path acquires an error branch that would need testing.
 *   - `publish` still returns the stamped event, because the emitter frequently
 *     wants the `seq` it just assigned.
 *
 * Second invariant: **the bus is display transport, never storage.** It holds no
 * history and replays nothing. `seq` is a process-monotonic counter that resets on
 * restart, so it is a dedupe key and not a resume cursor (plan §6.16) — a
 * reconnecting client re-fetches the session and reconciles by message id. Persist
 * first, then publish; an event published before its row is committed is a client
 * that can see something the database does not have.
 *
 * There is no module-level singleton. The bus travels in `AppCtx` like the database
 * and the clock (plan §0, dependency injection over globals), which is also what
 * lets a test construct one, count subscribers, and throw it away.
 */
import type { BoughEvent, EventInput } from "./schema/events.ts";
import type { Bus as BusPort } from "./types.ts";

/** A subscriber. Called synchronously, in subscription order, for every event. */
export type Listener = (event: BoughEvent) => void;

/** Injected seams. Both default to the real thing; tests supply their own. */
export interface BusOptions {
  /**
   * The clock used for `ts`. Injected so a test can assert stamping without
   * sleeping; production passes nothing and gets `Date.now`.
   */
  now?: () => number;
  /**
   * Where a throwing listener's error goes. The default logs it. A test passes a
   * collector so an *expected* throw does not print scary output, and so the
   * isolation itself can be asserted rather than inferred.
   */
  onListenerError?: (error: unknown, event: BoughEvent) => void;
}

export class Bus implements BusPort {
  #seq = 0;
  #listeners = new Set<Listener>();
  readonly #now: () => number;
  readonly #onListenerError: (error: unknown, event: BoughEvent) => void;

  constructor(options: BusOptions = {}) {
    this.#now = options.now ?? Date.now;
    this.#onListenerError = options.onListenerError ??
      ((err) => console.error("bus listener threw:", err));
  }

  /**
   * Stamp `seq`/`ts`, deliver synchronously to every subscriber, return the stamped
   * event.
   *
   * Delivery is synchronous on purpose: the caller has already persisted, so there
   * is nothing to await, and a microtask hop would let two emits interleave and
   * arrive out of `seq` order at a client.
   *
   * The input is not mutated — the stamped event is a fresh object, so a caller may
   * reuse the payload it passed in.
   *
   * Iteration is over the live set rather than a snapshot, which means a listener
   * unsubscribed *during* this fan-out (by an earlier listener, or by itself) does
   * not receive this event. That is the safe direction: an unsubscribe is a closed
   * connection, and delivering to it would only produce an error to swallow.
   */
  publish<E extends EventInput>(e: E): E & { seq: number; ts: number } {
    const event = { ...e, seq: ++this.#seq, ts: this.#now() };
    for (const fn of this.#listeners) {
      try {
        fn(event);
      } catch (err) {
        // Isolated, not rethrown: the remaining listeners still get this event.
        try {
          this.#onListenerError(err, event);
        } catch {
          // A reporter that throws is not allowed to break fan-out either.
        }
      }
    }
    return event;
  }

  /**
   * Register a listener; the returned thunk removes it.
   *
   * Unsubscribing is idempotent and safe to call from inside a listener. The thunk
   * is the only way to detach, so an SSE handler's cancel path cannot leak a
   * subscriber by forgetting a key.
   */
  subscribe(fn: Listener): () => void {
    this.#listeners.add(fn);
    return () => {
      this.#listeners.delete(fn);
    };
  }

  /** Live subscriber count. The SSE leak check (T1.3) asserts this returns to 0. */
  get size(): number {
    return this.#listeners.size;
  }
}
