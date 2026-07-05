/**
 * The HTTP surface: a tiny hand-rolled router over a single route table, plus the SSE
 * endpoint that tails the event bus. No framework — the table is a list of
 * {method, URLPattern, handler}, matched in order, so an OpenAPI doc can be generated
 * from it later. Bodies are Zod-validated at the edge; handlers work in domain types.
 *
 * Endpoints (contract mirrored by web/src/api.ts + useEvents.ts):
 *   GET  /sessions                 → Session[]
 *   POST /sessions                 → Session            {title, parentId?, kind?}
 *   GET  /sessions/:id             → {session, thread}  (thread = root→self messages)
 *   POST /sessions/:id/messages    → 202                {text}  (persist + start turn)
 *   GET|POST /events[?sessionId=]  → SSE stream of BoughEvent (named events + heartbeat)
 *
 * CORS is permissive (localhost dev; Vite proxies but standalone must work too).
 */
import { CreateSessionBody, PostMessageBody, type Session } from "../schema/parts.ts";
import type { Db } from "../db/db.ts";
import type { Bus, Listener } from "../bus.ts";
import { activeModel, interruptTurn, MODELS, setActiveModel, startUserTurn } from "../turn.ts";
import { normalizeWorkspace, workspaceProblem } from "../supervisor/workspace.ts";
import { UNTITLED } from "../supervisor/title.ts";
import { listSkills } from "../supervisor/skills.ts";
import { searchWorkspaceFiles } from "./files.ts";
import { fork, ForkBody, ForkError } from "../fork.ts";
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
import { suggestPolicy } from "../net/suggest.ts";
import {
  activationsFor as mcpActivationsFor,
  loadRegistry as loadMcpRegistry,
  saveRegistry as saveMcpRegistry,
  setActivation as setMcpActivation,
} from "../mcp/config.ts";
import { mcpManager } from "../mcp/manager.ts";
import { beginAuth, clearAuth, completeAuth, hasTokens } from "../mcp/oauth.ts";
import { clientFor } from "../supervisor/llm.ts";
import { defaultWebDir, serveWeb } from "./static.ts";
import { createAuth } from "./auth.ts";
import { compact, CompactBody, CompactError } from "../compact.ts";
import { extract, ExtractBody, ExtractError } from "../extract.ts";
import { adoptSubagent } from "../subagent.ts";
import type { LlmClient } from "../supervisor/llm.ts";
import { applyChanges, ChangesError, revertChanges, sessionChanges } from "./changes.ts";
import { ChangesApplyBody, ChangesRevertBody } from "../schema/changes.ts";
import { clearTheme, loadTheme, saveTheme, Theme, THEME_DEFAULTS, THEME_TOKENS } from "./theme.ts";

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
  /** Built web UI dir override (tests/packaging); undefined = web/dist. */
  webDir?: string;
  /** LLM client for compaction/turns; injected for tests, else the real Anthropic client. */
  llm?: LlmClient;
  /** Model override; else BOUGH_MODEL, else the default. */
  model?: string;
  /** clonefile snapshot root override (tests); else BOUGH_SNAPSHOT_BASE / default. */
  snapshotBase?: string;
  /** When set, every request requires a login session (see auth.ts). From BOUGH_PASSWORD. */
  password?: string;
}

type Handler = (
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
) => Response | Promise<Response>;
type Route = { method: string; pattern: URLPattern; handler: Handler };

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, POST, PATCH, PUT, DELETE, OPTIONS",
  "access-control-allow-headers": "content-type",
};

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...CORS },
  });
}

function error(status: number, message: string): Response {
  return json({ error: message }, status);
}

// ---- handlers --------------------------------------------------------------

const getConfig: Handler = () => json({ model: activeModel(), models: MODELS });

// Switch the model new turns run on. Any id is accepted (the picker lists a curated
// subset); a provider-prefixed id routes to OpenRouter (see turn.ts / llm.ts).
const patchConfig: Handler = async (req) => {
  const body = await req.json().catch(() => null) as { model?: unknown } | null;
  if (!body || typeof body.model !== "string" || !body.model.trim()) {
    return error(400, "invalid body: { model: string } required");
  }
  setActiveModel(body.model.trim());
  return json({ model: activeModel() });
};

// Installed skills (name + description) for composer autocomplete / discovery.
const getSkills: Handler = () => json({ skills: listSkills() });

const searchFiles: Handler = async (req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  const workspace = ctx.db.getSessionRuntime(session.id).workspace;
  if (!workspace) return json({ files: [] }); // chat-only session — nothing to reference
  const q = new URL(req.url).searchParams.get("q") ?? "";
  const files = await searchWorkspaceFiles(normalizeWorkspace(workspace), q);
  return json({ files });
};

// Each session carries `busy` (a turn in flight) so the sidebar can show it at a
// glance; the UI keeps it live from message.started/finished events after this read.
const listSessions: Handler = (_req, ctx) => {
  const busy = ctx.db.busySessionIds();
  const statuses = ctx.db.latestTurnStatuses();
  return json(
    ctx.db.listSessions().map((s) => ({
      ...s,
      busy: busy.has(s.id),
      ...(statuses.has(s.id) ? { lastTurnStatus: statuses.get(s.id) } : {}),
    })),
  );
};

const createSession: Handler = async (req, ctx) => {
  const parsed = CreateSessionBody.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);
  const { title, parentId, kind, workspace: rawWorkspace } = parsed.data;
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
    ...(workspace ? { workspace } : {}),
  };
  if (session.parentId && !ctx.db.getSession(session.parentId)) {
    return error(400, `parent ${session.parentId} not found`);
  }
  ctx.db.createSession(session);
  ctx.bus.publish({ type: "session.created", sessionId: session.id, data: session });
  return json(session, 201);
};

const getSession: Handler = (_req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  return json({
    session,
    thread: ctx.db.threadFor(session.id),
    usage: { ...ctx.db.sessionUsage(session.id), tree: ctx.db.treeUsage(session.id) },
  });
};

const postMessage: Handler = async (req, ctx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) return error(404, "session not found");
  const parsed = PostMessageBody.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);

  // Persist + announce the user message and run the turn (streams over /events).
  startUserTurn(ctx, session.id, parsed.data.text);
  return new Response(null, { status: 202, headers: CORS });
};

// Soft-delete: hide from the sidebar; the row and its thread stay (forks keep
// resolving their ancestor chains). The event lets every open UI drop it live.
const archiveSession: Handler = (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  // Deleting a conversation stops its work: interrupt any running turn first, which
  // also expires its parked net holds and reaps its proxy (gateway's turn.finished).
  interruptTurn(params.id);
  ctx.db.archiveSession(params.id);
  ctx.bus.publish({
    type: "session.archived",
    sessionId: params.id,
    data: { sessionId: params.id },
  });
  return json({ ok: true });
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
  const parsed = CompactBody.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);
  try {
    const session = await compact(ctx, params.id, parsed.data);
    return json({ session });
  } catch (e) {
    if (e instanceof CompactError) return error(e.status, e.message);
    throw e;
  }
};

// Extract-to-conversation: copy picked thread messages into a fresh root session
// (see extract.ts).
const extractSession: Handler = async (req, ctx, params) => {
  const parsed = ExtractBody.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);
  try {
    const session = extract(ctx, params.id, parsed.data);
    return json({ session });
  } catch (e) {
    if (e instanceof ExtractError) return error(e.status, e.message);
    throw e;
  }
};

// Adopt a subagent's branch: squash its jj change into its spawner's workspace —
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
  const parsed = ForkBody.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);
  try {
    const { session } = fork(ctx, params.id, parsed.data);
    return json({ session });
  } catch (e) {
    if (e instanceof ForkError) return error(e.status, e.message);
    throw e;
  }
};

// ---- changes (review rail) -------------------------------------------------

const emitChangesUpdated = (ctx: AppCtx, sessionId: string) =>
  ctx.bus.publish({ type: "changes.updated", sessionId, data: { sessionId } });

// GET /sessions/:id/changes → { diffs } across active snapshot sources (jj + clonefile).
const getChanges: Handler = async (_req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const diffs = await sessionChanges(ctx.db, params.id, { snapshotBase: ctx.snapshotBase });
  return json({ diffs });
};

const applyChangesH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const parsed = ChangesApplyBody.safeParse(await req.json().catch(() => null));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);
  try {
    await applyChanges(ctx.db, params.id, parsed.data, { snapshotBase: ctx.snapshotBase });
  } catch (e) {
    if (e instanceof ChangesError) return error(e.status, e.message);
    throw e;
  }
  emitChangesUpdated(ctx, params.id);
  return json({ ok: true, source: parsed.data.source, applied: parsed.data.paths });
};

const revertChangesH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) return error(404, "session not found");
  const parsed = ChangesRevertBody.safeParse(await req.json().catch(() => ({})));
  if (!parsed.success) return error(400, "invalid body: " + parsed.error.message);
  try {
    await revertChanges(ctx.db, params.id);
    emitChangesUpdated(ctx, params.id);
    return json({ ok: true, reverted: "jj" });
  } catch (e) {
    if (e instanceof ChangesError) return error(e.status, e.message);
    throw e;
  }
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
      ...CORS,
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

// Re-run the macOS keychain-trust check after the operator runs the trust command,
// so the CA hint clears without a server restart.
const recheckCaH: Handler = async (_req, ctx) => {
  await ctx.gateway?.refreshCaTrust();
  return json(
    ctx.gateway?.status() ?? { enabled: false, running: false, listeners: 0, caPath: "" },
  );
};

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

// Draft a rule set with the model: intent in, proposed NetConfig + rationale out.
// Nothing is enforced here — the proposal lands in the rule editor for review, and
// only the user's Save (PUT /net/policy) makes it live. With ?session semantics in
// the body, the proposal starts from that branch's effective config and sees its
// recent egress (including any pending hold) as context.
const suggestPolicyH: Handler = async (req, ctx) => {
  const body = await req.json().catch(() => null) as
    | { prompt?: string; sessionId?: string; requestIds?: string[] }
    | null;
  const selected = Array.isArray(body?.requestIds) && body.requestIds.length
    ? ctx.db.netEventsByIds(body.requestIds.filter((id) => typeof id === "string"))
    : undefined;
  // Grouping feed rows into rules needs no prose — a sensible default intent kicks in.
  const intent = body?.prompt?.trim() ||
    (selected?.length
      ? "Group the selected requests into rules that allow this kind of traffic, " +
        "generalizing no further than the pattern they form. Keep everything else as strict as the base config."
      : "");
  if (!intent) return error(400, "prompt or requestIds required");
  const sessionId = body?.sessionId ?? undefined;
  const base = sessionId
    ? resolveConfig(ctx.db, sessionId, ctx.netDir).config
    : loadConfig(ctx.netDir);
  const recent = ctx.db.recentNetEvents(sessionId, 20);
  const llm = ctx.llm ?? clientFor(activeModel());
  try {
    const suggestion = await suggestPolicy({
      llm,
      model: activeModel(),
      intent,
      base,
      recent,
      selected,
    });
    // Plugin activations are managed by their own enable/disable flow — a drafted
    // rule set must never silently drop (or invent) them, so pin the base's.
    suggestion.config.plugins = base.plugins;
    return json(suggestion);
  } catch (e) {
    return error(502, (e as Error).message);
  }
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
    const result = await ctx.gateway.pluginFromRequests(samples, sessionId, (name) => {
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
  const registry = loadMcpRegistry();
  // Remote (url) servers carry OAuth state so /mcp status can say "not authorized".
  const auth = Object.fromEntries(
    Object.entries(registry.servers)
      .filter(([, cfg]) => cfg.url)
      .map(([name]) => [name, { authorized: hasTokens(name) }]),
  );
  return json({
    registry,
    auth,
    active: mcpActivationsFor(sessionId),
    connections: mcpManager().statuses(sessionId),
  });
};

// Replace the whole registry (GET → edit → PUT, like /net/policy). Live connections
// are dropped so a changed entry can't keep serving; granting turns reconnect fresh.
const putMcpServers: Handler = async (req) => {
  const body = await req.json().catch(() => null);
  if (!body) return error(400, 'body must be the registry JSON: {"servers":{…}}');
  try {
    const registry = saveMcpRegistry(body);
    await mcpManager().dropAll();
    return json({ registry });
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
const resolveHold = (approve: boolean): Handler => (_req, ctx, params) => {
  if (ctx.gate?.resolveHold(params.id, approve)) {
    return json({ ok: true, id: params.id, verdict: approve ? "allowed" : "denied" });
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

// ---- route table + dispatch ------------------------------------------------

// Matched on pathname only (URLPattern rejects an init object + base together).
const routes: Route[] = [
  { method: "GET", pattern: new URLPattern({ pathname: "/config" }), handler: getConfig },
  { method: "PATCH", pattern: new URLPattern({ pathname: "/config" }), handler: patchConfig },
  { method: "GET", pattern: new URLPattern({ pathname: "/skills" }), handler: getSkills },
  { method: "GET", pattern: new URLPattern({ pathname: "/theme" }), handler: getTheme },
  { method: "PUT", pattern: new URLPattern({ pathname: "/theme" }), handler: putTheme },
  { method: "DELETE", pattern: new URLPattern({ pathname: "/theme" }), handler: deleteTheme },
  { method: "GET", pattern: new URLPattern({ pathname: "/sessions" }), handler: listSessions },
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
    pattern: new URLPattern({ pathname: "/sessions/:id/extract" }),
    handler: extractSession,
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
    pattern: new URLPattern({ pathname: "/sessions/:id/changes" }),
    handler: getChanges,
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
  { method: "GET", pattern: new URLPattern({ pathname: "/events" }), handler: events },
  // POST variant for tunneled use: Cloudflare quick tunnels buffer GET event-streams
  // until the connection closes (cloudflared#1449) but stream POST bodies live. The
  // web client always uses POST; GET stays for curl and local tools.
  { method: "POST", pattern: new URLPattern({ pathname: "/events" }), handler: events },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/status" }), handler: netStatus },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/ca/recheck" }),
    handler: recheckCaH,
  },
  { method: "GET", pattern: new URLPattern({ pathname: "/net/policy" }), handler: getPolicy },
  { method: "PUT", pattern: new URLPattern({ pathname: "/net/policy" }), handler: putPolicy },
  { method: "DELETE", pattern: new URLPattern({ pathname: "/net/policy" }), handler: deletePolicy },
  { method: "POST", pattern: new URLPattern({ pathname: "/net/yolo" }), handler: setYoloH },
  {
    method: "POST",
    pattern: new URLPattern({ pathname: "/net/policy/suggest" }),
    handler: suggestPolicyH,
  },
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
];

/** Build the fetch handler bound to a ctx (used by main.ts and by tests). */
export function createHandler(ctx: AppCtx): (req: Request) => Response | Promise<Response> {
  const webDir = ctx.webDir ?? defaultWebDir();
  const auth = createAuth(ctx.password);
  return async (req) => {
    if (req.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
    const denied = await auth.gate(req);
    if (denied) return denied;
    const { pathname } = new URL(req.url);
    for (const route of routes) {
      if (route.method !== req.method) continue;
      const match = route.pattern.exec({ pathname });
      if (match) return route.handler(req, ctx, match.pathname.groups as Record<string, string>);
    }
    // Unmatched GETs fall through to the built web UI (file, else SPA index).
    if (req.method === "GET") return serveWeb(req, webDir);
    return error(404, "not found");
  };
}
