import type { Event } from "./types";

type Message =
  | { kind: "ready"; owner: string }
  | { kind: "hello"; request: string }
  | { kind: "replay"; owner: string; request: string; events: Event[] }
  | { kind: "event"; owner: string; event: Event };

/** One stream per session across tabs; returns an unsubscribe. */
export function subscribe(id: string, onEvent: (ev: Event) => void): () => void {
  const deliver = (ev: Event) => {
    try { onEvent(ev); } catch { /* one listener must not break the stream */ }
  };
  const connect = (receive: (ev: Event) => void) => {
    const src = new EventSource(`/api/sessions/${id}/events`);
    // Frames are unnamed so new event kinds work without new listeners.
    src.onmessage = (m) => {
      try { receive(JSON.parse(m.data) as Event); } catch {
        /* a malformed frame is dropped, never fatal to the stream */
      }
    };
    return () => src.close();
  };
  if (typeof navigator === "undefined" || !navigator.locks || typeof BroadcastChannel === "undefined" ||
      typeof crypto === "undefined" || !crypto.randomUUID) {
    return connect(deliver);
  }

  const start = () => {
    const name = `bough-events:${id}`;
    let channel: BroadcastChannel;
    try { channel = new BroadcastChannel(name); } catch { return connect(deliver); }
    const pending = new AbortController();
    const owner = crypto.randomUUID();
    let request = "";
    let following = "";
    let leader = false;
    let stopped = false;
    let close: (() => void) | undefined;
    let release: (() => void) | undefined;
    let recent: Event[] = [];
    const post = (message: Message) => channel.postMessage(message);
    const hello = () => {
      following = "";
      request = crypto.randomUUID();
      post({ kind: "hello", request });
    };
    channel.onmessage = (m: MessageEvent<Message>) => {
      if (stopped) return;
      const message = m.data;
      if (message.kind === "hello" && leader) {
        post({ kind: "replay", owner, request: message.request, events: recent });
      } else if (message.kind === "ready" && !leader) {
        hello();
      } else if (message.kind === "replay" && !leader && message.request === request) {
        // A targeted replay precedes this owner's later live messages.
        // Other subscribers must not see it: deltas are not idempotent.
        following = message.owner;
        for (const ev of message.events) deliver(ev);
      } else if (message.kind === "event" && !leader && message.owner === following) {
        deliver(message.event);
      }
    };
    const fallback = () => {
      channel.close();
      if (!stopped) close = connect(deliver);
    };
    try {
      hello();
      // Six long-lived HTTP/1.1 connections block even a new tab's
      // document. Closing the owner hands its slot to the next waiter.
      void navigator.locks.request(name, { signal: pending.signal }, async () => {
        if (stopped) return;
        leader = true;
        const held = new Promise<void>((resolve) => { release = resolve; });
        post({ kind: "ready", owner });
        close = connect((event) => {
          if (event.seq > 0 && !recent.some((ev) => ev.seq === event.seq && ev.at === event.at)) {
            // Match serve's default 500-event ring, excluding ephemeral
            // deltas. Preserve live-only call starts/activity for late tabs.
            if (recent.length && event.seq <= recent[recent.length - 1].seq) recent = [];
            recent.push(event);
            if (recent.length > 500) recent.shift();
          }
          post({ kind: "event", owner, event });
          deliver(event); // BroadcastChannel does not echo to its sender.
        });
        await held;
      }).catch(fallback); // Present but disallowed APIs still get live updates.
    } catch { fallback(); }
    return () => {
      stopped = true;
      pending.abort(); // A departing follower must not later become owner.
      close?.();
      release?.();
      channel.close();
    };
  };
  let stop = start();
  let frozen = false;
  const freeze = () => { if (!frozen) { frozen = true; stop(); } };
  const resume = () => { if (frozen) { frozen = false; stop = start(); } };
  // A frozen background owner cannot forward events to visible tabs.
  // Rejoin with fresh resources when Chrome resumes this document.
  const lifecycle = typeof document === "undefined" ? undefined : document;
  lifecycle?.addEventListener("freeze", freeze);
  lifecycle?.addEventListener("resume", resume);
  return () => {
    lifecycle?.removeEventListener("freeze", freeze);
    lifecycle?.removeEventListener("resume", resume);
    stop();
  };
}
