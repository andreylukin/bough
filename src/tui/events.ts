// SSE client over plain GET /events. No visibilitychange resync (a terminal
// never backgrounds the fetch) — resync fires on reconnect only.
import { useEffect, useRef, useState } from "react";
import type { BoughEvent } from "../schema/parts.ts";
import { authHeaders, BASE } from "./api.ts";

const KNOWN_TYPES = new Set([
  "session.created",
  "session.updated",
  "session.archived",
  "message.started",
  "message.delta",
  "message.part",
  "message.finished",
  "turn.finished",
  "changes.updated",
  "usage.updated",
  "worker.step",
  "net.request",
  "session.activity",
]);

const RETRY_MS = 2000;

/** Parse complete SSE frames out of `buffer`, returning the unconsumed tail. */
function drainFrames(buffer: string, emit: (type: string, data: string) => void): string {
  let at = 0;
  for (;;) {
    const end = buffer.indexOf("\n\n", at);
    if (end < 0) return buffer.slice(at);
    let type = "";
    let data = "";
    for (const line of buffer.slice(at, end).split("\n")) {
      if (line.startsWith("event: ")) type = line.slice(7);
      else if (line.startsWith("data: ")) data += line.slice(6);
      // lines starting with ":" are heartbeats/comments — ignored
    }
    if (type && data) emit(type, data);
    at = end + 2;
  }
}

export function useEvents(onEvent: (ev: BoughEvent) => void, onResync?: () => void) {
  const handler = useRef(onEvent);
  handler.current = onEvent;
  const resync = useRef(onResync);
  resync.current = onResync;
  const [connected, setConnected] = useState(false);

  useEffect(() => {
    const abort = new AbortController();
    let dropped = false; // only resync on RE-connect; the initial open has fresh state

    (async () => {
      while (!abort.signal.aborted) {
        try {
          const res = await fetch(`${BASE}/events`, {
            signal: abort.signal,
            headers: authHeaders(),
          });
          if (!res.ok || !res.body) throw new Error(`events: ${res.status}`);
          setConnected(true);
          if (dropped) resync.current?.();
          dropped = false;

          const reader = res.body.getReader();
          const dec = new TextDecoder();
          let buffer = "";
          for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            buffer += dec.decode(value, { stream: true });
            buffer = drainFrames(buffer, (type, data) => {
              if (!KNOWN_TYPES.has(type)) return;
              try {
                handler.current(JSON.parse(data) as BoughEvent);
              } catch {
                /* ignore malformed frame */
              }
            });
          }
        } catch {
          /* network error / server gone — retry below */
        }
        if (abort.signal.aborted) break;
        setConnected(false);
        dropped = true;
        await new Promise((r) => setTimeout(r, RETRY_MS));
      }
    })();

    return () => abort.abort();
  }, []);

  return connected;
}
