/**
 * One shape for "what MCP state is true right now", and the HTTP surface that reads
 * and changes it.
 *
 * THE INVARIANT THIS HOLDS: **there is exactly one builder, and it never serves a
 * cached answer.** `mcpStatus()` inside a program, `GET /mcp/servers` in the TUI's
 * MCP tab, and the response to every mutation below all come from `mcpStatusFor` —
 * so the model and the human cannot be looking at different MCP states, and neither
 * can be looking at a stale one. Every call re-reads the registry file, re-resolves
 * the grant, re-reads the credential store and re-reads the live connections (plan
 * §6.13). The prompt tells the model to answer every MCP question from a FRESH call
 * precisely because grants and connections change between turns — a memo here would
 * make that instruction a lie, and the model's confident answer would be wrong in
 * the one way it cannot detect.
 *
 * THE FOUR KEYS ARE FIXED. `{registry, auth, active, connections}` is what
 * `prompt/mcp-status.md` promises the model it will get. Renaming or dropping one is
 * a prompt change, not a refactor.
 *
 * `active` IS THE EFFECTIVE GRANT, NOT THE FILE. For an ordinary session it is that
 * session's activations plus the global ones, expired entries already filtered
 * (`config.ts`). For a subagent it is the grant it INHERITED from its spawner, which
 * is the only true answer — a subagent has no activations of its own, and reporting
 * the file would tell it that it may call nothing while `mcp()` happily works
 * (`manager.ts`).
 *
 * SECRETS NEVER APPEAR HERE. `registry` carries `env` values verbatim, which are
 * `${VAR}` references, never expanded (`config.ts`) — this response is rendered in a
 * UI and read by the model, and an expanded token would land in both. `auth` is one
 * boolean per remote server and never a token (`oauth.ts`).
 *
 * WHY THE HTTP HANDLERS LIVE IN THIS FILE. They are the same state, mutated: the
 * registry, the grants and the connections, each answering with a body built by
 * `mcpStatusFor`. They import nothing from `server/` at runtime — the `Handler` type
 * is a type-only import, erased at compile time — so `hostfn/mcp.ts` can depend on
 * this module without dragging the server's import graph (and its `app.ts` ↔
 * `sessions.ts` cycle) into a host function. The route entries themselves are
 * appended to `server/app.ts`, which is the only file that owns the table. The three
 * `/mcp/servers/:name/auth` verbs are NOT here: they are T7.2's (`mcp/oauth.ts`),
 * and this file consumes that module's `hasTokens` rather than restating it.
 *
 * Ported from `src/mcp/status.ts` and the MCP handlers in `src/server/app.ts`.
 * Deltas from that port are marked `NOTE:`.
 */
import { McpError, NotFoundError } from "../errors.ts";
import { McpActivationBody } from "../schema/requests.ts";
import type { AppCtx } from "../types.ts";
import {
  type ActivationOptions,
  loadRegistry,
  type McpConfigOptions,
  type Registry,
  removeServer,
  requireServer,
  saveRegistry,
  setActivation,
  ttlToExpires,
  upsertServer,
} from "./config.ts";
import { hasTokens } from "./oauth.ts";
import { type ConnStatus, type GrantCtx, McpManager, mcpManager, resolveGrant } from "./manager.ts";
// Type-only: erased at compile time, so this module has no runtime edge into
// `server/`. See the header.
import type { Handler } from "../server/http.ts";

// ---------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------

/** Exactly the four keys `prompt/mcp-status.md` documents. */
export interface McpStatus {
  registry: Registry;
  /** Remote (`url`) servers only: whether stored credentials exist. Never a token. */
  auth: Record<string, { authorized: boolean }>;
  /** The servers this scope may actually call, right now. */
  active: string[];
  connections: ConnStatus[];
}

/**
 * Whether a remote server has stored credentials.
 *
 * Injected rather than imported at the call site so a test asserts the whole status
 * shape without writing a credential file, and so this module never grows a second
 * opinion about where tokens live — the default is `oauth.ts`'s own accessor (T7.2).
 */
export type AuthLookup = (server: string) => boolean;

export interface McpStatusOptions extends ActivationOptions {
  /** The scope whose grant and connections are reported. Absent = global scope. */
  sessionId?: string;
  /** An inherited grant (a subagent's). Absent = read this session's activations. */
  grant?: string[];
  /** Absent = the process manager. */
  manager?: McpManager;
  /** Absent = `oauth.ts`'s `hasTokens`. */
  auth?: AuthLookup;
}

/**
 * Build the whole MCP state for one scope. Read-only: it never connects, never
 * spawns, and never throws — status must be answerable while everything is broken,
 * because that is exactly when it is asked.
 */
export function mcpStatusFor(opts: McpStatusOptions = {}): McpStatus {
  const config: McpConfigOptions = {
    ...(opts.file ? { file: opts.file } : {}),
    ...(opts.env ? { env: opts.env } : {}),
  };
  const registry = loadRegistry(config);
  const auth = opts.auth ?? ((server: string) => hasTokens(server));
  const manager = opts.manager ?? mcpManager();
  const grantCtx: GrantCtx = {
    sessionId: opts.sessionId ?? "",
    ...(opts.grant !== undefined ? { mcpGrant: opts.grant } : {}),
  };
  return {
    registry,
    auth: Object.fromEntries(
      Object.entries(registry.servers)
        .filter(([, cfg]) => cfg.url)
        .map(([name]) => [name, { authorized: safely(auth, name) }]),
    ),
    // `resolveGrant` reads the file when nothing was inherited, and an empty
    // `sessionId` is the global scope — so a status asked for no session reports
    // exactly the grants every session has (`config.ts`).
    active: resolveGrant(grantCtx, opts),
    connections: manager.statuses(opts.sessionId),
  };
}

/** A credential store that throws must not take the status down with it. */
function safely(auth: AuthLookup, server: string): boolean {
  try {
    return auth(server);
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// The HTTP surface
// ---------------------------------------------------------------------------

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

/** The `?session=` scope, validated against the database when present. */
function scopeOf(req: Request, ctx: AppCtx): string | undefined {
  const sessionId = new URL(req.url).searchParams.get("session") ?? undefined;
  if (sessionId && !ctx.db.getSession(sessionId)) {
    throw new NotFoundError(`no session ${sessionId} — GET /sessions lists them.`);
  }
  return sessionId;
}

/** Where a server spawned for this session runs: its checkout, like every turn's. */
function workspaceOf(ctx: AppCtx, sessionId: string): string {
  return ctx.db.getSessionRuntime(sessionId).workspace ?? Deno.cwd();
}

async function bodyOf(req: Request): Promise<unknown> {
  return await req.json().catch(() => null);
}

function stateOf(sessionId?: string): McpStatus {
  return mcpStatusFor(sessionId ? { sessionId } : {});
}

/** `GET /mcp/servers[?session=]` — the whole state, for the UI and the `/mcp` flow. */
export const getMcpServersH: Handler = (req, ctx) => jsonResponse(stateOf(scopeOf(req, ctx)));

/**
 * `PUT /mcp/servers` — replace the whole registry (GET → edit → PUT).
 *
 * Only entries that CHANGED lose their connections. A bulk edit that reset every
 * session's unrelated servers would make one typo cost every open session its live
 * MCP state; grants are untouched either way (`saveRegistry` merges them back, and
 * drops only the ones naming a server that no longer exists).
 */
export const putMcpServersH: Handler = async (req, ctx) => {
  const body = await bodyOf(req);
  if (body === null) {
    throw new McpError(400, 'the body must be the registry document: {"servers": {…}}.');
  }
  const before = loadRegistry().servers;
  const registry = saveRegistry(body);
  const changed = [...new Set([...Object.keys(before), ...Object.keys(registry.servers)])]
    .filter((name) => JSON.stringify(before[name]) !== JSON.stringify(registry.servers[name]))
    .sort();
  for (const name of changed) await mcpManager().dropServer(name);
  return jsonResponse({ ...stateOf(scopeOf(req, ctx)), changed });
};

/**
 * `PUT /mcp/servers/:name` — register or update ONE entry.
 *
 * The shape the `/mcp` flow uses, so a registration cannot mangle sibling entries
 * (or their `${VAR}` secret references) in a read-modify-write of the whole file.
 *
 * NOTE (port): validated by `config.ts`'s `ServerConfig` rather than by
 * `schema/requests.ts`'s `PutMcpServerBody`. The latter is a strict subset — it
 * would silently STRIP a stdio entry's `cwd` — and its own doc comment defers: "the
 * MCP module owns the full validation". One schema, and it is the one the file is
 * actually written with.
 */
export const putMcpServerH: Handler = async (req, ctx, params) => {
  const body = await bodyOf(req);
  if (body === null) {
    throw new McpError(
      400,
      `the body must be one server entry: {"command": "…", "args": []} for a local ` +
        `server, or {"url": "https://…"} for a remote one.`,
    );
  }
  upsertServer(params.name, body);
  // A changed definition cannot keep serving from the old one.
  await mcpManager().dropServer(params.name);
  return jsonResponse(stateOf(scopeOf(req, ctx)));
};

/** `DELETE /mcp/servers/:name` — remove the entry, its grants, and its connections. */
export const deleteMcpServerH: Handler = async (req, ctx, params) => {
  if (!removeServer(params.name)) {
    throw new NotFoundError(
      `no MCP server named "${params.name}" is registered, so there is nothing to remove.`,
    );
  }
  await mcpManager().dropServer(params.name);
  return jsonResponse(stateOf(scopeOf(req, ctx)));
};

/**
 * `POST /mcp/servers/:name/connect?session=` — connect now and report the catalog.
 *
 * The "prove it" step: without it, a registration or a grant could only be tested by
 * starting a turn, and a typo'd command surfaced a turn later as an unavailable
 * server. Connecting is NOT a grant — `mcp()` checks the grant on every call
 * (`manager.ts`) — so this proves the command works and nothing more.
 *
 * A server that fails to start answers 200 with `connected: false` and the reason.
 * That is not a swallowed error: the request succeeded, and "this server is broken,
 * here is why" is the answer it asked for. The same reason appears in `connections`
 * as a `failed` row, so the next `mcpStatus()` says it too.
 */
export const connectMcpServerH: Handler = async (req, ctx, params) => {
  const sessionId = scopeOf(req, ctx);
  if (!sessionId) {
    throw new McpError(
      400,
      `connecting is per-session (the server runs in that session's checkout) — ` +
        `pass ?session=<id>.`,
    );
  }
  requireServer(params.name);
  const [catalog] = await mcpManager().ensure(sessionId, [params.name], {
    workspace: workspaceOf(ctx, sessionId),
  });
  return jsonResponse({
    server: params.name,
    connected: catalog.error === undefined,
    ...(catalog.error ? { error: catalog.error } : {}),
    tools: catalog.tools.map((t) => ({
      name: t.name,
      description: (t.description ?? "").split("\n")[0].trim(),
    })),
    ...stateOf(sessionId),
  });
};

/** `POST /mcp/servers/:name/restart?session=` — drop the child and start a new one. */
export const restartMcpServerH: Handler = async (req, ctx, params) => {
  const sessionId = scopeOf(req, ctx);
  if (!sessionId) throw new McpError(400, "restarting is per-session — pass ?session=<id>.");
  requireServer(params.name);
  const restarted = await mcpManager().restart(sessionId, params.name, {
    workspace: workspaceOf(ctx, sessionId),
  });
  return jsonResponse({ restarted, ...stateOf(sessionId) });
};

/**
 * `POST /mcp/servers/:name/enable` and `/disable` — the grant itself.
 *
 * `sessionId: ""` is the GLOBAL scope (`config.ts`), which is why the frozen
 * `McpActivationBody` can require the field: `""` means "every session" rather than
 * "unspecified". `ttl` ("90m" | "2h" | "7d") resolves to an ABSOLUTE expiry, so a
 * grant meant to last two hours cannot be silently extended by a later rewrite of
 * the file.
 *
 * Disabling DROPS the connection: revoking a grant while its subprocess keeps
 * running would leave the thing the human just switched off alive and holding their
 * credentials. Revoking globally drops it for every session, for the same reason.
 *
 * A grant takes effect on the NEXT call, not the next turn — `mcp()` re-resolves the
 * grant per call (`manager.ts`), and an in-flight subagent keeps the snapshot it was
 * spawned with (spec §7).
 */
export const setMcpActivationH = (on: boolean): Handler => async (req, ctx, params) => {
  const parsed = McpActivationBody.safeParse(await bodyOf(req) ?? {});
  if (!parsed.success) {
    throw new McpError(
      400,
      `the body must be {"sessionId": "<id>"} — use "" for the global scope, meaning ` +
        `every session — with an optional {"ttl": "2h"}.`,
    );
  }
  const sessionId = parsed.data.sessionId;
  if (sessionId && !ctx.db.getSession(sessionId)) {
    throw new NotFoundError(`no session ${sessionId} — GET /sessions lists them.`);
  }
  if (on) requireServer(params.name);
  const expires = on && parsed.data.ttl?.trim() ? ttlToExpires(parsed.data.ttl.trim()) : undefined;
  setActivation(sessionId || undefined, params.name, on, expires ? { expires } : {});
  if (!on) {
    if (sessionId) await mcpManager().drop(sessionId, params.name);
    else await mcpManager().dropServer(params.name);
  }
  return jsonResponse({
    scope: sessionId ? { sessionId } : "global",
    ...stateOf(sessionId || undefined),
  });
};
