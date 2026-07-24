// REST client for the TUI. Unlike the retired web client (relative URLs behind a
// proxy), this talks to the server directly, so every call needs the absolute base.
import type { AskQuestion, Message, Session } from "../schema/parts.ts";

const PORT = Deno.env.get("BOUGH_PORT") ?? "4321";
export const BASE = `http://127.0.0.1:${PORT}`;

// Server response shapes. Where the TUI consumes a server type verbatim, it
// re-exports the canonical definition (type-only, so nothing server-side is
// pulled into the TUI bundle) rather than re-declaring it — that keeps the TUI's
// view from silently drifting from what the server actually sends. Shapes with no
// single canonical source (assembled inline in app.ts) stay declared below.
import type { DirHit } from "../server/files.ts";
import type { Section as WireSection } from "../sections.ts";
import type {
  Diff as WireDiff,
  FileDiff as WireFileDiff,
  Hunk as WireHunk,
} from "../schema/changes.ts";
import type { KeyProvider } from "../server/keys.ts";
import type { JobInfo as JobRow } from "../tools/bash_bg.ts";
import type {
  Schedule as WireSchedule,
  WorkflowAgent as WireWorkflowAgent,
  WorkflowRun as WireWorkflowRun,
} from "../db/db.ts";
// An LLM-labeled topic section over the conversation tree's turns (Section), the
// change-review diff family (Diff/FileDiff/Hunk), a dir picker hit, a provider
// key name, a background-shell row, and a recurring-run row — re-exported (type
// only) so the TUI can't drift from what the server sends.
export type {
  DirHit,
  JobRow,
  WireDiff,
  WireFileDiff,
  WireHunk,
  WireSchedule,
  WireSection,
  WireWorkflowAgent,
  WireWorkflowRun,
};

/** GET /workflows list rows — the server's workflowSummary shape (workflow.ts). */
export interface WfSummary {
  id: string;
  name: string;
  description: string;
  status: WireWorkflowRun["status"];
  currentPhase: string | null;
  phases: { title: string; detail?: string }[];
  agents: { total: number; done: number; running: number; failed: number };
  result: unknown;
  error: string | null;
  resumeOf: string | null;
  createdAt: number;
  finishedAt: number | null;
  scriptFile: string;
}
export type { KeyProvider };
export type ChangeSource = "clonefile" | "shadow";
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
  /** Usable prompt budget of the session's model (window minus the output
   * reservation) — drives the "% left" context meter. Null/absent = unknown model. */
  contextLimit?: number | null;
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
  // archived=true → the soft-deleted rows only (the picker's reveal/restore).
  listSessions: (archived = false) => j<Session[]>(`/sessions${archived ? "?archived=1" : ""}`),
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
  unarchiveSession: (id: string) => post(`/sessions/${id}/unarchive`),
  deprecateSession: (id: string, on: boolean) =>
    j<{ ok: boolean }>(`/sessions/${id}/deprecate`, postJson({ on })),
  // Persist the composer draft on its session (stashed when switching away; the
  // prefill on open reads it back via getSession). null clears.
  putDraft: (id: string, draft: string | null) =>
    j<{ ok: boolean }>(`/sessions/${id}/draft`, { ...postJson({ draft }), method: "PUT" }),
  // ask() question holds — rebuilt/refreshed on (re)attach.
  questions: () => j<AskQuestion[]>(`/questions`),
  answerQuestion: (sessionId: string, qid: string, answer: string) =>
    j<{ ok: boolean }>(`/sessions/${sessionId}/questions/${qid}`, postJson({ answer })),
  declineQuestion: (sessionId: string, qid: string) =>
    j<{ ok: boolean }>(`/sessions/${sessionId}/questions/${qid}`, postJson({ decline: true })),

  // Worker-predicted ghost text: the user's likely next message, from the
  // conversation so far. Null when the worker has nothing usable.
  suggest: (sessionId: string) =>
    j<{ suggestion: string | null }>(`/suggest`, postJson({ sessionId }))
      .then((r) => r.suggestion),

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

  // Workflow runs of a session (newest first) + one run's full detail.
  workflows: (sessionId?: string) =>
    j<{ workflows: WfSummary[] }>(`/workflows${sessionId ? `?session=${sessionId}` : ""}`)
      .then((r) => r.workflows),
  getWorkflow: (id: string) =>
    j<{ workflow: WireWorkflowRun; agents: WireWorkflowAgent[]; scriptFile: string }>(
      `/workflows/${id}`,
    ),
  // stop / pause / resume — jmsg surfaces the 409 reason ("not running").
  workflowAction: (id: string, action: "stop" | "pause" | "resume") =>
    jmsg<WireWorkflowRun>(`/workflows/${id}/${action}`, { method: "POST" }),
  // Rerun with journal replay; no body = the run's (possibly edited) script mirror.
  rerunWorkflow: (id: string) => jmsg<WireWorkflowRun>(`/workflows/${id}/rerun`, postJson({})),

  // Live + recent background shells of a session (and its subagent branches).
  jobs: (id: string) => j<{ jobs: JobRow[] }>(`/sessions/${id}/jobs`).then((r) => r.jobs),
  // Kill a running background shell directly (no LLM round-trip).
  killJob: (id: string, jobId: string) =>
    jmsg<{ message: string }>(`/sessions/${id}/jobs/${jobId}/kill`, { method: "POST" }),
  applyChanges: (id: string, source: ChangeSource, paths: string[]) =>
    jmsg<ApplyOutcome>(`/sessions/${id}/changes/apply`, postJson({ source, paths })),
  revertChanges: (id: string) => j(`/sessions/${id}/changes/revert`, postJson({})),
  // Fold a finished subagent's branch into its spawner's workspace (the UI
  // affordance mirroring the program's adopt() host function).
  adoptSubagent: (subId: string) =>
    jmsg<{ message: string }>(`/sessions/${subId}/adopt`, { method: "POST" }),

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
  deleteKey: (provider: KeyProvider) =>
    j<{ ok: boolean; keys: Record<KeyProvider, boolean> }>("/config/keys", {
      ...postJson({ provider }),
      method: "DELETE",
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

  // Recurring agent runs (the /schedule popup). Mutations surface the server's
  // message (spec/workspace validation 400s carry one).
  listSchedules: () => j<{ schedules: WireSchedule[] }>("/schedules").then((r) => r.schedules),
  createSchedule: (body: { title: string; prompt: string; spec: string; workspace?: string }) =>
    jmsg<WireSchedule>("/schedules", postJson(body)),
  patchSchedule: (id: string, body: { enabled?: boolean; spec?: string }) =>
    jmsg<WireSchedule>(`/schedules/${id}`, { ...postJson(body), method: "PATCH" }),
  deleteSchedule: (id: string) => jmsg<{ ok: boolean }>(`/schedules/${id}`, { method: "DELETE" }),

  // MCP management (session-scoped).
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
