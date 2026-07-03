// Thin REST client over the bough backend. Paths are proxied to :4321 in dev.
import type { BundleSummary, ChangeSource, Message, NetRequest, Session, WireDiff } from "./types";

async function j<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

export interface ModelOption {
  id: string;
  label: string;
  provider: "anthropic" | "openrouter";
}
export interface Usage {
  contextTokens: number;
  outputTokens: number;
}
export interface SkillInfo {
  name: string;
  description: string;
}
export interface NetStatus {
  enabled: boolean;
  running: boolean;
  /** Live per-session proxy listeners (each branch gets its own port). */
  listeners: number;
  caPath: string;
}

/** Where a branch's effective rule set came from (mirrors src/net/config.ts). */
export interface PolicySource {
  scope: "session" | "inherited" | "global";
  sessionId?: string;
}

/** A loaded (or broken) policy extension (mirrors src/net/extensions.ts). */
export interface ExtensionInfo {
  name: string;
  file: string;
  error?: string;
}

// The editable rule set (mirrors src/net/config.ts NetConfig).
export type Verdict = "allow" | "deny" | "hold";
export interface NetConfig {
  mode: "read_only" | "review" | "all";
  allowHosts: string[];
  denyHosts: string[];
  hostMiss: Verdict;
  k8sHosts: string[];
  allowVerbs: string[];
  denyVerbs: string[];
  holdVerbs: string[];
}

export const api = {
  config: () => fetch("/config").then(j<{ model: string; models: ModelOption[] }>),

  // Switch the model new turns run on.
  setModel: (model: string) =>
    fetch("/config", {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model }),
    }).then(j<{ model: string }>),

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
    fetch(`/sessions/${id}`).then(j<{ session: Session; thread: Message[]; usage: Usage }>),

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

  // Installed skills (name + description) for the composer's / autocomplete.
  skills: () => fetch("/skills").then(j<{ skills: SkillInfo[] }>).then((r) => r.skills),

  // Workspace file search for the composer's @ autocomplete.
  searchFiles: (id: string, q: string) =>
    fetch(`/sessions/${id}/files?q=${encodeURIComponent(q)}`)
      .then(j<{ files: string[] }>)
      .then((r) => r.files),

  // ---- network: native Claw Patrol firewall --------------------------------
  // bough runs the egress proxy in-process; the live feed + human approvals + the
  // rule set all live here in bough's own UI.
  netStatus: () => fetch("/net/status").then(j<NetStatus>),

  // Backfill the feed; live updates arrive as `net.request` events over /events.
  netRequests: (sessionId?: string) =>
    fetch("/net/requests" + (sessionId ? `?sessionId=${encodeURIComponent(sessionId)}` : "")).then(
      j<NetRequest[]>,
    ),

  // Resolve a held request; the gate re-emits it with the final verdict.
  allowRequest: (id: string) => fetch(`/net/requests/${id}/allow`, { method: "POST" }),
  denyRequest: (id: string) => fetch(`/net/requests/${id}/deny`, { method: "POST" }),

  // The editable allow/deny/hold rule set. PUT hot-swaps the live gate.
  getPolicy: () => fetch("/net/policy").then(j<NetConfig>),
  putPolicy: (cfg: NetConfig) =>
    fetch("/net/policy", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(cfg),
    }).then(j<NetConfig>),

  // Branch-scoped rules: GET resolves the session's effective config + its source
  // (own override / inherited from an ancestor / global); PUT writes the branch's
  // override row; DELETE removes it so the branch inherits again.
  getSessionPolicy: (sessionId: string) =>
    fetch(`/net/policy?session=${encodeURIComponent(sessionId)}`)
      .then(j<{ config: NetConfig; source: PolicySource }>),
  putSessionPolicy: (sessionId: string, cfg: NetConfig) =>
    fetch(`/net/policy?session=${encodeURIComponent(sessionId)}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(cfg),
    }).then(j<{ config: NetConfig; source: PolicySource }>),
  deleteSessionPolicy: (sessionId: string) =>
    fetch(`/net/policy?session=${encodeURIComponent(sessionId)}`, { method: "DELETE" }),

  // ---- policy extensions (programmable guards) -----------------------------
  listExtensions: () =>
    fetch("/net/extensions").then(j<{ dir: string; extensions: ExtensionInfo[] }>),
  reloadExtensions: () =>
    fetch("/net/extensions/reload", { method: "POST" }).then(
      j<{ extensions: ExtensionInfo[] }>,
    ),
  createExtension: (name: string) =>
    fetch("/net/extensions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name }),
    }).then(j<{ path: string; extensions: ExtensionInfo[] }>),

  // AI-drafted rules: intent in, proposed config + rationale out. Nothing is
  // enforced until the user reviews the draft in the editor and saves it.
  // requestIds = feed rows to group into rules; prompt may be empty then (the
  // server supplies a sensible default grouping instruction).
  suggestPolicy: (prompt: string, sessionId?: string | null, requestIds?: string[]) =>
    fetch("/net/policy/suggest", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        prompt,
        sessionId: sessionId ?? undefined,
        requestIds: requestIds?.length ? requestIds : undefined,
      }),
    }).then(j<{ config: NetConfig; rationale: string }>),

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

  // Apply reviewed files. clonefile copies originals back; jj accepts the whole change
  // (seals it and advances the session bookmark, so the refetched diff comes back empty).
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

  // Adopt a subagent's branch: squash its changes into its spawner's workspace.
  // 400 (with the server message) for non-subagents or branches with no workspace.
  adopt: (sessionId: string) =>
    fetch(`/sessions/${sessionId}/adopt`, { method: "POST" }).then(j<{ message: string }>),

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
