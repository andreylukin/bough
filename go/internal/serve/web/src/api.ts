// The one place that knows the wire. Everything else takes typed values.
import type { Ask, Event, Line, Row } from "./types";

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: init?.body ? { "content-type": "application/json" } : undefined,
  });
  if (!res.ok) {
    // The API answers errors as {"error": "..."} — surface that text,
    // not a bare status code, so the UI can say what actually failed.
    let detail = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) detail = body.error;
    } catch {
      /* a non-JSON error body is not worth masking the status */
    }
    throw new Error(detail);
  }
  return (await res.json()) as T;
}

const post = (path: string, body?: unknown) =>
  req<{ ok: true }>(path, { method: "POST", body: body ? JSON.stringify(body) : undefined });

export const api = {
  sessions: (all = false) =>
    req<{ sessions: Row[] }>(`/api/sessions${all ? "?all=1" : ""}`).then((r) => r.sessions),

  /**
   * A session and its transcript. `since` asks for entries after a
   * history seq, which is how the live view catches up without
   * refetching the whole thread.
   */
  session: (id: string, since = 0) =>
    req<{ session: Row; entries: Line[] }>(
      `/api/sessions/${id}${since > 0 ? `?since=${since}` : ""}`,
    ),

  create: (cwd: string, prompt: string) =>
    req<{ session: Row }>("/api/sessions", {
      method: "POST",
      body: JSON.stringify({ cwd, prompt }),
    }).then((r) => r.session),

  prompt: (id: string, text: string) => post(`/api/sessions/${id}/prompt`, { text }),
  answer: (id: string, text: string) => post(`/api/sessions/${id}/answer`, { text }),
  interrupt: (id: string) => post(`/api/sessions/${id}/interrupt`),
  rename: (id: string, title: string) => post(`/api/sessions/${id}/rename`, { title }),
  archive: (id: string) => post(`/api/sessions/${id}/archive`),
  unarchive: (id: string) => post(`/api/sessions/${id}/unarchive`),
};

/**
 * Subscribe to a session's live events.
 *
 * The server deliberately sends unnamed frames, so one `message`
 * handler sees every kind — including kinds added after this code was
 * written. Returns an unsubscribe.
 */
export function subscribe(id: string, onEvent: (ev: Event) => void): () => void {
  const src = new EventSource(`/api/sessions/${id}/events`);
  src.onmessage = (m) => {
    try {
      onEvent(JSON.parse(m.data) as Event);
    } catch {
      /* a malformed frame is dropped, never fatal to the stream */
    }
  };
  return () => src.close();
}

export type { Ask, Event, Line, Row };
