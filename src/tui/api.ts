// REST client for the TUI. Unlike the retired web client (relative URLs behind a
// proxy), this talks to the server directly, so every call needs the absolute base.
import type { AskQuestion, Message, NetRequest, Session } from "../schema/parts.ts";

const PORT = Deno.env.get("BOUGH_PORT") ?? "4321";
export const BASE = `http://127.0.0.1:${PORT}`;

// Small response shapes, mirrored from the server routes (src/server/app.ts is the
// reference; these cover only what the TUI consumes).
export interface DirHit {
  path: string;
  display: string;
  repo: boolean;
}
export type ChangeSource = "clonefile" | "shadow";

/** An LLM-labeled topic section over the conversation tree's turns — the label
 * says what that stretch of work was about (inclusive 0-based turn indexes;
 * mirror of the server's Section). */
export interface WireSection {
  start: number;
  end: number;
  label: string;
}
export interface WireHunk {
  header: string;
  lines: string[];
}
export interface WireFileDiff {
  path: string;
  status: "added" | "modified" | "deleted";
  hunks: WireHunk[];
}
export interface WireDiff {
  source: ChangeSource;
  files: WireFileDiff[];
}
/** What POST changes/apply reports back — feeds the panel's feedback toast. */
export interface ApplyOutcome {
  applied: string[];
  /** The user's checkout files were delivered to (external mode), else null. */
  origin: string | null;
  branch: string | null;
  sealed: boolean;
}
export interface ModelOption {
  id: string;
  label: string;
}
export type KeyProvider = "anthropic" | "openrouter" | "openai";
export interface BoughConfig {
  model: string;
  models: ModelOption[];
  /** Thinking depth ("" = provider default) + the values the server accepts. */
  effort?: string;
  efforts?: string[];
  worker: string;
  workerOptions: ModelOption[];
  /** Which provider API keys are configured (booleans only — never values). */
  keys?: Record<KeyProvider, boolean>;
}
/** GET /theme: the stored theme (null = default palette) + the token contract. */
export interface ThemeState {
  theme: { name: string; colors: Record<string, string> } | null;
  tokens: string[];
  defaults: Record<string, string>;
}
// Shapes below mirror the server contract (src/server/app.ts), trimmed to TUI needs.
export interface Usage {
  contextTokens: number;
  outputTokens: number;
  inputTokens: number;
  cachedTokens?: number;
  lastLlmAt?: number | null;
  /** Cumulative dollars, priced server-side (pricing.ts); 0/absent when unpriced. */
  costUsd?: number;
  /** Rollup incl. the subagent subtree — the number a root session's spend should show. */
  tree?: { inputTokens: number; outputTokens: number; costUsd?: number; sessions: number };
}
export const USAGE_ZERO: Usage = { contextTokens: 0, outputTokens: 0, inputTokens: 0 };
export interface SkillInfo {
  name: string;
  description: string;
}
export interface NetStatus {
  enabled: boolean;
  running: boolean;
  listeners: number;
  caPath: string;
  caTrusted?: boolean;
  caTrustCommand?: string;
}
export type NetMode = "read_only" | "review" | "all" | "yolo";
export interface NetConfig {
  mode: NetMode;
  prevMode?: Exclude<NetMode, "yolo">;
  allowHosts: string[];
  denyHosts: string[];
  hostMiss: "allow" | "deny" | "hold";
  allowVerbs: string[];
  denyVerbs: string[];
  holdVerbs: string[];
}
export interface McpConnStatus {
  server: string;
  sessionId: string;
  alive: boolean;
  toolCount: number;
  lastUsed: number;
}
export interface McpStatus {
  registry: { servers: Record<string, { command?: string; url?: string }> };
  auth: Record<string, { authorized: boolean }>;
  active: string[];
  connections: McpConnStatus[];
}
export type McpAuthResult =
  | { status: "authorized" }
  | { status: "redirect"; authorizationUrl: string };

/** 401 — the server wants a password login (BOUGH_PASSWORD is set). */
export class AuthError extends Error {
  constructor() {
    super("authentication required");
  }
}

// Session cookie minted by POST /auth/login; attached to every request (including
// the SSE fetch in events.ts). null = no password configured or not logged in yet.
let cookie: string | null = null;
export function setCookie(c: string | null) {
  cookie = c;
}
export function authHeaders(): Record<string, string> {
  return cookie ? { cookie } : {};
}

async function j<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { ...(init?.headers as Record<string, string>), ...authHeaders() },
  });
  if (res.status === 401) throw new AuthError();
  if (!res.ok) throw new Error(`${init?.method ?? "GET"} ${path}: ${res.status}`);
  return (await res.json()) as T;
}

// Like j(), but surfaces the server's error message (fork/compact 400s carry one).
async function jmsg<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { ...(init?.headers as Record<string, string>), ...authHeaders() },
  });
  if (res.status === 401) throw new AuthError();
  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error((body as { error?: string }).error ?? `${path}: ${res.status}`);
  }
  return body as T;
}

const post = (path: string) => j<{ ok: boolean }>(path, { method: "POST" });

const postJson = (body: unknown): RequestInit => ({
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify(body),
});

export const api = {
  listSessions: () => j<Session[]>("/sessions"),
  createSession: (body: { title?: string; workspace?: string } = {}) =>
    j<Session>("/sessions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  getSession: (id: string) =>
    j<{ session: Session; thread: Message[]; usage: Usage }>(`/sessions/${id}`),
  // 202 with empty body — fire-and-forget; the turn streams over /events.
  postMessage: async (id: string, text: string) => {
    const res = await fetch(`${BASE}/sessions/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json", ...authHeaders() },
      body: JSON.stringify({ text }),
    });
    if (res.status === 401) throw new AuthError();
    if (!res.ok) throw new Error(`POST /sessions/${id}/messages: ${res.status}`);
  },
  interrupt: (id: string) =>
    j<{ ok: boolean; interrupted: boolean }>(`/sessions/${id}/interrupt`, { method: "POST" }),
  archiveSession: (id: string) => post(`/sessions/${id}/archive`),
  deprecateSession: (id: string, on: boolean) =>
    j<{ ok: boolean }>(`/sessions/${id}/deprecate`, postJson({ on })),
  netRequests: (sessionId?: string) =>
    j<NetRequest[]>(`/net/requests${sessionId ? `?sessionId=${sessionId}` : ""}`),
  allowRequest: (id: string, scope: "once" | "session" = "once") =>
    post(`/net/requests/${id}/allow${scope === "session" ? "?scope=session" : ""}`),
  denyRequest: (id: string) => post(`/net/requests/${id}/deny`),

  // ask() question holds — the reconnect/refresh mirror of netRequests.
  questions: () => j<AskQuestion[]>(`/questions`),
  answerQuestion: (sessionId: string, qid: string, answer: string) =>
    j<{ ok: boolean }>(`/sessions/${sessionId}/questions/${qid}`, postJson({ answer })),
  declineQuestion: (sessionId: string, qid: string) =>
    j<{ ok: boolean }>(`/sessions/${sessionId}/questions/${qid}`, postJson({ decline: true })),

  // Fuzzy directory search for the new-session workspace autocomplete.
  searchDirs: (q: string) =>
    j<{ dirs: DirHit[] }>(`/fs/dirs?q=${encodeURIComponent(q)}`).then((r) => r.dirs),

  // Workspace file search for the composer's @-reference autocomplete.
  searchFiles: (id: string, q: string) =>
    j<{ files: string[] }>(`/sessions/${id}/files?q=${encodeURIComponent(q)}`)
      .then((r) => r.files),

  // Same, for a draft conversation (no session yet): search the prospective
  // workspace by path.
  searchDraftFiles: (dir: string, q: string) =>
    j<{ files: string[] }>(`/fs/files?dir=${encodeURIComponent(dir)}&q=${encodeURIComponent(q)}`)
      .then((r) => r.files),

  // Review payloads for a session's workspace changes.
  getChanges: (id: string) => j<{ diffs: WireDiff[] }>(`/sessions/${id}/changes`),
  applyChanges: (id: string, source: ChangeSource, paths: string[]) =>
    jmsg<ApplyOutcome>(`/sessions/${id}/changes/apply`, postJson({ source, paths })),
  revertChanges: (id: string) => j(`/sessions/${id}/changes/revert`, postJson({})),

  // Branching. Both return the new branch's Session (or throw the server's message).
  fork: (
    id: string,
    body: { atMessageId: string; atPart?: number; editedText?: string; exclusive?: boolean },
  ) => jmsg<{ session: Session }>(`/sessions/${id}/fork`, postJson(body)).then((r) => r.session),
  compact: (id: string, picks: { messageId: string }[]) =>
    jmsg<{ session: Session }>(`/sessions/${id}/compact`, postJson({ picks }))
      .then((r) => r.session),
  // `replaceSource` makes the extract stand in for the source in the lineage
  // (title + origin link) — the delete-range flow, which archives the source.
  extract: (id: string, picks: { messageId: string }[], replaceSource = false) =>
    jmsg<{ session: Session }>(
      `/sessions/${id}/extract`,
      postJson(replaceSource ? { picks, replaceSource } : { picks }),
    ).then((r) => r.session),
  // Section grouping: LLM-label the tree's turn gists into contiguous activity
  // sections (color coding + whole-section selection in the conversation tree).
  getSections: (id: string, turns: { gist: string }[]) =>
    jmsg<{ sections: WireSection[] }>(`/sessions/${id}/sections`, postJson({ turns }))
      .then((r) => r.sections),
  // Handoff: the server drafts a self-contained opening prompt from this thread
  // toward `goal` and attaches it to a fresh conversation (session.draft).
  handoff: (id: string, goal: string) =>
    jmsg<{ session: Session }>(`/sessions/${id}/handoff`, postJson({ goal }))
      .then((r) => r.session),
  // Append copies of the source's picked messages onto an existing target session.
  moveInto: (targetId: string, sourceId: string, picks: { messageId: string }[]) =>
    jmsg<{ session: Session }>(`/sessions/${targetId}/move-into`, postJson({ sourceId, picks }))
      .then((r) => r.session),

  getConfig: () => j<BoughConfig>("/config"),
  putKey: (provider: KeyProvider, key: string) =>
    j<{ ok: boolean; keys: Record<KeyProvider, boolean> }>("/config/keys", {
      ...postJson({ provider, key }),
      method: "PUT",
    }),
  // With sessionId: pins that session to the model AND moves the default for
  // new sessions; other existing sessions keep their own.
  setModel: (model: string, sessionId?: string) =>
    j<{ model: string }>("/config", {
      ...postJson(sessionId ? { model, sessionId } : { model }),
      method: "PATCH",
    }),
  setWorker: (worker: string) =>
    j<{ worker: string }>("/config", { ...postJson({ worker }), method: "PATCH" }),
  // Thinking depth — same pin-the-session semantics as setModel; "default" clears.
  setEffort: (effort: string, sessionId?: string) =>
    j<{ effort: string }>("/config", {
      ...postJson(sessionId ? { effort, sessionId } : { effort }),
      method: "PATCH",
    }),

  // Web-UI theme: stored palette + token contract; PUT applies, DELETE resets.
  getTheme: () => j<ThemeState>("/theme"),
  putTheme: (name: string, colors: Record<string, string>) =>
    jmsg<{ theme: ThemeState["theme"] }>("/theme", {
      ...postJson({ name, colors }),
      method: "PUT",
    }),
  resetTheme: () => j<{ theme: null }>("/theme", { method: "DELETE" }),

  skills: () => j<{ skills: SkillInfo[] }>("/skills").then((r) => r.skills),

  // Claw Patrol: gateway status, the editable rule set, and the yolo toggle.
  netStatus: () => j<NetStatus>("/net/status"),
  getPolicy: () => j<NetConfig>("/net/policy"),
  setYolo: (on: boolean) => j<{ config: NetConfig }>("/net/yolo", postJson({ on })),

  // MCP management (mirrors the web rail; session-scoped like the web client).
  mcpStatus: (sessionId?: string | null) =>
    j<McpStatus>(`/mcp/servers${sessionId ? `?session=${encodeURIComponent(sessionId)}` : ""}`),
  connectMcp: (name: string, sessionId: string) =>
    j<{ connected: boolean; error?: string; tools?: unknown[] }>(
      `/mcp/servers/${encodeURIComponent(name)}/connect?session=${encodeURIComponent(sessionId)}`,
      { method: "POST" },
    ),
  restartMcp: (name: string) =>
    j(`/mcp/servers/${encodeURIComponent(name)}/restart`, { method: "POST" }),
  setMcpEnabled: (name: string, on: boolean, sessionId?: string | null) =>
    j(
      `/mcp/servers/${encodeURIComponent(name)}/${on ? "enable" : "disable"}${
        sessionId ? `?session=${encodeURIComponent(sessionId)}` : ""
      }`,
      { method: "POST" },
    ),
  mcpAuth: (name: string) =>
    j<McpAuthResult>(`/mcp/servers/${encodeURIComponent(name)}/auth`, { method: "POST" }),

  /** POST /auth/login; on success stores and returns the session cookie. */
  login: async (password: string) => {
    const res = await fetch(`${BASE}/auth/login`, postJson({ password }));
    if (!res.ok) throw new Error("wrong password");
    const tok = res.headers.getSetCookie().find((c) => c.startsWith("bough_session="));
    if (!tok) throw new Error("login succeeded but no cookie in response");
    cookie = tok.split(";")[0];
    return cookie;
  },
};
