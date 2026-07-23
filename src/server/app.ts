/**
 * The HTTP surface: a tiny hand-rolled router over a single route table, plus the SSE
 * endpoint that tails the event bus. No framework — the table is a list of
 * {method, URLPattern, handler}, matched in order, so an OpenAPI doc can be generated
 * from it later. Bodies are Zod-validated at the edge; handlers work in domain types.
 *
 * Endpoints (consumed by the TUI's api.ts/events.ts and the headless CLI):
 *   GET  /sessions                 → Session[]
 *   POST /sessions                 → Session            {title, parentId?, kind?}
 *   GET  /sessions/:id             → {session, thread}  (thread = root→self messages)
 *   POST /sessions/:id/messages    → 202                {text}  (persist + start turn)
 *   GET/POST /schedules, PATCH/DELETE /schedules/:id → recurring runs (schedules.ts)
 *   GET /events[?sessionId=]  → SSE stream of BoughEvent (named events + heartbeat)
 *
 * No CORS headers: the only client is the native `bough` TUI (not a browser), so the
 * server never opts into cross-origin access. This keeps a webpage you happen to visit
 * from reaching the loopback API and driving the agent (browsers block the cross-origin
 * fetch without an allow-origin header) — the web UI that once needed CORS is gone.
 */
import type { z } from "zod";
import { HttpError } from "../errors.ts";
import {
  AnswerQuestionBody,
  CreateSessionBody,
  PostMessageBody,
  type Session,
} from "../schema/parts.ts";
import { answerAsk, declineAsk, getAsk, pendingAsks } from "../asks.ts";
import type { Db } from "../db/db.ts";
import type { Bus, Listener } from "../bus.ts";
import {
  activeEffort,
  activeModel,
  interruptTurn,
  MODELS,
  oracleModel,
  postSystemNote,
  setActiveEffort,
  setActiveModel,
  setOracleModel,
  startUserTurn,
  usableContextLimit,
  workflowCtxFor,
} from "../turn.ts";
import {
  pauseWorkflow,
  rerunWorkflow,
  resumeWorkflow,
  scriptPath as workflowScriptPath,
  startWorkflow,
  stopWorkflow,
  WorkflowCreateBody,
  WorkflowRerunBody,
  workflowSummary,
} from "../workflow.ts";
import { clientFor, type Effort, EFFORTS, type LlmClient } from "../supervisor/llm.ts";
import { setWorkerChoice, WORKER_OPTIONS, workerChoice } from "../worker/frontier.ts";
import { SuggestBody, suggestNextStep } from "../worker/suggest.ts";
import { sessionMetrics } from "../metrics.ts";
import { normalizeWorkspace, prepareWorkspace, workspaceProblem } from "../supervisor/workspace.ts";
import { UNTITLED } from "../supervisor/title.ts";
import { listSkills } from "../supervisor/skills.ts";
import { grantedDirs, searchDirectories, searchWorkspaceFiles } from "./files.ts";
import { fork, ForkBody } from "../fork.ts";
import { type BundleManifest, getBundle, listBundles } from "../net/bundles.ts";
import type { Gate } from "../net/gate.ts";
import type { ClawpatrolGateway } from "../net/gateway.ts";
import { installBundle, InstallError, isInstalled } from "../net/install.ts";
import {
  loadConfig,
  NetConfig,
  resolveConfig,
  saveConfig,
  setPluginActivation,
  setYolo,
  toPolicy,
} from "../net/config.ts";
import { ttlToExpires } from "../net/plugins.ts";
import {
  activationsFor as mcpActivationsFor,
  loadRegistry as loadMcpRegistry,
  removeServer as removeMcpServer,
  saveRegistry as saveMcpRegistry,
  setActivation as setMcpActivation,
  upsertServer as upsertMcpServer,
} from "../mcp/config.ts";
import { mcpManager } from "../mcp/manager.ts";
import { mcpStatusFor } from "../mcp/status.ts";
import { beginAuth, clearAuth, completeAuth } from "../mcp/oauth.ts";
import { listArtifacts, serveArtifact } from "./artifacts.ts";
import { VIEWER_JS_PATH, viewerBundle } from "./jsonrender/bundle.ts";
import {
  addComment,
  AddCommentBody,
  deleteComment,
  formatForAgent,
  loadComments,
  markSent,
} from "./comments.ts";
import { createAuth } from "./auth.ts";
import { compact, CompactBody } from "../compact.ts";
import {
  scheduleCreate,
  ScheduleCreateBody,
  schedulePatch,
  SchedulePatchBody,
  scheduleRemove,
} from "../schedules.ts";
import { sectionize, SectionsBody } from "../sections.ts";
import { extract, ExtractBody } from "../extract.ts";
import { handoff, HandoffBody } from "../handoff.ts";
import { move, MoveBody } from "../move.ts";
import { adoptSubagent } from "../subagent.ts";
import { bashKill, listJobs, onJobEvent } from "../tools/bash_bg.ts";
import { applyChanges, revertChanges, sessionChanges } from "./changes.ts";
import { ChangesApplyBody, ChangesRevertBody } from "../schema/changes.ts";
import { clearTheme, loadTheme, saveTheme, Theme, THEME_DEFAULTS, THEME_TOKENS } from "./theme.ts";
import { deleteKey, KeyDeleteBody, KeysBody, keyStatus, persistEnvVar, setKey } from "./keys.ts";
import {
  ensureOpenAIModels,
  mergeModels,
  openaiModels,
  refreshOpenAIModels,
} from "./openai_models.ts";

export interface AppCtx {
  db: Db;
  bus: Bus;
  /** The egress gate the native proxy calls; owns hold-and-ask. Absent in tests that don't gate. */
  gate?: Gate;
  /** The Claw Patrol gateway bough supervises; absent in tests. */
  gateway?: ClawpatrolGateway;
  /** Net config dir override (tests); undefined = ~/.bough/net. */
  netDir?: string;
  /** Theme storage dir override (tests); undefined = ~/.bough. */
  themeDir?: string;
  /** Launcher env-file dir override (tests); undefined = ~/.bough. */
  envDir?: string;
  /** LLM client for compaction/turns; injected for tests, else the real Anthropic client. */
  llm?: LlmClient;
  /** Model override; else BOUGH_MODEL, else the default. */
  model?: string;
  /** clonefile snapshot root override (tests); else BOUGH_SNAPSHOT_BASE / default. */
  snapshotBase?: string;
  /** When set, every request requires a login session (see auth.ts). From BOUGH_PASSWORD. */
  password?: string;
  /** Retitler for compaction branches (production: local title worker); absent in tests. */
  retitler?: (text: string) => Promise<string | null>;
}

type Handler = (
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
) => Response | Promise<Response>;
type Route = { method: string; pattern: URLPattern; handler: Handler };

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function error(status: number, message: string): Response {
  return json({ error: message }, status);
}

/** Parse + validate a JSON body; an invalid one throws the 400 that the
 * dispatcher's HttpError catch turns into a response. `fallback` stands in for
 * an absent/unparseable body (default null → schema decides the 400). */
async function parseBody<S extends z.ZodTypeAny>(
  req: Request,
  schema: S,
  fallback: unknown = null,
): Promise<z.infer<S>> {
  const parsed = schema.safeParse(await req.json().catch(() => fallback));
  if (!parsed.success) throw new HttpError(400, "invalid body: " + parsed.error.message);
  return parsed.data;
}

// ---- handlers --------------------------------------------------------------

const getConfig: Handler = () => {
  // First config read after boot with a key already present: refresh the OpenAI
  // list in the background — this response serves the static table, the next
  // one includes the pulled models.
  ensureOpenAIModels();
  return json({
    model: activeModel(),
    models: mergeModels(MODELS, openaiModels()),
    // Thinking depth: "" = provider default; the picker offers `efforts`.
    effort: activeEffort(),
    efforts: EFFORTS,
    oracle: oracleModel(),
    worker: workerChoice(),
    workerOptions: WORKER_OPTIONS,
    // Which provider API keys are configured — booleans only, never the values.
    keys: keyStatus(),
  });
};

// Set a provider's API key: applies to the live process env immediately (clients read
// the env at run time) and persists to ~/.bough/env for restarts. Returns the
// refreshed booleans; never echoes the value back. An OpenAI key also pulls the
// account's model list (awaited, so the client's follow-up config read sees it).
const putKeys: Handler = async (req) => {
  const body = await parseBody(req, KeysBody);
  const keys = setKey(body.provider, body.key);
  if (body.provider === "openai") await refreshOpenAIModels();
  return json({ ok: true, keys });
};

// Remove a provider's API key: from the live process env and from ~/.bough/env,
// so the deletion also survives a restart. Returns the refreshed booleans.
const deleteKeys: Handler = async (req) => {
  const body = await parseBody(req, KeyDeleteBody);
  return json({ ok: true, keys: deleteKey(body.provider) });
};

// Switch the model new turns run on and/or the worker micro-tasks run on. Any id
// is accepted (the pickers list curated subsets); a provider-prefixed model id
// routes to OpenRouter (see turn.ts / llm.ts). `worker` is "local" or a model id —
// process-global like the active model, never per session.
// A model change with `sessionId` also PINS that session to the model: the open
// conversation switches immediately, the global default moves so NEW sessions
// start on it, and every other existing session keeps whatever it was on. The
// default persists to ~/.bough/env (BOUGH_MODEL) so it survives a restart.
const patchConfig: Handler = async (req, ctx) => {
  const body = await req.json().catch(() => null) as
    | { model?: unknown; worker?: unknown; oracle?: unknown; effort?: unknown; sessionId?: unknown }
    | null;
  const model = typeof body?.model === "string" && body.model.trim() ? body.model.trim() : null;
  const worker = typeof body?.worker === "string" && body.worker.trim() ? body.worker.trim() : null;
  const oracle = typeof body?.oracle === "string" && body.oracle.trim() ? body.oracle.trim() : null;
  // Thinking depth: one of EFFORTS, or "default" to fall back to the provider
  // default (clears the pin when a sessionId rides along).
  const effort = typeof body?.effort === "string" && body.effort.trim() ? body.effort.trim() : null;
  const sessionId = typeof body?.sessionId === "string" && body.sessionId ? body.sessionId : null;
  if (!model && !worker && !oracle && !effort) {
    return error(
      400,
      "invalid body: { model?: string, worker?: string, oracle?: string, effort?: string } — at least one required",
    );
  }
  if (effort && effort !== "default" && !(EFFORTS as string[]).includes(effort)) {
    return error(400, `invalid effort: one of ${EFFORTS.join(", ")}, or "default"`);
  }
  if (worker && worker !== "local" && Deno.env.get("BOUGH_WORKER_LOCAL_ONLY") === "1") {
    return error(400, "BOUGH_WORKER_LOCAL_ONLY=1 pins the worker to local");
  }
  if (model) {
    // Validate before mutating anything — a bad sessionId must not half-apply
    // (moving the global default while failing the pin).
    if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "session not found");
    setActiveModel(model);
    persistEnvVar("BOUGH_MODEL", model, ctx.envDir);
    if (sessionId) {
      ctx.db.setSessionModel(sessionId, model);
      const updated = ctx.db.getSession(sessionId)!;
      ctx.bus.publish({ type: "session.updated", sessionId, data: updated });
    }
  }
  if (effort) {
    // Same pinning semantics as model: with a sessionId the session pins AND the
    // global default moves; other sessions keep theirs. "default" clears both.
    if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "session not found");
    const value = effort === "default" ? "" : (effort as Effort);
    setActiveEffort(value);
    persistEnvVar("BOUGH_EFFORT", value, ctx.envDir);
    if (sessionId) {
      ctx.db.setSessionEffort(sessionId, value || null);
      const updated = ctx.db.getSession(sessionId)!;
      ctx.bus.publish({ type: "session.updated", sessionId, data: updated });
    }
  }
  if (oracle) {
    setOracleModel(oracle);
    persistEnvVar("BOUGH_ORACLE", oracle, ctx.envDir);
  }
  if (worker) setWorkerChoice(worker);
  return json({
    model: activeModel(),
    effort: activeEffort(),
    worker: workerChoice(),
    oracle: oracleModel(),
  });
};

// Installed skills (name + description) for composer autocomplete / discovery.
const getSkills: Handler = () => json({ skills: listSkills() });

// Composer ghost text: the worker predicts the user's NEXT message from the
// conversation so far (shown dim on the idle composer, tab accepts). Null
// suggestion = nothing usable (no worker, empty thread) — no ghost, no error.
const postSuggest: Handler = async (req, ctx) => {
  const body = await parseBody(req, SuggestBody);
  const session = ctx.db.getSession(body.sessionId);
  if (!session) return error(404, "session not found");
  const lines = ctx.db.threadFor(session.id)
    .filter((m) => !m.pending)
    .map((m) => ({
      role: m.role === "user" ? "user" as const : "agent" as const,
      // The prose only: tool calls/results are the agent's scratch work, and
      // reasoning is hidden from the user — neither is what they'd reply to.
      text: m.parts.flatMap((p) => p.type === "text" ? [p.text] : []).join("\n").trim(),
    }))
    .filter((l) => l.text.length > 0);
  return json({ suggestion: await suggestNextStep(lines) });
};

const searchFiles: Handler = async (req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  const workspace = ctx.db.getSessionRuntime(session.id).workspace;
  if (!workspace) return json({ files: [] }); // chat-only session — nothing to reference
  const q = new URL(req.url).searchParams.get("q") ?? "";
  const files = await searchWorkspaceFiles(normalizeWorkspace(workspace), q);
  return json({ files });
};

// File autocomplete for a draft conversation (no session yet): search the
// prospective workspace directly, same bounded walk as the per-session route.
const searchDraftFiles: Handler = async (req) => {
  const url = new URL(req.url);
  const dir = url.searchParams.get("dir");
  if (!dir) return error(400, "dir required");
  const q = url.searchParams.get("q") ?? "";
  const files = await searchWorkspaceFiles(normalizeWorkspace(dir), q);
  return json({ files });
};

// Directory autocomplete for the new-session dialog: fuzzy dirs under the query's
// base (fzf-style subsequence), seeded with every workspace a session has ever
// used — but not the per-session worktrees bough itself creates.
const searchDirs: Handler = (req, ctx) => {
  const q = new URL(req.url).searchParams.get("q") ?? "";
  const known = [
    ...new Set([
      ...grantedDirs(),
      ...ctx.db.listSessions()
        .map((s) => s.workspace)
        .filter((w): w is string => !!w && !w.includes("/.bough/workspaces/")),
    ]),
  ];
  return json({ dirs: searchDirectories(q, known) });
};

// Each session carries `busy` (a turn in flight) so the sidebar can show it at a
// glance; the UI keeps it live from message.started/finished events after this read.
const listSessions: Handler = (req, ctx) => {
  const busy = ctx.db.busySessionIds();
  const statuses = ctx.db.latestTurnStatuses();
  // ?archived=1 → the soft-deleted rows only, so a UI can reveal and restore
  // them (they were set archived_at but unreachable over HTTP before).
  const archived = new URL(req.url).searchParams.get("archived") === "1";
  return json(
    (archived ? ctx.db.listArchivedSessions() : ctx.db.listSessions()).map((s) => {
      // Per-row spend (own turns only) — the tree views' cost column.
      const { costUsd } = ctx.db.sessionUsage(s.id);
      return {
        ...s,
        busy: busy.has(s.id),
        ...(statuses.has(s.id) ? { lastTurnStatus: statuses.get(s.id) } : {}),
        ...(costUsd > 0 ? { costUsd } : {}),
      };
    }),
  );
};

const createSession: Handler = async (req, ctx) => {
  const body = await parseBody(req, CreateSessionBody);
  const { title, parentId, kind, workspace: rawWorkspace } = body;
  // Expand `~` and reject a workspace that doesn't exist NOW, with one clear
  // message — otherwise the bad path only surfaces later as per-tool sandbox
  // spawn failures inside the session.
  let workspace: string | undefined;
  if (rawWorkspace) {
    workspace = normalizeWorkspace(rawWorkspace);
    const problem = await workspaceProblem(workspace);
    if (problem) return error(400, problem);
  }
  const session: Session = {
    id: crypto.randomUUID(),
    parentId: parentId ?? null,
    // No title → the placeholder; the title worker names it on the first message.
    title: title ?? UNTITLED,
    kind: kind ?? (parentId ? "fork" : "root"),
    createdAt: Date.now(),
    // Absent when not supplied, so responses/events stay byte-identical (toSession
    // only surfaces workspace when non-null; createSession persists it in one insert).
    // originDir records the project dir permanently — the workspace column gets
    // repointed at the session's shadow worktree on the first turn.
    ...(workspace ? { workspace, originDir: workspace } : {}),
  };
  if (session.parentId && !ctx.db.getSession(session.parentId)) {
    return error(400, `parent ${session.parentId} not found`);
  }
  ctx.db.createSession(session);
  // Model pin (not a createSession column): persist it before the announce so the
  // event and response carry it.
  if (body.model) {
    ctx.db.setSessionModel(session.id, body.model);
    session.model = body.model;
  }
  // Prompt-variant pin (bough exec --prompt-dir / tuner). Like the model pin, it is
  // not a createSession column — persist it so the turn runner reads it per turn.
  if (body.promptDir) {
    ctx.db.setSessionPromptDir(session.id, body.promptDir);
    session.promptDir = body.promptDir;
  }
  ctx.bus.publish({ type: "session.created", sessionId: session.id, data: session });
  return json(session, 201);
};

const getSession: Handler = (_req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  return json({
    session,
    thread: ctx.db.threadFor(session.id),
    usage: {
      ...ctx.db.sessionUsage(session.id),
      contextLimit: usableContextLimit(session.model ?? activeModel()),
      tree: ctx.db.treeUsage(session.id),
    },
  });
};

const postMessage: Handler = async (req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  const body = await parseBody(req, PostMessageBody);

  // A handoff draft is consumed by the first post — whatever the user actually
  // sent (edited or not) supersedes it, so clear it and announce the change.
  if (session.draft != null) {
    ctx.db.setSessionDraft(session.id, null);
    const updated = ctx.db.getSession(session.id);
    if (updated) {
      ctx.bus.publish({ type: "session.updated", sessionId: session.id, data: updated });
    }
  }

  // Persist + announce the user message and run the turn (streams over /events).
  startUserTurn(ctx, session.id, body.text);
  return new Response(null, { status: 202 });
};

// Soft-delete: hide from the sidebar; the row and its thread stay (forks keep
// resolving their ancestor chains). The event lets every open UI drop it live.
const archiveSession: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  // Deleting a conversation stops its work: interrupt any running turn first, which
  // also expires its parked net holds and reaps its proxy (gateway's turn.finished).
  interruptTurn(params.id);
  ctx.db.archiveSession(params.id);
  // The session's sandbox VM is torn down by the session.archived bus subscription
  // (server/main.ts) — one hook covering root sessions and subagents alike.
  ctx.bus.publish({
    type: "session.archived",
    sessionId: params.id,
    data: { sessionId: params.id },
  });
  return json({ ok: true });
};

// Undo the soft-delete: the session returns to GET /sessions. session.updated
// (archivedAt now absent) tells open UIs the row is live again.
const unarchiveSession: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  ctx.db.unarchiveSession(params.id);
  const updated = ctx.db.getSession(params.id)!;
  ctx.bus.publish({ type: "session.updated", sessionId: params.id, data: updated });
  return json({ ok: true });
};

// Persist a session's composer draft (the TUI stashes it on session switch so a
// half-typed prompt stays with its conversation — same column handoff prefills).
// Body {draft: string | null}; null clears. No event on purpose: the writer is
// switching away, and a session.updated here would race the client's prefill.
const putSessionDraft: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const body = await req.json().catch(() => null) as { draft?: string | null } | null;
  if (!body || (body.draft !== null && typeof body.draft !== "string")) {
    return error(400, "body {draft: string | null} required");
  }
  ctx.db.setSessionDraft(params.id, body.draft);
  return json({ ok: true });
};

// Deprecate/un-deprecate a branch: hidden by default in the tree views, still fully
// usable. Body {on: boolean}. A session.updated event carries the new flag.
const deprecateSession: Handler = async (req, ctx, params) => {
  const s = ctx.db.getSession(params.id);
  if (!s) return error(404, "session not found");
  const body = await req.json().catch(() => null) as { on?: boolean } | null;
  if (typeof body?.on !== "boolean") return error(400, "body {on: boolean} required");
  ctx.db.setDeprecated(params.id, body.on);
  const updated = ctx.db.getSession(params.id)!;
  ctx.bus.publish({ type: "session.updated", sessionId: params.id, data: updated });
  return json({ ok: true, deprecated: body.on });
};

// How long an archived session lingers before the long-term purge removes it.
export const PURGE_RETENTION_MS = 30 * 24 * 60 * 60 * 1000;

// Run the long-term purge now (also runs on server boot). `bough purge` hits this.
const purgeArchived: Handler = (_req, ctx) => {
  const purged = ctx.db.purgeArchivedBefore(Date.now() - PURGE_RETENTION_MS);
  return json({ ok: true, purged });
};

const interruptSession: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const stopped = interruptTurn(params.id);
  // 200 whether or not a turn was live — interrupting an idle session is a no-op,
  // not an error (the UI may race the turn finishing).
  return json({ ok: true, interrupted: stopped });
};

// Compaction-as-a-branch: summarize selected turns onto a new compaction session
// (see compact.ts).
const compactSession: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, CompactBody);
  const session = await compact(ctx, params.id, body);
  return json({ session });
};

// Section grouping: LLM-label the client's turn gists into contiguous activity
// sections for the conversation tree's color coding + section selection
// (see sections.ts). Read-only; nothing stored.
const sectionsSession: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, SectionsBody);
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  return json({ sections: await sectionize(ctx, body.turns) });
};

// Extract-to-conversation: copy picked thread messages into a fresh root session
// (see extract.ts).
const extractSession: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, ExtractBody);
  const session = extract(ctx, params.id, body);
  return json({ session });
};

// Handoff: draft a goal-focused opening prompt from this thread and attach it to a
// fresh root conversation as an editable composer draft (see handoff.ts).
const handoffSession: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, HandoffBody);
  // Use the session's model (else the active default) via clientFor — not the
  // Anthropic client hardcoded in handoff.ts. Without this, a handoff on an
  // OpenAI/OpenRouter model (or with no Anthropic key) threw an unhandled error
  // → 500 instead of drafting through the configured provider.
  const model = ctx.model ?? ctx.db.getSession(params.id)?.model ?? activeModel();
  const session = await handoff(
    { ...ctx, llm: ctx.llm ?? clientFor(model), model },
    params.id,
    body,
  );
  return json({ session });
};

// Move (copy) picked messages from a source session onto this existing target.
const moveInto: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, MoveBody);
  const session = move(ctx, params.id, body);
  return json({ session });
};

// Adopt a subagent's branch: fold its diff into its spawner's workspace —
// the UI affordance mirroring the supervisor program's adopt() host function.
const adoptSession: Handler = async (_req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  if (session.kind !== "subagent" || !session.originId) {
    return error(400, "not a subagent session");
  }
  try {
    const message = await adoptSubagent(ctx, session.originId, session.id);
    return json({ message });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// Fork-at-message: branch a new session at a past turn (edit & resend or plain branch).
const forkSession: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, ForkBody);
  const { session } = fork(ctx, params.id, body);
  return json({ session });
};

// ---- changes (review rail) -------------------------------------------------

const emitChangesUpdated = (ctx: AppCtx, sessionId: string) =>
  ctx.bus.publish({ type: "changes.updated", sessionId, data: { sessionId } });

// GET /sessions/:id/metrics → usability metrics derived from stored data (metrics.ts).
const getMetrics: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  return json(sessionMetrics(ctx.db, params.id));
};

// GET /sessions/:id/jobs → live + recent background shells of the session AND its
// subagent branches (the TUI's status-bar chip and live job cards). Each row
// carries its owning sessionId so subagent jobs are attributable.
const getJobs: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const subagents = ctx.db.listSessions()
    .filter((s) => s.kind === "subagent" && s.originId === params.id);
  return json({ jobs: [...listJobs(params.id), ...subagents.flatMap((s) => listJobs(s.id))] });
};

// POST /sessions/:id/jobs/:jobId/kill → SIGTERM a running background shell of
// the session directly (the tool-only bashKill, reachable from the TUI so a kill
// doesn't cost a full LLM turn). bashKill keys the registry off ctx.sessionId.
const killJob: Handler = async (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  try {
    const message = await bashKill(params.jobId, { workspace: "", sessionId: params.id });
    return json({ message });
  } catch (e) {
    return error(404, e instanceof Error ? e.message : String(e));
  }
};

// GET /sessions/:id/changes → { diffs } across active snapshot sources (shadow + clonefile).
const getChanges: Handler = async (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const diffs = await sessionChanges(ctx.db, params.id, { snapshotBase: ctx.snapshotBase });
  return json({ diffs });
};

const applyChangesH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const body = await parseBody(req, ChangesApplyBody);
  const result = await applyChanges(ctx.db, params.id, body, { snapshotBase: ctx.snapshotBase });
  emitChangesUpdated(ctx, params.id);
  return json({ ok: true, source: body.source, ...result });
};

const revertChangesH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const body = await parseBody(req, ChangesRevertBody, {});
  const revertedPaths = await revertChanges(ctx.db, params.id, body.paths);
  emitChangesUpdated(ctx, params.id);
  return json({ ok: true, reverted: "shadow", paths: revertedPaths });
};

// ---- schedules (recurring agent runs — schedules.ts owns validation) --------

const listSchedulesH: Handler = (_req, ctx) => json({ schedules: ctx.db.listSchedules() });

const createScheduleH: Handler = async (req, ctx) => {
  const body = await parseBody(req, ScheduleCreateBody);
  return json(await scheduleCreate(ctx.db, body), 201);
};

const patchScheduleH: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, SchedulePatchBody);
  return json(await schedulePatch(ctx.db, params.id, body));
};

const deleteScheduleH: Handler = (_req, ctx, params) => {
  scheduleRemove(ctx.db, params.id);
  return json({ ok: true });
};

// ---- workflows (scripted multi-agent orchestration — workflow.ts owns the engine)

/** The production WorkflowCtx for REST-started runs: agents spawn as subagents
 * of the run's session, anchored to its latest message on the map. */
function restWorkflowCtx(ctx: AppCtx, sessionId: string) {
  const anchor = ctx.db.threadFor(sessionId).at(-1)?.id ?? `rest:${sessionId}`;
  const model = ctx.db.getSession(sessionId)?.model ?? ctx.model ?? activeModel();
  return workflowCtxFor(ctx, sessionId, anchor, model);
}

const listWorkflowsH: Handler = (req, ctx) => {
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  return json({
    workflows: ctx.db.listWorkflows(sessionId).map((r) => workflowSummary(ctx.db, r)),
  });
};

const getWorkflowH: Handler = (_req, ctx, params) => {
  const run = ctx.db.getWorkflow(params.id);
  if (!run) return error(404, "workflow not found");
  return json({
    workflow: run,
    agents: ctx.db.listWorkflowAgents(run.id),
    scriptFile: workflowScriptPath(run.id),
  });
};

const createWorkflowH: Handler = async (req, ctx) => {
  const body = await parseBody(req, WorkflowCreateBody);
  const run = await startWorkflow(restWorkflowCtx(ctx, body.sessionId), {
    sessionId: body.sessionId,
    script: body.script,
    args: body.args,
  });
  return json(run, 201);
};

const stopWorkflowH: Handler = (_req, ctx, params) => json(stopWorkflow(ctx, params.id));
const pauseWorkflowH: Handler = (_req, ctx, params) => json(pauseWorkflow(ctx, params.id));
const resumeWorkflowH: Handler = (_req, ctx, params) => json(resumeWorkflow(ctx, params.id));

const rerunWorkflowH: Handler = async (req, ctx, params) => {
  const body = await parseBody(req, WorkflowRerunBody, {});
  const src = ctx.db.getWorkflow(params.id);
  if (!src) return error(404, "workflow not found");
  const run = await rerunWorkflow(restWorkflowCtx(ctx, src.sessionId), params.id, body);
  return json(run, 201);
};

const events: Handler = (req, ctx) => {
  const filter = new URL(req.url).searchParams.get("sessionId");
  let unsubscribe: (() => void) | undefined;
  let heartbeat: ReturnType<typeof setInterval> | undefined;
  const enc = new TextEncoder();

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      const send: Listener = (event) => {
        if (filter && event.sessionId && event.sessionId !== filter) return;
        // Named SSE frame: the UI attaches one listener per event type.
        const frame = `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
        try {
          controller.enqueue(enc.encode(frame));
        } catch {
          // stream already closed; the cancel() below will tidy up.
        }
      };
      unsubscribe = ctx.bus.subscribe(send);
      controller.enqueue(enc.encode(": connected\n\n"));
      heartbeat = setInterval(() => {
        try {
          controller.enqueue(enc.encode(": ping\n\n"));
        } catch { /* closed */ }
      }, 15_000);
    },
    cancel() {
      unsubscribe?.();
      if (heartbeat !== undefined) clearInterval(heartbeat);
    },
  });

  return new Response(stream, {
    headers: {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    },
  });
};

// ---- theme -------------------------------------------------------------------

// The UI palette. GET returns the stored theme (null = default) plus the token
// contract + default values so clients and the /theme skill can ground edits.
// PUT validates and persists ~/.bough/theme.json; DELETE reverts to the default.
const getTheme: Handler = (_req, ctx) =>
  json({ theme: loadTheme(ctx.themeDir), tokens: THEME_TOKENS, defaults: THEME_DEFAULTS });

const putTheme: Handler = async (req, ctx) => {
  const parsed = Theme.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid theme: " + parsed.error.message);
  return json({ theme: saveTheme(parsed.data, ctx.themeDir) });
};

const deleteTheme: Handler = (_req, ctx) => {
  clearTheme(ctx.themeDir);
  return json({ theme: null });
};

// ---- net: rail, holds, bundles ---------------------------------------------

// Native egress-proxy status for the Network rail. The feed + approvals live here in
// bough (see /net/requests, /net/policy) — there is no external dashboard.
const netStatus: Handler = (_req, ctx) =>
  json(ctx.gateway?.status() ?? { enabled: false, running: false, listeners: 0, caPath: "" });

// The editable rule set (allow/deny/hold config) the gate compiles + enforces.
// ?session=<id> scopes to that branch: GET returns its effective config plus where it
// came from (own override / inherited from an ancestor / global); PUT writes the
// branch's override row; DELETE removes it (reverting to inheritance). Without the
// param, GET/PUT read and write the global rule set as before.
const getPolicy: Handler = (req, ctx) => {
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  if (!sessionId) return json(loadConfig(ctx.netDir));
  const { config, source } = resolveConfig(ctx.db, sessionId, ctx.netDir);
  return json({ config, source });
};

// Persist a new rule set and hot-swap the live gate so it takes effect on the next
// request — no restart. Rejects a malformed body with the Zod message.
const putPolicy: Handler = async (req, ctx) => {
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  const parsed = NetConfig.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid policy: " + parsed.error.message);
  if (sessionId) {
    if (!ctx.db.getSession(sessionId)) return error(404, "unknown session");
    ctx.db.setNetPolicy(sessionId, JSON.stringify(parsed.data));
    ctx.gate?.invalidate();
    return json({ config: parsed.data, source: { scope: "session", sessionId } });
  }
  const saved = saveConfig(parsed.data, ctx.netDir);
  ctx.gate?.setPolicy(toPolicy(saved));
  return json(saved);
};

// Open a plugin's definition file in Zed on the machine bough runs on.
// Fire-and-forget: zed detaches, we only report spawn failures. Works for broken
// plugins too (their list entry is the filename).
const openPluginH: Handler = (_req, ctx, params) => {
  if (!ctx.gateway) return error(400, "Claw Patrol is off");
  const plugin = ctx.gateway.listPlugins().plugins.find((p) => p.name === params.name);
  if (!plugin) return error(404, `no plugin named "${params.name}"`);
  const argv = ["zed", plugin.file];
  try {
    new Deno.Command(argv[0], { args: argv.slice(1), stdout: "null", stderr: "null" }).spawn()
      .unref();
    return json({ ok: true, file: plugin.file });
  } catch (e) {
    return error(500, `could not launch editor: ${(e as Error).message}`);
  }
};

// Classifier plugins: list (the UI renders each ops table), hot-reload, scaffold a
// starter file, and install a declarative spec. Creation is skill-first: the
// /net-plugin builtin has the in-session agent draft the spec, install it here,
// and live-test the classifications against real traffic.
const listPluginsH: Handler = (_req, ctx) =>
  json(ctx.gateway?.listPlugins() ?? { dir: "", plugins: [] });
const reloadPluginsH: Handler = async (_req, ctx) =>
  json({ plugins: await (ctx.gateway?.reloadPlugins() ?? Promise.resolve([])) });
const createPluginH: Handler = async (req, ctx) => {
  if (!ctx.gateway) return error(400, "Claw Patrol is off");
  const body = await req.json().catch(() => null) as { name?: string } | null;
  const name = body?.name?.trim();
  if (!name) return error(400, "name is required");
  try {
    return json(await ctx.gateway.createPlugin(name));
  } catch (e) {
    return error(409, (e as Error).message);
  }
};

// Install a declarative spec into the plugin LIBRARY (the /net-plugin skill's path).
// The spec is validated and its fixtures re-run before anything touches disk. The
// file gates nothing by itself — turn it on per scope via /net/plugins/:name/enable.
const installPluginH: Handler = async (req, ctx) => {
  if (!ctx.gateway) return error(400, "Claw Patrol is off");
  const body = await req.json().catch(() => null) as { plugin?: unknown } | null;
  if (!body?.plugin) return error(400, "plugin spec is required");
  try {
    return json(await ctx.gateway.installPlugin(body.plugin));
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// "Group into plugin": synthesize a classifier plugin from selected feed requests,
// install it to the library, and enable it for the calling branch (?session=) so it
// gates immediately. Deterministic — each distinct action becomes an op row. The
// user refines the rendered file (✎ Edit) and re-checks fixtures on Reload.
const pluginFromRequestsH: Handler = async (req, ctx) => {
  if (!ctx.gateway) return error(400, "Claw Patrol is off");
  const body = await req.json().catch(() => null) as
    | { requestIds?: string[]; sessionId?: string }
    | null;
  const ids = (body?.requestIds ?? []).filter((id) => typeof id === "string");
  if (ids.length === 0) return error(400, "requestIds required");
  const samples = ctx.db.netEventsByIds(ids).map((r) => ({
    host: r.host,
    verb: r.verb,
    action: r.action,
  }));
  if (samples.length === 0) return error(404, "none of those requests were found");
  const sessionId = body?.sessionId ?? undefined;
  if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "unknown session");
  try {
    const result = await ctx.gateway.pluginFromRequests(samples, (name) => {
      setPluginActivation(ctx.db, sessionId, name, true, undefined, ctx.netDir);
      ctx.gate?.invalidate();
    });
    return json({ ...result, scope: sessionId ? { sessionId } : "global" });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// Turn a library plugin on/off for one scope: ?session=<id> targets that branch
// (copy-on-write override, inherited by its children), no param targets the global
// rule set. Enable takes an optional per-activation `ttl` ("90m" | "2h" | "7d") —
// the same plugin can run open-ended in one branch and lapse on schedule in another.
const setPluginH = (on: boolean): Handler => async (req, ctx, params) => {
  if (!ctx.gateway) return error(400, "Claw Patrol is off");
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "unknown session");
  if (on && !ctx.gateway.hasPlugin(params.name)) {
    return error(400, `no loaded plugin named "${params.name}" — install it first`);
  }
  const body = await req.json().catch(() => null) as { ttl?: string } | null;
  try {
    const expires = on && body?.ttl?.trim() ? ttlToExpires(body.ttl.trim()) : undefined;
    const config = setPluginActivation(ctx.db, sessionId, params.name, on, expires, ctx.netDir);
    // The session row (or policy.json) changed under the gate — refresh either way.
    if (sessionId) ctx.gate?.invalidate();
    else ctx.gate?.setPolicy(toPolicy(config));
    return json({ config, scope: sessionId ? { sessionId } : "global" });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// MCP servers: the registry (~/.bough/mcp/servers.json), this session's live
// connections, and manual activations. Management is skill-first — the /mcp builtin
// drives these over loopback. Registering grants nothing by itself: a skill's `mcp:`
// frontmatter or an enable is what connects a server to a turn (turn.ts), and every
// call is gated through Claw Patrol before the server sees it (mcp/gate.ts).
const getMcpServers: Handler = (req, ctx) => {
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "unknown session");
  return json(mcpStatusFor(sessionId));
};

// Replace the whole registry (GET → edit → PUT, like /net/policy). Only servers
// whose entry changed or vanished lose their live connections — a bulk edit must
// not reset every session's unrelated servers; granting turns reconnect fresh.
const putMcpServers: Handler = async (req) => {
  const body = await req.json().catch(() => null);
  if (!body) return error(400, 'body must be the registry JSON: {"servers":{…}}');
  try {
    const before = loadMcpRegistry().servers;
    const registry = saveMcpRegistry(body);
    const touched = [...new Set([...Object.keys(before), ...Object.keys(registry.servers)])]
      .filter((name) => JSON.stringify(before[name]) !== JSON.stringify(registry.servers[name]));
    for (const name of touched) await mcpManager().dropServer(name);
    return json({ registry });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// Register or update ONE server without round-tripping the registry — the shape
// the /mcp skill uses, so a registration can't mangle sibling entries (or their
// ${VAR} secret references) in a shell read-modify-write.
const putMcpServer: Handler = async (req, _ctx, params) => {
  const body = await req.json().catch(() => null);
  if (!body) return error(400, 'body must be one server entry: {"command":…} or {"url":…}');
  try {
    const registry = upsertMcpServer(params.name, body);
    await mcpManager().dropServer(params.name); // a changed entry can't keep serving
    return json({ registry });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

const deleteMcpServer: Handler = async (_req, _ctx, params) => {
  if (!removeMcpServer(params.name)) {
    return error(404, `no registered mcp server named "${params.name}"`);
  }
  await mcpManager().dropServer(params.name);
  return json({ removed: params.name });
};

// Connect (or reuse) one server for a session RIGHT NOW and report its catalog —
// the validation primitive behind the /mcp skill's "prove it" step. Without this,
// a registration or enable could only be tested by starting another turn; a typo'd
// command surfaced a turn later as UNAVAILABLE. Connecting is not a grant: the
// turn's mcp() bridge still comes only from skills/activations at turn start.
const connectMcpServer: Handler = async (req, ctx, params) => {
  const sessionId = new URL(req.url).searchParams.get("session");
  if (!sessionId) return error(400, "connect is per-session — pass ?session=");
  if (!ctx.db.getSession(sessionId)) return error(404, "unknown session");
  if (!loadMcpRegistry().servers[params.name]) {
    return error(400, `no registered mcp server named "${params.name}" — register it first`);
  }
  try {
    // Same spawn context a turn would use (workspace cwd + snapshot dir), so the
    // probe exercises the REAL seatbelt/proxy confinement, not a lenient variant.
    const prepared = await prepareWorkspace(ctx.db, sessionId);
    const [catalog] = await mcpManager().ensure(sessionId, [params.name], {
      workspace: prepared.cwd,
      sandbox: prepared.sandboxed ? { sessionDir: prepared.sessionDir } : undefined,
    });
    if (catalog.error) return json({ server: params.name, connected: false, error: catalog.error });
    return json({
      server: params.name,
      connected: true,
      status: mcpManager().statuses(sessionId).find((s) => s.server === params.name),
      tools: catalog.tools.map((t) => ({
        name: t.name,
        description: (t.description ?? "").split("\n")[0].trim(),
      })),
    });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

const restartMcpServer: Handler = async (req, ctx, params) => {
  const sessionId = new URL(req.url).searchParams.get("session");
  if (!sessionId) return error(400, "restart is per-session — pass ?session=");
  if (!ctx.db.getSession(sessionId)) return error(404, "unknown session");
  try {
    return json({ status: await mcpManager().restart(sessionId, params.name) });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// OAuth for remote (url) servers. Start: discovery + dynamic registration + PKCE
// happen server-side (mcp/oauth.ts); "redirect" hands back the URL the human must
// open — the /mcp skill (or UI) shows it. The browser lands on GET
// /mcp/oauth/callback below, which validates the state nonce and exchanges the
// code. Tokens live in ~/.bough/mcp/tokens/<name>.json and reach the remote
// transport only.
const startMcpAuth: Handler = async (_req, _ctx, params) => {
  const cfg = loadMcpRegistry().servers[params.name];
  if (!cfg) return error(400, `no registered mcp server named "${params.name}"`);
  if (!cfg.url) return error(400, `"${params.name}" is a stdio server — no OAuth involved`);
  try {
    return json(await beginAuth(params.name, cfg.url));
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

const deleteMcpAuth: Handler = async (_req, _ctx, params) => {
  clearAuth(params.name);
  await mcpManager().dropServer(params.name);
  return json({ authorized: false });
};

// Where the authorization server sends the user's browser back. HTML because a
// human is looking at it; the state nonce (minted by our own provider) is what
// authenticates the flow. On success the tab is done — tokens are stored and the
// next granting turn connects.
const mcpOauthCallback: Handler = async (req) => {
  const q = new URL(req.url).searchParams;
  const page = (status: number, body: string) =>
    new Response(
      `<!doctype html><meta charset="utf-8"><title>bough</title>` +
        `<body style="font-family:system-ui;max-width:32rem;margin:4rem auto">${body}</body>`,
      { status, headers: { "content-type": "text/html" } },
    );
  const err = q.get("error");
  if (err) {
    return page(
      400,
      `<h2>Authorization failed</h2><p>${err}: ${q.get("error_description") ?? ""}</p>`,
    );
  }
  const code = q.get("code");
  const state = q.get("state");
  if (!code || !state) return page(400, "<h2>Missing code or state</h2>");
  try {
    const server = await completeAuth(
      state,
      code,
      (name) => loadMcpRegistry().servers[name]?.url,
    );
    return page(
      200,
      `<h2>bough is connected to "${server}"</h2><p>You can close this tab and return to bough.</p>`,
    );
  } catch (e) {
    return page(400, `<h2>Authorization failed</h2><p>${(e as Error).message}</p>`);
  }
};

// Manual activation for a scope (?session=<id>, or global without it) — the grant
// path that doesn't require authoring a skill; it still enters through the
// human-typed /mcp invocation. Enable takes an optional ttl ("90m" | "2h" | "7d");
// a lapsed activation fails closed. Servers connect at turn start, so an enable
// takes effect on the session's next turn.
const setMcpServer = (on: boolean): Handler => async (req, ctx, params) => {
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "unknown session");
  if (on && !loadMcpRegistry().servers[params.name]) {
    return error(400, `no registered mcp server named "${params.name}" — PUT /mcp/servers first`);
  }
  const body = await req.json().catch(() => null) as { ttl?: string } | null;
  try {
    const expires = on && body?.ttl?.trim() ? ttlToExpires(body.ttl.trim()) : undefined;
    setMcpActivation(sessionId, params.name, on, expires);
    if (!on && sessionId) await mcpManager().drop(sessionId, params.name);
    return json({
      active: mcpActivationsFor(sessionId),
      scope: sessionId ? { sessionId } : "global",
    });
  } catch (e) {
    return error(400, (e as Error).message);
  }
};

// Remove a branch's override so it inherits again (no-op if it had none).
const deletePolicy: Handler = (req, ctx) => {
  const sessionId = new URL(req.url).searchParams.get("session");
  if (!sessionId) return error(400, "?session= is required (the global rule set can't be deleted)");
  ctx.db.deleteNetPolicy(sessionId);
  ctx.gate?.invalidate();
  return json({ ok: true });
};

// Flip YOLO (log-only, no gating) for one scope — the Network rail's red button.
// ?session=<id> targets that branch (copy-on-write override, inherited by children);
// no param targets the global rule set. Toggling off restores the pre-yolo mode.
const setYoloH: Handler = async (req, ctx) => {
  if (!ctx.gateway) return error(400, "Claw Patrol is off");
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  if (sessionId && !ctx.db.getSession(sessionId)) return error(404, "unknown session");
  const body = await req.json().catch(() => null) as { on?: boolean } | null;
  if (typeof body?.on !== "boolean") return error(400, "body {on: boolean} required");
  const config = setYolo(ctx.db, sessionId, body.on, ctx.netDir);
  if (sessionId) ctx.gate?.invalidate();
  else ctx.gate?.setPolicy(toPolicy(config));
  // Flipping YOLO on no longer silently auto-approves requests a human is already
  // looking at — parked holds stay parked for an explicit decision. YOLO only affects
  // requests decided AFTER the flip.
  return json({ config, scope: sessionId ? { sessionId } : "global" });
};

// Recent NetRequest rows for the Network rail (optionally per-session).
const netRequests: Handler = (req, ctx) => {
  const sessionId = new URL(req.url).searchParams.get("sessionId") ?? undefined;
  return json(ctx.db.recentNetEvents(sessionId));
};

// Resolve a held request: the gate's awaiting Promise settles and the row/event flip
// to allowed|denied. A `pending` row with no live hold behind it (stale — its turn or
// server died) is healed in place instead of 404-looping the approval card forever.
const resolveHold = (approve: boolean): Handler => (req, ctx, params) => {
  // ?scope=session mints a short-TTL host+verb grant so the retried command passes
  // (used by "allow for session" and by any approval of a timed-out hold).
  const scope = new URL(req.url).searchParams.get("scope") === "session" ? "session" : "once";
  if (ctx.gate?.resolveHold(params.id, approve, scope)) {
    return json({ ok: true, id: params.id, verdict: approve ? "allowed" : "denied", scope });
  }
  const row = ctx.db.netEventsByIds([params.id])[0];
  if (row?.verdict === "pending") {
    const healed = {
      ...row,
      verdict: "denied" as const,
      reason: "expired — request was no longer waiting",
      ts: Date.now(),
    };
    ctx.db.recordNetEvent(row.sessionId, healed);
    ctx.bus.publish({ type: "net.request", sessionId: row.sessionId, data: healed });
    return json({ ok: true, id: params.id, verdict: "denied", stale: true });
  }
  return error(404, "no request awaiting approval for that id");
};

// ---- ask() questions -------------------------------------------------------
// Pending mid-task questions (asks.ts holds) — the ask() mirror of the net-hold
// routes. Memory-only: GET rebuilds a freshly-attached client's hold card (like
// /net/requests rebuilds the rail); POST settles one and the program resumes.

// GET /questions[?sessionId=] → pending AskQuestions, oldest first.
const listQuestionsH: Handler = (req) => {
  const sessionId = new URL(req.url).searchParams.get("sessionId") ?? undefined;
  return json(pendingAsks(sessionId));
};

// POST /sessions/:id/questions/:qid — {answer} resolves the program's ask();
// {decline: true} rejects it with a catchable "user declined" error.
const answerQuestionH: Handler = async (req, _ctx, params) => {
  const q = getAsk(params.qid);
  if (!q || q.sessionId !== params.id) {
    return error(404, "no question awaiting an answer for that id");
  }
  const body = AnswerQuestionBody.safeParse(await req.json().catch(() => null));
  if (!body.success) return error(400, "body {answer: string} or {decline: true} required");
  if (body.data.decline === true) {
    declineAsk(params.qid);
    return json({ ok: true, id: params.qid, status: "declined" });
  }
  if (typeof body.data.answer !== "string" || !body.data.answer.trim()) {
    return error(400, "body {answer: string} or {decline: true} required");
  }
  answerAsk(params.qid, body.data.answer);
  return json({ ok: true, id: params.qid, status: "answered" });
};

function bundleSummary(m: BundleManifest, dir: string | undefined) {
  return {
    name: m.name,
    version: m.version,
    description: m.description,
    params: m.params,
    credentials: m.credentials,
    installed: isInstalled(m.name, dir),
  };
}

const listBundlesH: Handler = (_req, ctx) =>
  json(listBundles().map((m) => bundleSummary(m, ctx.netDir)));

const getBundleH: Handler = (_req, ctx, params) => {
  const m = getBundle(params.name);
  if (!m) return error(404, "bundle not found");
  // Include fixtures here (the detail view / dry-run reference); render() is a fn, omitted.
  return json({ ...bundleSummary(m, ctx.netDir), fixtures: m.fixtures });
};

const installBundleH: Handler = async (req, ctx, params) => {
  const m = getBundle(params.name);
  if (!m) return error(404, "bundle not found");
  const body = await req.json().catch(() => ({}));
  const rawParams =
    (body && typeof body === "object" && "params" in body
      ? (body as { params?: Record<string, unknown> }).params
      : undefined) ?? {};
  try {
    const result = ctx.netDir
      ? installBundle(m, rawParams, ctx.netDir)
      : installBundle(m, rawParams);
    // The bundle merged into the rule set — hot-swap the live gate so it takes effect now.
    ctx.gate?.setPolicy(toPolicy(loadConfig(ctx.netDir)));
    return json(result);
  } catch (e) {
    if (e instanceof InstallError) return json({ error: e.message, detail: e.detail }, 400);
    throw e;
  }
};

// List a session's published artifacts (server/artifacts.ts). Filesystem-backed, so
// it survives restarts with no DB row; the TUI fetches this on demand.
const listArtifactsH: Handler = async (_req, _ctx, params) =>
  json({ artifacts: await listArtifacts(params.id) });

// Serve one hosted artifact by path (rendered HTML/JS/CSS/…). Same origin as the UI so
// links open in the browser; traversal + bad ids are rejected inside serveArtifact.
// ?raw=1 skips the viewer wrapper on *.ui.json spec artifacts.
const getArtifact: Handler = (req, _ctx, params) =>
  serveArtifact(params.id, params.path ?? "", undefined, {
    raw: new URL(req.url).searchParams.get("raw") === "1",
  });

// The spec-viewer bundle every *.ui.json wrapper page loads (jsonrender/bundle.ts).
// Built lazily and cached by source hash; the hash is the ETag so browsers revalidate
// with a cheap 304 while a changed viewer lands immediately.
const getViewerJs: Handler = async (req) => {
  const { js, etag } = await viewerBundle();
  if (req.headers.get("if-none-match") === `"${etag}"`) {
    return new Response(null, { status: 304, headers: { etag: `"${etag}"` } });
  }
  return new Response(js, {
    headers: {
      "content-type": "text/javascript; charset=utf-8",
      "cache-control": "no-cache",
      etag: `"${etag}"`,
    },
  });
};

// ---- artifact comments -----------------------------------------------------
// The comment layer injected into every served HTML artifact (comments.ts) talks
// to these same-origin endpoints. Notes are the user's margin annotations left
// for the agent; "send" wakes the session so the agent reads them.

// GET /sessions/:id/comments[?artifact=] → this session's artifact comments.
const listComments: Handler = (req, _ctx, params) => {
  const artifact = new URL(req.url).searchParams.get("artifact");
  const all = loadComments(params.id);
  return json({ comments: artifact ? all.filter((c) => c.artifact === artifact) : all });
};

// POST /sessions/:id/comments → add one note (from the injected widget).
const addCommentH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const body = await parseBody(req, AddCommentBody);
  return json(addComment(params.id, body), 201);
};

// DELETE /sessions/:id/comments/:cid → remove a note.
const deleteCommentH: Handler = (_req, _ctx, params) =>
  deleteComment(params.id, params.cid) ? json({ ok: true }) : error(404, "comment not found");

// POST /sessions/:id/comments/send → deliver the unsent notes to the agent as a
// system note (waking a turn) and mark them sent. One turn per batch, not per note.
const sendCommentsH: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const unsent = loadComments(params.id).filter((c) => !c.sent);
  if (unsent.length === 0) return json({ sent: 0 });
  postSystemNote(ctx, params.id, formatForAgent(unsent));
  markSent(params.id, unsent.map((c) => c.id));
  return json({ sent: unsent.length });
};

// ---- route table + dispatch ------------------------------------------------

// Matched on pathname only (URLPattern rejects an init object + base together).
const routes: Route[] = [
  { method: "GET", pattern: new URLPattern({ pathname: "/config" }), handler: getConfig },
  { method: "PATCH", pattern: new URLPattern({ pathname: "/config" }), handler: patchConfig },
  { method: "PUT", pattern: new URLPattern({ pathname: "/config/keys" }), handler: putKeys },
  { method: "DELETE", pattern: new URLPattern({ pathname: "/config/keys" }), handler: deleteKeys },
  { method: "GET", pattern: new URLPattern({ pathname: "/skills" }), handler: getSkills },
  { method: "POST", pattern: new URLPattern({ pathname: "/suggest" }), handler: postSuggest },
  { method: "GET", pattern: new URLPattern({ pathname: "/theme" }), handler: getTheme },
  { method: "PUT", pattern: new URLPattern({ pathname: "/theme" }), handler: putTheme },
  { method: "DELETE", pattern: new URLPattern({ pathname: "/theme" }), handler: deleteTheme },
  { method: "GET", pattern: new URLPattern({ pathname: "/sessions" }), handler: listSessions },
  { method: "GET", pattern: new URLPattern({ pathname: "/fs/dirs" }), handler: searchDirs },
  { method: "GET", pattern: new URLPattern({ pathname: "/fs/files" }), handler: searchDraftFiles },
  { method: "POST", pattern: new URLPattern({ pathname: "/sessions" }), handler: createSession },
  { method: "GET", pattern: new URLPattern({ pathname: "/sessions/:id" }), handler: getSession },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/messages" }),
    handler: postMessage,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/archive" }),
    handler: archiveSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/unarchive" }),
    handler: unarchiveSession,
  },
  {
    method: "PUT",
    pattern: new URLPattern({ pathname: "/sessions/:id/draft" }),
    handler: putSessionDraft,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/deprecate" }),
    handler: deprecateSession,
  },
  { method: "POST", pattern: new URLPattern({ pathname: "/purge" }), handler: purgeArchived },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/interrupt" }),
    handler: interruptSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/compact" }),
    handler: compactSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/sections" }),
    handler: sectionsSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/extract" }),
    handler: extractSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/handoff" }),
    handler: handoffSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/move-into" }),
    handler: moveInto,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/fork" }),
    handler: forkSession,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/adopt" }),
    handler: adoptSession,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/sessions/:id/files" }),
    handler: searchFiles,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/sessions/:id/artifacts" }),
    handler: listArtifactsH,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/sessions/:id/comments" }),
    handler: listComments,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/comments" }),
    handler: addCommentH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/comments/send" }),
    handler: sendCommentsH,
  },
  {
    method: "DELETE",
    pattern: new URLPattern({ pathname: "/sessions/:id/comments/:cid" }),
    handler: deleteCommentH,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/sessions/:id/changes" }),
    handler: getChanges,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/sessions/:id/metrics" }),
    handler: getMetrics,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/sessions/:id/jobs" }),
    handler: getJobs,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/jobs/:jobId/kill" }),
    handler: killJob,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/changes/apply" }),
    handler: applyChangesH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/changes/revert" }),
    handler: revertChangesH,
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/questions" }), handler: listQuestionsH },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/sessions/:id/questions/:qid" }),
    handler: answerQuestionH,
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/schedules" }), handler: listSchedulesH },
  { method: "POST", pattern: new URLPattern({ pathname: "/schedules" }), handler: createScheduleH },
  {
    method: "PATCH",
    pattern: new URLPattern({ pathname: "/schedules/:id" }),
    handler: patchScheduleH,
  },
  {
    method: "DELETE",
    pattern: new URLPattern({ pathname: "/schedules/:id" }),
    handler: deleteScheduleH,
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/workflows" }), handler: listWorkflowsH },
  { method: "POST", pattern: new URLPattern({ pathname: "/workflows" }), handler: createWorkflowH },
  { method: "GET", pattern: new URLPattern({ pathname: "/workflows/:id" }), handler: getWorkflowH },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/workflows/:id/stop" }),
    handler: stopWorkflowH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/workflows/:id/pause" }),
    handler: pauseWorkflowH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/workflows/:id/resume" }),
    handler: resumeWorkflowH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/workflows/:id/rerun" }),
    handler: rerunWorkflowH,
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/events" }), handler: events },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/status" }), handler: netStatus },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/policy" }), handler: getPolicy },
  { method: "PUT", pattern: new URLPattern({ pathname: "/net/policy" }), handler: putPolicy },
  { method: "DELETE", pattern: new URLPattern({ pathname: "/net/policy" }), handler: deletePolicy },
  { method: "POST", pattern: new URLPattern({ pathname: "/net/yolo" }), handler: setYoloH },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/plugins" }), handler: listPluginsH },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins/reload" }),
    handler: reloadPluginsH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins/:name/open" }),
    handler: openPluginH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins" }),
    handler: createPluginH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins/install" }),
    handler: installPluginH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins/from-requests" }),
    handler: pluginFromRequestsH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins/:name/enable" }),
    handler: setPluginH(true),
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/plugins/:name/disable" }),
    handler: setPluginH(false),
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/mcp/servers" }), handler: getMcpServers },
  { method: "PUT", pattern: new URLPattern({ pathname: "/mcp/servers" }), handler: putMcpServers },
  {
    method: "PUT",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name" }),
    handler: putMcpServer,
  },
  {
    method: "DELETE",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name" }),
    handler: deleteMcpServer,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name/connect" }),
    handler: connectMcpServer,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name/restart" }),
    handler: restartMcpServer,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name/enable" }),
    handler: setMcpServer(true),
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name/disable" }),
    handler: setMcpServer(false),
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name/auth" }),
    handler: startMcpAuth,
  },
  {
    method: "DELETE",
    pattern: new URLPattern({ pathname: "/mcp/servers/:name/auth" }),
    handler: deleteMcpAuth,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/mcp/oauth/callback" }),
    handler: mcpOauthCallback,
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/requests" }), handler: netRequests },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/requests/:id/allow" }),
    handler: resolveHold(true),
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/requests/:id/deny" }),
    handler: resolveHold(false),
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/bundles" }), handler: listBundlesH },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/net/bundles/:name" }),
    handler: getBundleH,
  },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/bundles/:name/install" }),
    handler: installBundleH,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: "/artifacts/:id/:path*" }),
    handler: getArtifact,
  },
  {
    method: "GET",
    pattern: new URLPattern({ pathname: VIEWER_JS_PATH }),
    handler: getViewerJs,
  },
];

/** Build the fetch handler bound to a ctx (used by main.ts and by tests). */
export function createHandler(ctx: AppCtx): (req: Request) => Response | Promise<Response> {
  const auth = createAuth(ctx.password);
  // Background-shell lifecycle (bash_bg.ts) → bus, so the TUI hears job spawns
  // and exits live (status-bar chip + job cards) without polling blind.
  onJobEvent((ev) => ctx.bus.publish({ type: ev.type, sessionId: ev.sessionId, data: ev.job }));
  return async (req) => {
    const denied = await auth.gate(req);
    if (denied) return denied;
    const { pathname } = new URL(req.url);
    for (const route of routes) {
      if (route.method !== req.method) continue;
      const match = route.pattern.exec({ pathname });
      if (match) {
        try {
          return await route.handler(req, ctx, match.pathname.groups as Record<string, string>);
        } catch (e) {
          // Domain errors (HttpError subclasses) carry their response; this one
          // catch replaces a per-handler catch block per error type.
          if (e instanceof HttpError) return error(e.status, e.message);
          throw e;
        }
      }
    }
    // No web UI — bough is driven through the TUI; the server is API + artifacts.
    if (req.method === "GET" && pathname === "/") {
      return new Response(
        "bough server — drive it with the `bough` TUI. Artifacts: /artifacts/<sessionId>/<name>\n",
        { headers: { "content-type": "text/plain; charset=utf-8" } },
      );
    }
    return error(404, "not found");
  };
}
