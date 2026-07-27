/**
 * `GET /events[?sessionId=]` — the SSE stream every client watches.
 *
 * The invariant this file holds is the one from spec §3 and plan §6.16: **`seq` is
 * a dedupe key, not a resume cursor.** It is process-monotonic and resets on server
 * restart, so there is nothing here that replays, buffers, or accepts a cursor. A
 * reconnecting client re-fetches `GET /sessions/:id` and reconciles by message id;
 * the database is the source of truth and this stream is display transport. That is
 * also why no frame carries an SSE `id:` field: `id:` is precisely the resume
 * mechanism (the client echoes it back as `Last-Event-ID`), and emitting one would
 * advertise a guarantee this server cannot keep across a restart. Any state a client
 * cannot rebuild from a fresh fetch is a bug in the event design, not something to
 * fix here.
 *
 * Second invariant: **a connection that goes away releases its bus subscription,
 * always.** Every subscriber is a closure holding an encoder and a stream
 * controller; a leaked one is delivered to for the life of the process, and — since
 * the bus fans out synchronously on the emitting path — a pile of them is paid for
 * by every turn, forever. Teardown therefore runs from three independent triggers:
 * the stream's `cancel()` (the normal path), the request's abort signal (the client
 * vanished), and a failed `enqueue` (the stream errored, in which case `cancel()` is
 * never called at all). `teardown` is idempotent, so all three firing is fine and
 * exactly none of them firing is impossible.
 *
 * Third, the filtering rule, which is easy to get subtly wrong: `?sessionId=` scopes
 * the stream to one session, but **an event with no `sessionId` is global and always
 * delivered.** Filtering only ever drops session-scoped events belonging to another
 * session. A filtered client that silently lost the un-scoped events would be
 * missing exactly the announcements it has no other way to learn about, and with no
 * replay it would never recover them.
 *
 * Note what filtering deliberately does NOT do: it does not resolve lineage. A
 * subagent's events are published under the subagent's own session id and do not
 * reach a stream filtered on its spawner. That is correct — a subagent's report
 * reaches the spawner as a system note published under the *spawner's* id — and it
 * keeps this handler free of database access, which is what lets it be tested with a
 * bus and nothing else.
 *
 * Everything injectable is injected (plan §0): the bus arrives in `AppCtx`, and the
 * heartbeat's timer functions are a parameter, so the heartbeat test fires a tick by
 * hand instead of waiting fifteen seconds.
 */
import type { BoughEvent } from "../schema/events.ts";
import type { Handler } from "./http.ts";

/**
 * How often a comment line is written to keep the connection warm. Long enough to
 * be negligible, short enough to beat any intermediary's idle timeout — though on
 * loopback its real job is to notice a dead peer, since a write to a closed socket
 * is what surfaces the disconnect the abort signal may not have reported yet.
 */
export const HEARTBEAT_MS = 15_000;

/**
 * The two timer functions, injected. Production passes nothing and gets the
 * globals; a test passes a fake so it can fire a heartbeat synchronously and assert
 * the interval was cleared, rather than sleeping and hoping.
 */
export interface Timers {
  setInterval(callback: () => void, ms: number): TimerHandle;
  clearInterval(handle: TimerHandle): void;
}

/**
 * Opaque on purpose. `setInterval` returns a `number` under Deno's own lib and a
 * `Timeout` object once node types are in scope, and this module has no business
 * caring which — it only ever hands the value straight back to `clearInterval`.
 */
export type TimerHandle = unknown;

const REAL_TIMERS: Timers = {
  setInterval: (callback, ms) => setInterval(callback, ms),
  clearInterval: (handle) => clearInterval(handle as number),
};

export interface EventsOptions {
  /** Heartbeat period. `0` disables it entirely — used by tests that count frames. */
  heartbeatMs?: number;
  timers?: Timers;
  /**
   * Where a failure that is not the caller's business is reported.
   *
   * `serialize` is a defect: an event whose `data` cannot be JSON-encoded. It is
   * logged and that one event is skipped — one malformed payload must not take
   * down a connection that is otherwise fine.
   *
   * `enqueue` is ordinary: the peer closed between the socket dying and the
   * teardown running. It is silent by default because it is not an error, and it
   * is surfaced here only so a test can assert the teardown it triggers.
   */
  onStreamError?: (error: unknown, info: { phase: "serialize" | "enqueue" }) => void;
}

/**
 * Does `event` reach a stream opened with `filter`?
 *
 * Pure, and exported so the rule is testable without a stream: no filter passes
 * everything, and an event with no `sessionId` is global and passes regardless.
 */
export function passesFilter(event: { sessionId?: string }, filter: string | null): boolean {
  if (!filter) return true;
  if (event.sessionId === undefined) return true;
  return event.sessionId === filter;
}

/**
 * One named SSE frame.
 *
 * `event:` carries the type because clients attach one listener per event name, and
 * the full stamped envelope — `seq` and `ts` included — is the `data:` payload. A
 * single `data:` line is safe for any payload: `JSON.stringify` escapes every
 * newline, and an embedded raw newline is the one thing that would split a frame.
 *
 * Throws if `event.data` is not JSON-encodable; the caller skips that event.
 */
export function frame(event: BoughEvent): string {
  return `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
}

/** The comment line written once the subscription is live, so a client sees bytes immediately. */
export const CONNECTED_FRAME = ": connected\n\n";
/** The keep-alive. A comment, so every SSE client ignores it without a case for it. */
export const HEARTBEAT_FRAME = ": ping\n\n";

/**
 * Build the `/events` handler. `events` below is the production instance; a test
 * builds its own to inject timers or disable the heartbeat.
 */
export function createEventsHandler(options: EventsOptions = {}): Handler {
  const heartbeatMs = options.heartbeatMs ?? HEARTBEAT_MS;
  const timers = options.timers ?? REAL_TIMERS;
  const onStreamError = options.onStreamError ?? ((error, info) => {
    // A dead peer is not news; an unencodable payload is.
    if (info.phase === "serialize") console.error("events: undeliverable event:", error);
  });

  return (req, ctx) => {
    const filter = new URL(req.url).searchParams.get("sessionId");
    const encoder = new TextEncoder();

    let unsubscribe: (() => void) | undefined;
    let heartbeat: TimerHandle | undefined;
    let closed = false;

    /**
     * Release everything this connection holds. Idempotent by design: it is reached
     * from `cancel()`, from the abort signal, and from a failed write, and which of
     * those wins is a race no caller should have to think about.
     */
    const teardown = () => {
      if (closed) return;
      closed = true;
      unsubscribe?.();
      unsubscribe = undefined;
      if (heartbeat !== undefined) timers.clearInterval(heartbeat);
      heartbeat = undefined;
    };

    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        // The client may already be gone by the time the body starts.
        if (req.signal.aborted) {
          closed = true;
          return;
        }
        // The client vanished. Release, then close the stream so anything still
        // holding the body observes end-of-stream instead of waiting forever.
        req.signal.addEventListener("abort", () => {
          teardown();
          try {
            controller.close();
          } catch {
            // Already closed or errored — the release above is what mattered.
          }
        }, { once: true });

        const push = (text: string): void => {
          if (closed) return;
          try {
            controller.enqueue(encoder.encode(text));
          } catch (error) {
            // An errored or closed stream never calls `cancel()`, so this is the
            // only place the subscription would otherwise be released from.
            onStreamError(error, { phase: "enqueue" });
            teardown();
          }
        };

        push(CONNECTED_FRAME);
        if (closed) return; // the write failed; nothing to subscribe on behalf of

        unsubscribe = ctx.bus.subscribe((event) => {
          if (!passesFilter(event, filter)) return;
          let text: string;
          try {
            text = frame(event);
          } catch (error) {
            // Skip the event, keep the connection. Reported, never swallowed.
            onStreamError(error, { phase: "serialize" });
            return;
          }
          push(text);
        });

        if (heartbeatMs > 0) {
          heartbeat = timers.setInterval(() => push(HEARTBEAT_FRAME), heartbeatMs);
        }
      },
      cancel() {
        teardown();
      },
    });

    return new Response(stream, {
      headers: {
        "content-type": "text/event-stream",
        // No caching and no buffering: a proxy that holds frames back turns a live
        // stream into a batch delivered at close.
        "cache-control": "no-cache, no-transform",
        connection: "keep-alive",
      },
    });
  };
}

/** The production handler, wired into the route table in `app.ts`. */
export const events: Handler = createEventsHandler();
