// Thin REST client over the bough-next backend. Paths are proxied to :4321 in dev.
import type { BundleSummary, ChangeSource, Message, NetRequest, Session, WireDiff } from "./types";

async function j<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

export const api = {
  config: () => fetch("/config").then(j<{ model: string }>),

  listSessions: () => fetch("/sessions").then(j<Session[]>),

  // `title` optional: the backend creates the session as "untitled" and a title
  // worker names it from the first message (session.updated carries the rename).
  createSession: (body: {
    title?: string;
    parentId?: string | null;
    kind?: Session["kind"];
    workspace?: string;
  }) =>
    fetch("/sessions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then(j<Session>),

  getSession: (id: string) =>
    fetch(`/sessions/${id}`).then(j<{ session: Session; thread: Message[] }>),

  // Fire-and-forget: the turn streams back over /events.
  postMessage: (id: string, text: string) =>
    fetch(`/sessions/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ text }),
    }),

  // Stop the session's in-flight turn. Safe to call when nothing is running.
  interrupt: (id: string) => fetch(`/sessions/${id}/interrupt`, { method: "POST" }),

  // Soft-delete: the session leaves the sidebar; its thread and lineage remain.
  archiveSession: (id: string) => fetch(`/sessions/${id}/archive`, { method: "POST" }),

  // Workspace file search for the composer's @ autocomplete.
  searchFiles: (id: string, q: string) =>
    fetch(`/sessions/${id}/files?q=${encodeURIComponent(q)}`)
      .then(j<{ files: string[] }>)
      .then((r) => r.files),

  // ---- network rail --------------------------------------------------------
  // Backfill the feed; live updates arrive as `net.request` events over /events.
  netRequests: (sessionId?: string) =>
    fetch("/net/requests" + (sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : "")).then(
      j<NetRequest[]>
    ),

  // Resolve a held request; the gate re-emits it with the final verdict.
  allowRequest: (id: string) => fetch(`/net/requests/${id}/allow`, { method: "POST" }),
  denyRequest: (id: string) => fetch(`/net/requests/${id}/deny`, { method: "POST" }),

  // ---- policy bundles ------------------------------------------------------
  listBundles: () => fetch("/net/bundles").then(j<BundleSummary[]>),

  getBundle: (name: string) =>
    fetch(`/net/bundles/${encodeURIComponent(name)}`).then(j<BundleSummary & { fixtures: unknown[] }>),

  installBundle: (name: string, params: Record<string, unknown>) =>
    fetch(`/net/bundles/${encodeURIComponent(name)}/install`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ params }),
    }).then(j<unknown>),

  // ---- changes review ------------------------------------------------------
  // 0..2 diffs (jj repo + clonefile config). Refetched on the `changes.updated` event.
  getChanges: (sessionId: string) =>
    fetch(`/sessions/${sessionId}/changes`).then(j<{ diffs: WireDiff[] }>),

  // Apply reviewed files. clonefile copies originals back; jj apply is acceptance (no-op).
  applyChanges: (sessionId: string, source: ChangeSource, paths: string[]) =>
    fetch(`/sessions/${sessionId}/changes/apply`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source, paths }),
    }),

  // Whole-change undo (jj only; 400 if no jj workspace). `paths` accepted but ignored v1.
  revertChanges: (sessionId: string) =>
    fetch(`/sessions/${sessionId}/changes/revert`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({}),
    }),

  // ---- branching -----------------------------------------------------------
  // Fork at one of the session's OWN turns. With editedText → edit & resend (runs a
  // turn); without → a plain branch point. 400 (with the server message) for an
  // inherited turn or a non-user edit target.
  fork: (sessionId: string, body: { atMessageId: string; editedText?: string }) =>
    fetch(`/sessions/${sessionId}/fork`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),

  // Compact a span of the session's OWN turns onto a new summary branch. 400 for a span
  // that reaches into ancestor history.
  compact: (sessionId: string, body: { fromMessageId: string; toMessageId: string }) =>
    fetch(`/sessions/${sessionId}/compact`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
};

// Read {session} from a fork/compact response, or throw the server's error message so
// the UI can surface the "…the ancestor session instead" 400 gracefully.
export async function readBranch(res: Response): Promise<Session> {
  const bodyText = await res.text();
  let parsed: unknown;
  try {
    parsed = bodyText ? JSON.parse(bodyText) : {};
  } catch {
    parsed = {};
  }
  if (!res.ok) {
    const msg = (parsed as { error?: string }).error ?? `${res.status} ${res.statusText}`;
    throw new Error(msg);
  }
  return (parsed as { session: Session }).session;
}
