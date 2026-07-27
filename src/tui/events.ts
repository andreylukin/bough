/**
 * The SSE subscription: `GET /events`, forever, across server restarts.
 *
 * THE INVARIANT THIS HOLDS, and it is the whole design: **`seq` is a dedupe key, not
 * a resume cursor** (spec §3, plan §6.16). There is therefore nothing here that
 * resumes. No `Last-Event-ID` header is sent — the server deliberately emits no `id:`
 * field for exactly this reason (`server/events.ts`) — no seq is remembered across a
 * reconnect, and no frame is ever asked for again. What happens on RE-connect instead
 * is that `onReconnect` fires and the caller re-fetches `GET /sessions/:id` and
 * reconciles by message id (`store.ts`). The database is the source of truth; this
 * stream is display transport. Any state a client cannot rebuild from a fresh fetch is
 * a bug in the event design, not something to fix with replay.
 *
 * Second invariant: **the known-type list is the schema's, never a local copy.** The
 * drain loop skips frames it does not recognize, so a locally-maintained list that
 * missed a new event name silently dropped it — that is how `tool.log` shipped in the
 * old tree with live program output rendering nowhere while the backend streamed
 * perfectly. `EVENT_TYPES` is frozen in `schema/events.ts`; importing it makes drift
 * impossible rather than merely unlikely.
 *
 * Third: **no React, no terminal, no globals.** This is a plain function returning a
 * handle. `fetch`, the retry delay and the URL are all injected, so the reconnect
 * behaviour is testable against a loopback server that is stopped and restarted under
 * it, with nothing mounted and nothing on the network.
 *
 * The envelope IS parsed with the Zod schema, unlike the rest of the wire types: this
 * is the one place bytes come off a socket, and a malformed frame must be skipped
 * rather than reduced into state.
 */
import { BoughEvent, EVENT_TYPES } from "../schema/events.ts";

/** The closed set, straight from the frozen schema. Frames outside it are ignored. */
export const KNOWN_EVENT_TYPES: ReadonlySet<string> = new Set<string>(EVENT_TYPES);

/** How long to wait before redialing a dropped or refused connection. */
export const RETRY_MS = 2_000;

/**
 * Parse whole SSE frames out of `buffer` and return the unconsumed tail.
 *
 * Pure and exported so framing is testable on strings — including the cases that
 * matter and are easy to get wrong: a frame split across two chunk boundaries, and a
 * comment line (`: ping`, `: connected`), which carries no `event:` and must be
 * skipped without disturbing the buffer.
 */
export function parseFrames(
  buffer: string,
  emit: (type: string, data: string) => void,
): string {
  let at = 0;
  for (;;) {
    const end = buffer.indexOf("\n\n", at);
    if (end < 0) return buffer.slice(at);
    let type = "";
    let data = "";
    for (const line of buffer.slice(at, end).split("\n")) {
      if (line.startsWith("event:")) type = line.slice(6).trim();
      // A multi-line `data:` is concatenated, per the SSE grammar. The server writes
      // one line (JSON escapes every newline), but the parser must not depend on that.
      else if (line.startsWith("data:")) data += line.slice(5).replace(/^ /, "");
      // Anything else — a comment (`:`), a `retry:` — is not ours to interpret.
    }
    if (type && data) emit(type, data);
    at = end + 2;
  }
}

export interface EventStreamOptions {
  /** The stream URL. Absent = built from `base` and `sessionId`. */
  url?: string;
  /** Origin, when `url` is not given. Absent = `api.ts`'s `defaultBase()`. */
  base?: string;
  /** Scope the stream to one session. Un-scoped events are delivered regardless. */
  sessionId?: string;
  /** Every well-formed, known-type event, in arrival order. */
  onEvent: (event: BoughEvent) => void;
  /**
   * The stream came up. `reconnect` is false for the very first open (the caller's
   * state is already fresh) and true afterwards — which is the signal to RE-FETCH,
   * because whatever was published while the connection was down is gone for good.
   */
  onOpen?: (info: { reconnect: boolean; attempt: number }) => void;
  /** The stream went down. Always followed by a retry unless the handle was closed. */
  onClose?: (info: { error: unknown }) => void;
  /**
   * A frame that could not be parsed as an event envelope. Skipped, never fatal —
   * reported so a schema drift is visible instead of silently dropping display data.
   */
  onBadFrame?: (info: { type: string; data: string; error?: unknown }) => void;
  retryMs?: number;
  fetchFn?: typeof fetch;
  /** Injected so a test does not wait two seconds for a redial. */
  delay?: (ms: number, signal: AbortSignal) => Promise<void>;
}

export interface EventStream {
  /** Is a connection live right now? Drives the TUI's disconnected indicator. */
  readonly connected: boolean;
  /** How many times the stream has come up. 1 = the first open. */
  readonly opens: number;
  /** Stop reconnecting and release the connection. Idempotent. */
  close(): void;
  /** Resolves once the loop has stopped. Tests await it; the TUI ignores it. */
  readonly done: Promise<void>;
}

/** Abortable sleep. Resolves early — not rejects — when the handle is closed. */
function defaultDelay(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener("abort", () => {
      clearTimeout(timer);
      resolve();
    }, { once: true });
  });
}

/**
 * Open the stream and keep it open.
 *
 * The loop is deliberately shaped so that "connected" is only true while bytes can
 * actually arrive: it is set after the response headers land and cleared on every exit
 * path, including the one where the server closed the body cleanly. A client that
 * showed "connected" while its reader was at end-of-stream would hide precisely the
 * outage the reconnect fetch exists to repair.
 */
export function connectEvents(options: EventStreamOptions): EventStream {
  const retryMs = options.retryMs ?? RETRY_MS;
  const doFetch = options.fetchFn ?? globalThis.fetch;
  const delay = options.delay ?? defaultDelay;
  const url = options.url ??
    `${options.base ?? ""}/events${
      options.sessionId ? `?sessionId=${encodeURIComponent(options.sessionId)}` : ""
    }`;

  const abort = new AbortController();
  const state = { connected: false, opens: 0 };

  const done = (async () => {
    while (!abort.signal.aborted) {
      let error: unknown = null;
      try {
        const res = await doFetch(url, {
          signal: abort.signal,
          headers: { accept: "text/event-stream" },
        });
        if (!res.ok || !res.body) {
          // Drain the body so the connection can be reused rather than leaked.
          await res.body?.cancel().catch(() => {});
          throw new Error(`GET ${url}: ${res.status}`);
        }

        state.connected = true;
        state.opens++;
        options.onOpen?.({ reconnect: state.opens > 1, attempt: state.opens });

        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        for (;;) {
          const { done: finished, value } = await reader.read();
          if (finished) break;
          buffer += decoder.decode(value, { stream: true });
          buffer = parseFrames(buffer, (type, data) => {
            if (!KNOWN_EVENT_TYPES.has(type)) {
              options.onBadFrame?.({ type, data });
              return;
            }
            let payload: unknown;
            try {
              payload = JSON.parse(data);
            } catch (err) {
              options.onBadFrame?.({ type, data, error: err });
              return;
            }
            // The one place bytes become state. Parse the envelope, never trust it.
            const parsed = BoughEvent.safeParse(payload);
            if (!parsed.success) {
              options.onBadFrame?.({ type, data, error: parsed.error });
              return;
            }
            options.onEvent(parsed.data);
          });
        }
      } catch (err) {
        error = err;
      }

      if (state.connected) {
        state.connected = false;
        options.onClose?.({ error });
      }
      if (abort.signal.aborted) break;
      await delay(retryMs, abort.signal);
    }
    state.connected = false;
  })();

  return {
    get connected() {
      return state.connected;
    },
    get opens() {
      return state.opens;
    },
    close: () => abort.abort(),
    done,
  };
}
