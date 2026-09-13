// The one place that knows the wire. Everything else takes typed values.
import type { Ask, Event, Line, Project, Row } from "./types";

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

export interface Change { path: string; add: number; del: number; new?: boolean }
/** One line of a session's running log, written by the small model per turn. */
export interface TurnLine { turn: number; text: string; at: string }

export const api = {
  /** The session's running log, one caveman line per finished turn. */
  turns: (id: string) => req<{ turns: TurnLine[] }>(`/api/sessions/${id}/turns`).then((r) => r.turns ?? []),
  /** What is uncommitted in the session's working tree, live from git. */
  changes: (id: string) => req<{ repo: boolean; files: Change[] }>(`/api/sessions/${id}/changes`),
  /** One file's unified diff against HEAD, 3 lines of context. */
  diff: (id: string, path: string) => req<{ diff: string }>(`/api/sessions/${id}/diff?path=${encodeURIComponent(path)}`).then((r) => r.diff),
  /** Ask the session to stop one of its background jobs. */
  killJob: (id: string, job: number) => post(`/api/sessions/${id}/jobs/${job}/kill`),
  /** Mark what a session has recorded so far as seen, taking it out of Needs you. */
  ack: (id: string) => post(`/api/sessions/${id}/ack`),

  /** Where a new session starts, from the server that knows. */
  home: () => req<{ home: string }>("/api/health").then((r) => r.home ?? ""),

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
  /** Save a pasted image on the server; the path is what a prompt references. */
  attach: async (img: Blob) => {
    const res = await fetch("/api/attachments", { method: "POST", headers: { "content-type": img.type }, body: img });
    const body = (await res.json().catch(() => ({}))) as { path?: string; error?: string };
    if (!res.ok || !body.path) throw new Error(body.error ?? `${res.status} ${res.statusText}`);
    return body.path;
  },
  attachmentURL: (path: string) => `/api/attachments?path=${encodeURIComponent(path)}`,
  answer: (id: string, text: string, ask?: string) => post(`/api/sessions/${id}/answer`, { text, ask }),
  interrupt: (id: string) => post(`/api/sessions/${id}/interrupt`),
  rename: (id: string, title: string) => post(`/api/sessions/${id}/rename`, { title }),
  archive: (id: string) => post(`/api/sessions/${id}/archive`),
  model: (id: string, model: string) => post(`/api/sessions/${id}/model`, { model }),
  effort: (id: string, effort: string) => post(`/api/sessions/${id}/effort`, { effort }),
  assign: (id: string, project: string) => post(`/api/sessions/${id}/project`, { project }),

  projects: () => req<{ projects: Project[] }>("/api/projects").then((r) => r.projects ?? []),
  newProject: (name: string) =>
    req<{ project: Project }>("/api/projects", { method: "POST", body: JSON.stringify({ name }) })
      .then((r) => r.project),
  renameProject: (id: string, name: string) => post(`/api/projects/${id}/rename`, { name }),
  deleteProject: (id: string) => req<{ ok: true }>(`/api/projects/${id}`, { method: "DELETE" }),
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

export type { Ask, Event, Line, Project, Row };
