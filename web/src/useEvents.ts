// Single event stream from /events, shared app-wide. Consumers register a handler and
// receive every envelope; routing/reduction lives in the store.
//
// This is SSE over fetch(POST), not EventSource: Cloudflare quick tunnels buffer GET
// event-streams until the connection closes (cloudflared#1449) but stream POST bodies
// live, and phone-over-tunnel is a supported way to use bough. The cost is hand-rolled
// frame parsing and reconnect, both below.
//
// Events that fire while the stream is down are gone (there's no replay), so `onResync`
// tells the store to refetch state whenever a gap is possible: after a reconnect, and
// when the tab becomes visible again (mobile browsers freeze background tabs without
// reliably erroring the stream first).
import { useEffect, useRef, useState } from "react";
import type { BoughEvent } from "./types";

const KNOWN_TYPES = new Set([
  "session.created",
  "session.updated",
  "session.archived",
  "message.started",
  "message.delta",
  "message.part",
  "message.finished",
  "net.request",
  "changes.updated",
  "worker.step",
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
          const res = await fetch("/events", { method: "POST", signal: abort.signal });
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

    const onVisible = () => {
      if (document.visibilityState === "visible") resync.current?.();
    };
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      abort.abort();
    };
  }, []);

  return connected;
}
