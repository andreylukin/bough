/**
 * In-process event bus. One instance fans every BoughEvent out to all live SSE
 * subscribers. It is deliberately memory-only and persistence-agnostic: the bus
 * stamps a process-monotonic `seq` and a `ts`, then notifies listeners — persisting
 * the event (to messages, …) is the caller's job, done before publish.
 *
 * Contract:
 *   - publish(e) takes the event minus its stamp fields, assigns `seq`/`ts`, delivers
 *     it synchronously to every subscriber, and returns the stamped event.
 *   - subscribe(fn) returns an unsubscribe thunk; a listener that throws is isolated
 *     (logged, not allowed to break the fan-out to the rest).
 */
import type { BoughEvent } from "./schema/parts.ts";

/** An event as published — everything except the bus-assigned `seq`/`ts`. */
export type EventInput = Omit<BoughEvent, "seq" | "ts">;

export type Listener = (event: BoughEvent) => void;

export class Bus {
  #seq = 0;
  #listeners = new Set<Listener>();

  publish(e: EventInput): BoughEvent {
    const event: BoughEvent = { ...e, seq: ++this.#seq, ts: Date.now() };
    for (const fn of this.#listeners) {
      try {
        fn(event);
      } catch (err) {
        console.error("bus listener threw:", err);
      }
    }
    return event;
  }

  subscribe(fn: Listener): () => void {
    this.#listeners.add(fn);
    return () => this.#listeners.delete(fn);
  }

  get size(): number {
    return this.#listeners.size;
  }
}

/** The app-wide bus. Tests construct their own `new Bus()` for isolation. */
export const bus = new Bus();
