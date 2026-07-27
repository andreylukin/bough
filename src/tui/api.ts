/**
 * The TUI's typed HTTP client — the one module in the client that knows a URL.
 *
 * THE INVARIANT THIS HOLDS: **no component talks HTTP, and no URL is written
 * twice.** Every server route reachable from the TUI is a method here, so a route
 * that moves is one edit, and a renderer that wants data takes it from the store
 * (`store.ts`) rather than reaching for `fetch`. The old tree lost this boundary and
 * paid for it with a 3,618-line `App.tsx` that fetched, reduced and rendered in the
 * same file; the plan's hard rule for M9 (no component over ~300 lines) is only
 * achievable if the data layer is somewhere else. This is that somewhere.
 *
 * Second invariant: **the client never re-declares a wire shape it can import.**
 * Every response type below is either the server's own exported type (imported
 * `import type`, so nothing server-side is pulled in at runtime and no import cycle
 * can form) or an interface for a body the server assembles inline in its handler,
 * declared here beside the method that reads it. A hand-copied shape drifts silently;
 * an imported one fails `deno check` the day the server changes.
 *
 * Third, and the reason `createApi` takes a base and a `fetch`: **everything is
 * injected.** A test points the client at a loopback server it started itself, or at
 * a fake `fetch`, and never touches the real port or the user's `~/.bough`. The
 * module-level `api` is the production convenience, built from `BOUGH_PORT`.
 *
 * Note on `server/sessions.ts`: `app.ts` and `sessions.ts` are a mutual import cycle
 * that only resolves because `app.ts` evaluates first, so `SessionListItem` is
 * RESTATED here (as `SessionRow`) rather than imported. It is four fields over
 * `Session` and the alternative is a module-initialization hazard for every future
 * importer of this file.
 *
 * The interrupt gap this header used to report is CLOSED: `POST
 * /sessions/:id/interrupt` exists (`server/turns.ts`) and `interrupt()` below is the
 * method that raises it. It always resolves for a session that exists —
 * `{interrupted: false}` is the answer when the turn had already ended — so the
 * caller needs no race-condition branch for a button whose job is to be safe to
 * press.
 */
import type {
  AskQuestion,
  BackgroundJob,
  Message,
  Schedule,
  Session,
  TurnStatus,
  WorkflowAgent,
  WorkflowPhase,
  WorkflowRun,
  WorkflowStatus,
} from "../schema/parts.ts";
// T10.4 — the wire shape `tui/theme.ts` already declares for `GET /theme`. Imported
// from there rather than from `server/theme.ts`: the TUI process must not pull a
// server module in for a type.
import type { ThemeColors, ThemeState } from "./theme.ts";
import type {
  CompactBody,
  CreateScheduleBody,
  CreateSessionBody,
  CreateWorkflowBody,
  ExtractBody,
  ForkBody,
  HandoffBody,
  MoveBody,
  PatchScheduleBody,
  PostCommentBody,
  PostMessageBody,
  RerunWorkflowBody,
  SectionsBody,
} from "../schema/requests.ts";
import type { UsageTotals } from "../types.ts";
// Type-only, all of them: erased at compile time, so this module has no runtime edge
// into `server/`, `workflow/` or `mcp/` and cannot participate in an import cycle.
import type { Artifact } from "../hostfn/artifact.ts";
import type { Section } from "../history/sections.ts";
import type { McpStatus } from "../mcp/status.ts";
import type { AuthStart, AuthStatus } from "../mcp/oauth.ts";
import type { ArtifactComment } from "../server/comments.ts";
import type { RevertOutcome, SessionChangeSet } from "../server/changes.ts";
import type { SearchResult } from "../server/search.ts";
import type { InterruptResult } from "../server/turns.ts";
import type { SkillRow as SkillListRow } from "../server/skills.ts";
import type { SkillSource } from "../skills/skills.ts";
import type { WorkflowAgentView } from "../workflow/control.ts";
import type { LargeRunFlag, ReplaySummary, RunCost, SizeGuideline } from "../workflow/report.ts";
import type { RelaunchPreview, RelaunchReport } from "../workflow/relaunch.ts";
import type { SavedWorkflow, SavedWorkflowDetail } from "../workflow/saved.ts";

// ---- where the server is ----------------------------------------------------

export const DEFAULT_PORT = 4321;

/**
 * The loopback origin the server binds (`server/main.ts`). `BOUGH_PORT` is how the
 * rewrite runs beside the live install (plan §2), so it is read here rather than
 * hard-coded — and read through a `try`, because the TUI may run with no env access
 * and a missing permission must degrade to the default rather than crash the client.
 */
export function defaultBase(): string {
  let port = String(DEFAULT_PORT);
  try {
    port = Deno.env.get("BOUGH_PORT") ?? port;
  } catch {
    // No --allow-env. The default port is the right answer.
  }
  return `http://127.0.0.1:${port}`;
}

// ---- errors -----------------------------------------------------------------

/**
 * A request that reached the server and came back non-2xx.
 *
 * `message` is the server's own `{error}` text whenever there is one, because that
 * text is a product surface (spec §6): "select turns from this conversation" is an
 * answer, and `POST /sessions/x/compact: 400` is not. `status` is kept so a caller
 * can branch (404 → the session is gone) without parsing prose.
 */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly method: string,
    readonly path: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/**
 * The server could not be reached at all.
 *
 * Its own class because the remedy is different and specific: nothing is wrong with
 * the request, the server is not running. Deno's raw failure here is a Rust-flavoured
 * `TypeError` ("error sending request … Connection refused") that reads as a bough
 * crash, which is exactly the wrong thing to show a user at cold start.
 */
export class OfflineError extends Error {
  constructor(readonly base: string, override readonly cause: unknown) {
    // The COMMAND comes before the address, because this line is rendered into a
    // one-row notice that truncates: with the remedy last, an 80-column terminal
    // cut it to "…start it with: bough st…" — the one thing the reader needed,
    // clipped mid-word. What is worth losing to a narrow terminal is the URL.
    super(`bough server unreachable — run: bough start · ${base}`);
    this.name = "OfflineError";
  }
}

// ---- shapes the server assembles inline -------------------------------------

/**
 * A row of `GET /sessions`. Mirrors `server/sessions.ts`'s `SessionListItem`,
 * restated rather than imported — see the header's note on the `app.ts`/`sessions.ts`
 * cycle. All three extras are DERIVED server-side at read time; none is a column.
 */
export interface SessionRow extends Session {
  /** A turn is in flight right now. Live-updated from events after this read. */
  busy: boolean;
  /** How the most recent turn ended. Absent when the session never ran one. */
  lastTurnStatus?: TurnStatus;
  /** This session's own spend. Omitted when zero. */
  costUsd?: number;
}

/** `GET /sessions/:id` — the reconnect payload (spec §3). */
export interface SessionSnapshot {
  session: Session;
  /** Ancestors root→parent, then own. Assembled, never stored. */
  thread: Message[];
  usage: UsageTotals & { tree: UsageTotals };
  /**
   * The model the next turn will actually call. `session.model` is null until a
   * user pins one, so this — not that — is what the meter names. Optional because
   * a server older than the field simply omits it.
   */
  effectiveModel?: string;
  /** The effective model's context window, for the meter's percentage. Null = unknown. */
  contextLimit?: number | null;
}

/** `POST /sessions/:id/messages` — 202. `queued` = a turn was already running. */
export interface PostedMessage {
  message: Message;
  queued: boolean;
}

/** What a history operation that CREATES a branch answers with (spec §14). */
export interface BranchResult {
  session: Session;
  thread: Message[];
  /** Fork only: whether "edit & resend" started a turn on the new branch. */
  turnStarted?: boolean;
}

/** `POST /sessions/:id/move-into` — appends onto an existing session, creates nothing. */
export interface MoveResult {
  session: Session;
  thread: Message[];
  /** Copies actually written; duplicate picks of one message merge. */
  appended: number;
}

/** `GET /sessions/:id/jobs/:jobId/output` — the whole retained buffer, non-destructively. */
export interface JobOutput {
  output: string;
  job: BackgroundJob;
}

/** A row of `GET /workflows` — `workflow/run.ts`'s `workflowSummary`, minus the script. */
export interface WorkflowSummary {
  id: string;
  name: string;
  description: string;
  status: WorkflowStatus;
  currentPhase: string | null;
  phases: WorkflowPhase[];
  agents: {
    total: number;
    done: number;
    cached: number;
    running: number;
    queued: number;
    failed: number;
  };
  result: unknown;
  error: string | null;
  resumeOf: string | null;
  createdAt: number;
  finishedAt: number | null;
  scriptFile: string;
}

/**
 * `GET /workflows/:id` — `workflow/control.ts`'s `workflowDetail`.
 *
 * `replay` is not decorative. Spec §8: replay is ALWAYS reported, because a relaunch
 * that replayed nothing and one that replayed everything are otherwise the same
 * screen. `live` distinguishes a run this process is driving from one a restart
 * orphaned — a fan-out that will never advance must not render as a running one.
 */
export interface WorkflowDetail {
  workflow: WorkflowRun;
  agents: WorkflowAgentView[];
  scriptFile: string;
  live: boolean;
  replay: ReplaySummary;
  cost: RunCost;
  warning: LargeRunFlag | null;
  guideline: SizeGuideline;
}

/** `POST /workflows/:id/rerun` — the new run, plus what it has on offer to replay. */
export type RerunResult = WorkflowRun & { replay: ReplaySummary };

/**
 * `POST /workflows/:id/relaunch` — stop → edit → relaunch, the second half (spec §8).
 *
 * A NEW run seeded from the stopped run's journal. `replay` is the PREVIEW (what the
 * source has on offer); the counts of what was actually claimed arrive on
 * `GET /workflows/:id/replay`, because the run is detached and has claimed nothing yet.
 */
export interface RelaunchResult {
  workflow: WorkflowRun;
  /** The run whose journal this one reads. Untouched by the relaunch. */
  source: string;
  /** Where the script came from: the edited mirror, the stored row, or the caller. */
  script: string;
  replay: RelaunchPreview;
}

/** `GET /workflows/:id/replay` — the report plus its one-line human form. */
export type ReplayReport = RelaunchReport & { line: string };

/** `GET`/`PUT /workflow-settings` — advice to whoever writes the next script. */
export interface WorkflowSettings {
  sizeGuideline: SizeGuideline;
  target: number | null;
  advice: string;
  tokenWarnThreshold: number;
  concurrency?: number;
  maxAgentsPerRun?: number;
  /** Always true: nothing here caps, pauses or throttles a run (spec §8). */
  advisory: true;
}

/** `POST /mcp/servers/:name/connect` — the "prove it" step; not a grant. */
export type McpConnectResult = McpStatus & {
  server: string;
  connected: boolean;
  error?: string;
  tools: { name: string; description: string }[];
};

/** `POST /search/reindex` — the repair path for a swallowed index write. */
export interface ReindexResult {
  rebuilt: boolean;
  messages: number;
  sessions: number;
}

// ---- the client -------------------------------------------------------------

export interface ApiOptions {
  /** Absent = `defaultBase()`. */
  base?: string;
  /** Absent = the global `fetch`. Injected by tests and by the offline-error wrapper. */
  fetchFn?: typeof fetch;
}

/** Query-string builder that omits absent values, so no URL ends in a bare `?`. */
function query(params: Record<string, string | number | undefined | null>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null && value !== "") search.set(key, String(value));
  }
  const text = search.toString();
  return text ? `?${text}` : "";
}

/** Percent-encode one path segment. Session ids are uuids; artifact names are not. */
const seg = (value: string) => encodeURIComponent(value);

export function createApi(options: ApiOptions = {}) {
  const base = options.base ?? defaultBase();
  const doFetch = options.fetchFn ?? globalThis.fetch;

  /**
   * One request. Every method goes through here, which is what makes "a dead server
   * says so in one sentence" a property of the client rather than of each call site.
   */
  async function send(method: string, path: string, body?: unknown): Promise<Response> {
    const init: RequestInit = { method };
    if (body !== undefined) {
      init.headers = { "content-type": "application/json" };
      init.body = JSON.stringify(body);
    }
    try {
      return await doFetch(`${base}${path}`, init);
    } catch (cause) {
      throw new OfflineError(base, cause);
    }
  }

  /**
   * Request → parsed JSON, with the server's `{error}` message preserved on failure.
   *
   * There is deliberately only ONE of these. The old client had two (`j` and `jmsg`)
   * and the difference was whether the caller would see the server's sentence — which
   * meant every new endpoint had to remember to opt in, and the ones that forgot
   * turned "select turns from this conversation" into "400".
   */
  async function json<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await send(method, path, body);
    const text = await res.text();
    let parsed: unknown = undefined;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        parsed = undefined;
      }
    }
    if (!res.ok) {
      const message = (parsed as { error?: string } | undefined)?.error ??
        (text.trim() || `${method} ${path}: ${res.status}`);
      throw new ApiError(res.status, message, method, path);
    }
    return parsed as T;
  }

  const get = <T>(path: string) => json<T>("GET", path);
  const post = <T>(path: string, body?: unknown) => json<T>("POST", path, body);
  const put = <T>(path: string, body?: unknown) => json<T>("PUT", path, body);
  const patch = <T>(path: string, body?: unknown) => json<T>("PATCH", path, body);
  const del = <T>(path: string, body?: unknown) => json<T>("DELETE", path, body);

  return {
    /** The origin every path below is relative to. Read by `events.ts`. */
    base,
    /** `GET /events[?sessionId=]`. Built here so the SSE client owns no URL either. */
    eventsUrl: (sessionId?: string) => `${base}/events${query({ sessionId })}`,
    /** Same-origin link to a published artifact — what the agent prints for the user. */
    artifactUrl: (sessionId: string, name: string) =>
      `${base}/artifacts/${seg(sessionId)}/${name.split("/").map(seg).join("/")}`,

    // -- sessions and messages (T1.2) -----------------------------------------

    /** Top level, `subagent`/`workflow_agent` excluded. With `originId`: the drill-in. */
    listSessions: (originId?: string) => get<SessionRow[]>(`/sessions${query({ originId })}`),
    createSession: (body: CreateSessionBody = {}) => post<Session>("/sessions", body),
    /** The reconnect fetch: `{session, thread, usage}`, reconciled by message id. */
    getSession: (id: string) => get<SessionSnapshot>(`/sessions/${seg(id)}`),
    postMessage: (id: string, body: PostMessageBody) =>
      post<PostedMessage>(`/sessions/${seg(id)}/messages`, body),
    /** `null` clears the prefilled composer text. No event — the writer is this client. */
    putDraft: (id: string, draft: string | null) =>
      put<{ ok: boolean; draft: string | null }>(`/sessions/${seg(id)}/draft`, { draft }),
    /**
     * Stop the running turn (spec §5). The response says whether there was one; the
     * turn actually ending arrives as `turn.finished` on the stream, like every
     * other fact about a turn, because the server signals and does not wait for the
     * children to die (`server/turns.ts`).
     */
    interrupt: (id: string) => post<InterruptResult>(`/sessions/${seg(id)}/interrupt`),

    // -- ask() holds (T6.1) ----------------------------------------------------

    /** Memory-only server-side, so this is how a freshly-attached client rebuilds the card. */
    listQuestions: (sessionId?: string) => get<AskQuestion[]>(`/questions${query({ sessionId })}`),
    answerQuestion: (sessionId: string, qid: string, answer: string) =>
      post<{ ok: boolean; id: string; status: string }>(
        `/sessions/${seg(sessionId)}/questions/${seg(qid)}`,
        { answer },
      ),
    /** The program's `ask()` rejects catchably with "user declined" (spec §6). */
    declineQuestion: (sessionId: string, qid: string) =>
      post<{ ok: boolean; id: string; status: string }>(
        `/sessions/${seg(sessionId)}/questions/${seg(qid)}`,
        { decline: true },
      ),

    // -- background jobs (T6.8) ------------------------------------------------

    /** The session AND its subagents — the work running on its behalf. */
    listJobs: (id: string) => get<{ jobs: BackgroundJob[] }>(`/sessions/${seg(id)}/jobs`),
    /** The human's kill switch, so stopping a runaway shell costs no LLM round-trip. */
    killJob: (id: string, jobId: string) =>
      post<{ message: string }>(`/sessions/${seg(id)}/jobs/${seg(jobId)}/kill`),
    jobOutput: (id: string, jobId: string) =>
      get<JobOutput>(`/sessions/${seg(id)}/jobs/${seg(jobId)}/output`),

    // -- changes (T8.5) --------------------------------------------------------

    /** Always 200: "not a repository" is an answer, not an error (spec §13). */
    getChanges: (id: string) => get<SessionChangeSet>(`/sessions/${seg(id)}/changes`),
    /**
     * The rail's only mutation. OMITTING `paths` reverts the whole change set;
     * passing an explicit empty array is refused by the server (400) rather than
     * treated as a wildcard, because an empty selection is what a UI produces when
     * nothing is highlighted and revert deletes files. Call `revertChanges(id)` for
     * "revert everything" — never `revertChanges(id, selection)` with an empty
     * `selection`.
     */
    revertChanges: (id: string, paths?: string[]) =>
      post<RevertOutcome>(`/sessions/${seg(id)}/changes/revert`, paths ? { paths } : {}),

    // -- history operations (T8.2–T8.4) ---------------------------------------

    fork: (id: string, body: ForkBody) => post<BranchResult>(`/sessions/${seg(id)}/fork`, body),
    compact: (id: string, body: CompactBody) =>
      post<BranchResult>(`/sessions/${seg(id)}/compact`, body),
    /** Stateless: gists in, labeled ranges out. Nothing is read or written. */
    sections: (id: string, body: SectionsBody) =>
      post<{ sections: Section[] }>(`/sessions/${seg(id)}/sections`, body),
    extract: (id: string, body: ExtractBody) =>
      post<BranchResult>(`/sessions/${seg(id)}/extract`, body),
    moveInto: (targetId: string, body: MoveBody) =>
      post<MoveResult>(`/sessions/${seg(targetId)}/move-into`, body),
    handoff: (id: string, body: HandoffBody) =>
      post<{ session: Session }>(`/sessions/${seg(id)}/handoff`, body),

    // -- workflows (T5.5, T5.7, T5.8) ------------------------------------------

    listWorkflows: (sessionId?: string) =>
      get<{ workflows: WorkflowSummary[] }>(`/workflows${query({ session: sessionId })}`),
    createWorkflow: (body: CreateWorkflowBody) => post<WorkflowRun>("/workflows", body),
    /** The run's reconnect path — and the only place `replay` is guaranteed present. */
    getWorkflow: (id: string) => get<WorkflowDetail>(`/workflows/${seg(id)}`),
    /** Kills the worker AND interrupts every subagent turn the run started. */
    stopWorkflow: (id: string) => post<WorkflowRun>(`/workflows/${seg(id)}/stop`),
    /** Gates NEW `agent()` calls; the ones in flight finish and are journaled. */
    pauseWorkflow: (id: string) => post<WorkflowRun>(`/workflows/${seg(id)}/pause`),
    resumeWorkflow: (id: string) => post<WorkflowRun>(`/workflows/${seg(id)}/resume`),
    /** A rerun is the relaunch case where the script did not change (spec §8). */
    rerunWorkflow: (id: string, body: RerunWorkflowBody = {}) =>
      post<RerunResult>(`/workflows/${seg(id)}/rerun`, body),
    /** Stop → edit → relaunch. The unchanged PREFIX replays; the rest runs live. */
    relaunchWorkflow: (id: string, body: RerunWorkflowBody = {}) =>
      post<RelaunchResult>(`/workflows/${seg(id)}/relaunch`, body),
    /** How many calls came from the journal and how many cost an agent. */
    workflowReplay: (id: string) => get<ReplayReport>(`/workflows/${seg(id)}/replay`),
    /** The run view's `x`/`r` on ONE agent, while the rest of the run keeps going. */
    controlWorkflowAgent: (id: string, agentId: string, action: "stop" | "restart") =>
      post<WorkflowAgent>(`/workflows/${seg(id)}/agents/${seg(agentId)}/${action}`),
    saveWorkflowAs: (id: string, name: string) =>
      post<SavedWorkflow>(`/workflows/${seg(id)}/save`, { name }),
    listSavedWorkflows: () => get<{ saved: SavedWorkflow[] }>("/saved-workflows"),
    getSavedWorkflow: (name: string) => get<SavedWorkflowDetail>(`/saved-workflows/${seg(name)}`),
    putSavedWorkflow: (name: string, body: { script?: string; runId?: string }) =>
      put<SavedWorkflow>(`/saved-workflows/${seg(name)}`, body),
    /** Invoke a saved workflow by name, parameterized through `args`. */
    runSavedWorkflow: (name: string, body: { sessionId: string; args?: unknown }) =>
      post<WorkflowRun & { savedAs: string }>(`/saved-workflows/${seg(name)}/runs`, body),
    /** What a NEW conversation runs on, for the picker's ● before any session exists. */
    getModelSettings: () => get<{ defaultModel: string }>("/model-settings"),
    getWorkflowSettings: () => get<WorkflowSettings>("/workflow-settings"),
    putWorkflowSettings: (sizeGuideline: SizeGuideline) =>
      put<WorkflowSettings>("/workflow-settings", { sizeGuideline }),

    // -- schedules (T6.3) ------------------------------------------------------

    listSchedules: () => get<Schedule[]>("/schedules"),
    createSchedule: (body: CreateScheduleBody) => post<Schedule>("/schedules", body),
    patchSchedule: (id: string, body: PatchScheduleBody) =>
      patch<Schedule>(`/schedules/${seg(id)}`, body),
    deleteSchedule: (id: string) => del<{ ok: boolean; removed: string }>(`/schedules/${seg(id)}`),

    // -- artifacts and their comment layer (T6.6, T6.7) ------------------------

    /** Filesystem-backed: correct for a session whose row is gone (spec §4). */
    listArtifacts: (id: string) => get<{ artifacts: Artifact[] }>(`/sessions/${seg(id)}/artifacts`),
    listComments: (id: string, artifact?: string) =>
      get<{ comments: ArtifactComment[] }>(`/sessions/${seg(id)}/comments${query({ artifact })}`),
    postComment: (id: string, body: PostCommentBody) =>
      post<ArtifactComment>(`/sessions/${seg(id)}/comments`, body),
    deleteComment: (id: string, commentId: string) =>
      del<{ ok: boolean }>(`/sessions/${seg(id)}/comments/${seg(commentId)}`),
    /** Delivers the batch as one `[artifact comments]` system message. */
    sendComments: (id: string, ids?: string[]) =>
      post<{ sent: number; wake?: boolean }>(
        `/sessions/${seg(id)}/comments/send`,
        ids ? { ids } : {},
      ),

    // -- MCP (T7.1–T7.3) -------------------------------------------------------

    /** Never cached by a caller: grants and connections change between turns (§6.13). */
    mcpStatus: (sessionId?: string) =>
      get<McpStatus>(`/mcp/servers${query({ session: sessionId })}`),
    putMcpRegistry: (registry: { servers: Record<string, unknown> }) =>
      put<McpStatus & { changed: string[] }>("/mcp/servers", registry),
    putMcpServer: (name: string, config: unknown) =>
      put<McpStatus>(`/mcp/servers/${seg(name)}`, config),
    deleteMcpServer: (name: string) => del<McpStatus>(`/mcp/servers/${seg(name)}`),
    /** Proves the command works. Connecting is NOT granting. */
    connectMcpServer: (name: string, sessionId: string) =>
      post<McpConnectResult>(
        `/mcp/servers/${seg(name)}/connect${query({ session: sessionId })}`,
      ),
    restartMcpServer: (name: string, sessionId: string) =>
      post<McpStatus & { restarted: boolean }>(
        `/mcp/servers/${seg(name)}/restart${query({ session: sessionId })}`,
      ),
    /** The grant itself. `sessionId: ""` is the global scope (`mcp/config.ts`). */
    setMcpEnabled: (name: string, on: boolean, sessionId: string, ttl?: string) =>
      post<McpStatus>(
        `/mcp/servers/${seg(name)}/${on ? "enable" : "disable"}`,
        ttl ? { sessionId, ttl } : { sessionId },
      ),
    mcpAuthStatus: (name: string) => get<AuthStatus>(`/mcp/servers/${seg(name)}/auth`),
    /** Returns the URL the human must open. Never opens a browser, never blocks. */
    beginMcpAuth: (name: string) => post<AuthStart>(`/mcp/servers/${seg(name)}/auth`),
    clearMcpAuth: (name: string) =>
      del<{ server: string; cleared: boolean }>(`/mcp/servers/${seg(name)}/auth`),

    // -- keyword search (T8.6) -------------------------------------------------

    search: (q: string, opts: { sessionId?: string; limit?: number } = {}) =>
      get<SearchResult>(`/search${query({ q, sessionId: opts.sessionId, limit: opts.limit })}`),
    reindex: () => post<ReindexResult>("/search/reindex"),

    // -- theme (T10.4) ---------------------------------------------------------
    //
    // The transport half of "browsing never commits" (`tui/theme.ts`). The picker
    // paints previews entirely client-side and touches none of these until the user
    // keeps one, which is why `commit()` takes `persist` as an injected function
    // rather than importing this module: only the composition root decides that
    // adopting a theme should outlive the process.
    //
    // `getTheme` answers `{theme, defaults}` and never 404s — nothing stored IS the
    // default palette, which is an answer (`server/theme.ts`). `deleteTheme` is what
    // the "Default" preset means, since the empty partial is a reset rather than a
    // palette; both it and `putTheme` return the same document `getTheme` does, so a
    // caller repaints from the response instead of re-fetching.

    getTheme: () => get<ThemeState>("/theme"),
    putTheme: (theme: { name: string; colors: ThemeColors }) => put<ThemeState>("/theme", theme),
    deleteTheme: () => del<ThemeState>("/theme"),

    // -- composer ghost text (T10.1) -------------------------------------------
    //
    // The cheap tier predicting the user's next message. ALWAYS resolves — `{ghost:
    // null}` covers every failure there is — so the composer needs no error path and
    // must not render one: a suggestion that does not arrive is the feature working
    // as specified (spec §12).

    ghostText: (sessionId: string, prefix = "") =>
      post<{ ghost: string | null }>(`/sessions/${seg(sessionId)}/ghost`, { prefix }),

    // -- skills (T10.2) --------------------------------------------------------
    //
    // A fresh walk of the source directories on every call, because that is what the
    // route is (`server/skills.ts`) — a skill written a second ago lists a second
    // later, and there is nothing here to invalidate. `sources` rides along so the
    // panel can tell "nothing installed" from "looking in the wrong directory",
    // which is the question an empty list cannot answer on its own.

    listSkills: () => get<{ skills: SkillListRow[]; sources: SkillSource[] }>("/skills"),
  };
}

/** The client's shape, for the store and for anything that fakes it in a test. */
export type Api = ReturnType<typeof createApi>;

/** The production client, bound to `BOUGH_PORT`. Tests build their own. */
export const api: Api = createApi();
