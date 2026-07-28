/**
 * The MCP connection manager, and the grant that decides who may ask.
 *
 * THE INVARIANT THIS HOLDS: **a turn may call exactly the servers a human granted
 * it — and a subagent doing part of that granted work may call the same set, and
 * nothing else.** Two halves, and both are load-bearing:
 *
 *   1. **Registration is not a grant** (`config.ts`). Every call goes through
 *      `requireGranted`, which reads the grant FRESH: a program cannot enable a
 *      server for itself, and a grant revoked between turns is gone from the very
 *      next check with nothing to sweep.
 *   2. **The grant carries into subagents** (spec §7). A subagent has a fresh,
 *      task-only thread and no activations of its own, so a child that resolved its
 *      own grant would resolve to *nothing* and every delegated MCP task would die
 *      at the first tool call — while a child that re-read the file would pick up
 *      grants made after it was spawned. Neither is what the human authorized. So
 *      the grant is CAPTURED AT SPAWN: `bindTurnGrant` installs `mcpGrant` on a
 *      top-level turn's ctx as a live read, `agents/subagent.ts` copies its value
 *      into the child's ctx (a plain array — a snapshot), and `resolveGrant` treats
 *      an inherited array as the authority. A later manual continuation of that
 *      branch starts from the server's own `AppCtx`, carries no `mcpGrant`, and so
 *      inherits nothing (`types.ts`).
 *
 * NOTHING HERE IS CACHED (plan §6.13). The registry and the activations are re-read
 * per operation, connections are consulted live, and `statuses()` reports what the
 * process actually holds at the instant it is called. `mcpStatus()` is the model's
 * only truthful source of MCP state, and a status served from a cache is how a model
 * ends up confidently calling a tool that was revoked two turns ago. The only thing
 * kept between calls is the connection itself, which is a live process, not an
 * answer.
 *
 * A SERVER THAT DOES NOT WORK IS A NAMED STATUS, NEVER A HANG (plan T7.1). The
 * client already bounds every path out of a broken server (`client.ts`); this layer
 * adds the part the model sees: a failed connect is REMEMBERED per (session, server)
 * and reported by `statuses()` as `state: "failed"` with the reason, so a down
 * server degrades to a line in `mcpStatus()` rather than to an exception the model
 * has to have caught, or to a spinner. A connection whose child died since the last
 * call reports `state: "exited"` and is respawned on the next use.
 *
 * CONNECTIONS ARE PER (SESSION, SERVER). Two sessions working on different checkouts
 * must not share one child process: the child's cwd is the session's workspace, and
 * a filesystem-backed server handed the wrong tree answers about the wrong project.
 * Idle connections are reaped opportunistically on use — no background timer, so a
 * quiet server holds a subprocess for at most `IDLE_MS` past its last call and the
 * process has nothing to shut down.
 *
 * WHAT IS NOT HERE. This module connects, calls and reports. It does not decide what
 * a turn is told about MCP (that is `prompt/assemble.ts`), it does not own the
 * registry file (`config.ts`), and it does not speak HTTP (`status.ts`).
 *
 * Ported from `src/mcp/manager.ts`. Deltas from that port are marked `NOTE:`.
 */
import { McpError } from "../errors.ts";
import type { TurnCtx } from "../types.ts";
import {
  type McpCallResult,
  type McpConnection,
  McpStdioClient,
  type McpTimeouts,
  type McpToolInfo,
} from "./client.ts";
import {
  activationsFor,
  childEnv,
  expandHeaders,
  isStdio,
  loadRegistry,
  type McpConfigOptions,
  requireServer,
  type ServerConfig,
} from "./config.ts";
import { McpRemoteClient } from "./remote.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/** Reap a connection this long after its last use (on the next manager touch). */
export const IDLE_MS = 30 * 60_000;

/** What a spawned server needs from the turn that wants it. */
export interface SpawnCtx {
  /** The child's cwd — the session's checkout, so a filesystem server sees it. */
  workspace: string;
}

/** One server's connect outcome: its tools, or the sentence explaining why none. */
export interface ServerCatalog {
  name: string;
  tools: McpToolInfo[];
  error?: string;
}

/**
 * Why a (session, server) pair is not usable, in one word.
 *
 * NOTE (port): the old status carried only `alive: boolean`, which cannot tell "it
 * never started" from "it started and died" — and a server that failed to start had
 * no status row at all, so the one case the model most needs to see was invisible.
 */
export type McpConnState = "connected" | "exited" | "failed";

/** One (session, server) pair as `mcpStatus()` and `GET /mcp/servers` report it. */
export interface ConnStatus {
  server: string;
  sessionId: string;
  state: McpConnState;
  alive: boolean;
  toolCount: number;
  /**
   * Tool names, so `mcpStatus()` carries a callable catalog. The turn-start prompt
   * catalog renders full signatures (`prompt/assemble.ts`); this is the live answer
   * to "what can I call right now", which is the question the model is told to ask
   * from a fresh call rather than from memory.
   */
  tools: string[];
  lastUsed: number;
  /** Present when `state` is not `connected`: what failed, and what resolves it. */
  error?: string;
  stderrTail?: string;
}

interface Conn {
  client: McpConnection;
  tools: McpToolInfo[];
  spawn: SpawnCtx;
  sessionId: string;
  server: string;
  lastUsed: number;
}

/** A remembered connect failure — the reason a status row exists with no connection. */
interface Failure {
  sessionId: string;
  server: string;
  error: string;
  at: number;
}

/**
 * How a registry entry becomes a live connection. Injected so a test drives the
 * whole manager — grants, catalogs, degradation — against a fake transport, and so
 * the remote transport (T7.2) can be installed without this file knowing about it.
 */
export type Connector = (spec: {
  name: string;
  server: ServerConfig;
  spawn: SpawnCtx;
  config: McpConfigOptions;
  timeouts?: McpTimeouts;
}) => Promise<McpConnection>;

export interface McpManagerOptions {
  /** Where the registry and grants live, and where `${VAR}` comes from. */
  config?: McpConfigOptions;
  /** Injected clock, epoch ms. Absent = `Date.now()`. */
  now?: () => number;
  /** Absent = spawn a stdio child (`client.ts`). */
  connect?: Connector;
  /** Client deadlines. A test turns these down so a no-hang assertion is fast. */
  timeouts?: McpTimeouts;
  /** Idle reap window. Absent = `IDLE_MS`. */
  idleMs?: number;
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/** The ctx fields a grant decision needs. Narrow so a test needs no turn. */
export interface GrantCtx {
  sessionId: string;
  /** Inherited from the spawning turn. `[]` means "granted nothing", not "unset". */
  mcpGrant?: string[];
}

/**
 * The servers this turn may call, resolved fresh.
 *
 * An INHERITED grant wins outright — including an empty one, which is why the test
 * is `!== undefined` and not truthiness. A subagent spawned by an ungranted turn
 * must stay ungranted rather than falling through to the global scope and quietly
 * acquiring servers its spawner never had.
 */
export function resolveGrant(ctx: GrantCtx, opts: McpConfigOptions = {}): string[] {
  return ctx.mcpGrant !== undefined ? [...ctx.mcpGrant] : activationsFor(ctx.sessionId, opts);
}

/**
 * Make a top-level turn's grant readable — and therefore inheritable — without
 * freezing it.
 *
 * `agents/subagent.ts` copies `ctx.mcpGrant` into the child at spawn, so a turn
 * whose ctx has no `mcpGrant` hands its subagents nothing and every delegated MCP
 * task fails at the first call. Setting a plain array here would fix that and break
 * the other half: the array would be resolved once at turn start, and a grant
 * revoked mid-turn would keep working until the turn ended.
 *
 * So the property is installed as a LIVE READ. Every access re-reads the
 * activations, which makes a revocation visible to the very next `mcpStatus()` call;
 * and the one access that matters for inheritance — the spawn — copies the value out
 * as a plain array, which is exactly the spawn-time snapshot spec §7 describes.
 *
 * Idempotent, and it never overwrites an inherited grant: a subagent's ctx already
 * carries its spawner's snapshot, and re-deriving it from the child's own (empty)
 * activations would revoke it.
 */
export function bindTurnGrant(ctx: TurnCtx, opts: McpConfigOptions = {}): TurnCtx {
  if (ctx.mcpGrant !== undefined) return ctx;
  Object.defineProperty(ctx, "mcpGrant", {
    get: () => activationsFor(ctx.sessionId, opts),
    enumerable: true,
    configurable: true,
  });
  // Marks the grant as this session's OWN live read rather than an inherited
  // snapshot. Without it a bound ctx is indistinguishable from a subagent's — both
  // have an `mcpGrant` — and a top-level turn would be told it "inherited a grant it
  // cannot widen", which is both false and the opposite of the move that fixes it.
  // Non-enumerable so a spread never carries it into a child's ctx.
  Object.defineProperty(ctx, LIVE_GRANT, { value: true, enumerable: false, configurable: true });
  return ctx;
}

/** Set by {@link bindTurnGrant}. Present = the grant is read live, not inherited. */
const LIVE_GRANT = Symbol.for("bough.mcp.liveGrant");

/** True when this ctx holds a grant handed down from a spawner (spec §7). */
function isInherited(ctx: GrantCtx): boolean {
  return ctx.mcpGrant !== undefined && !(LIVE_GRANT in Object(ctx));
}

/**
 * Throw unless this turn may call `server`.
 *
 * Three distinct outcomes, because collapsing them costs the model a round: a name
 * that is not registered at all (404, naming what is), a registered server nobody
 * granted (403, naming what *is* granted and who can grant it), and a pass. A
 * program cannot grant itself a server — saying so is what stops the next round
 * being spent trying.
 */
export function requireGranted(
  ctx: GrantCtx,
  server: string,
  opts: McpConfigOptions = {},
): void {
  requireServer(server, opts); // 404 with the registered names
  const grant = resolveGrant(ctx, opts);
  if (grant.includes(server)) return;
  const inherited = isInherited(ctx);
  throw new McpError(
    403,
    `MCP server "${server}" is registered but not granted to this turn. ` +
      (grant.length > 0 ? `Granted here: ${grant.join(", ")}. ` : `Nothing is granted here. `) +
      (inherited
        ? `This session inherited its spawner's grant and cannot widen it — ` +
          `report what you could not do rather than retrying.`
        : `A human grants one from /mcp (POST /mcp/servers/${server}/enable); ` +
          `a program cannot grant itself one. Say what you could not do and move on.`),
  );
}

// ---------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------

export class McpManager {
  /** `${sessionId} ${server}` → live connection. */
  #conns = new Map<string, Conn>();
  /** In-flight connects, so concurrent callers share one spawn. */
  #connecting = new Map<string, Promise<Conn>>();
  /** `${sessionId} ${server}` → the last connect failure, for the status surface. */
  #failures = new Map<string, Failure>();
  #opts: McpManagerOptions;

  constructor(opts: McpManagerOptions = {}) {
    this.#opts = opts;
  }

  get #now(): number {
    return (this.#opts.now ?? Date.now)();
  }

  get config(): McpConfigOptions {
    return this.#opts.config ?? {};
  }

  /**
   * Connect (or reuse) each named server and report its catalog.
   *
   * A server that cannot connect yields an `error` entry instead of throwing: one
   * broken server must not take the other three down with it, and the failure is
   * recorded so `statuses()` reports it too.
   */
  async ensure(
    sessionId: string,
    servers: readonly string[],
    spawn: SpawnCtx,
  ): Promise<ServerCatalog[]> {
    this.#sweep();
    const registry = loadRegistry(this.config).servers;
    return await Promise.all(servers.map(async (name): Promise<ServerCatalog> => {
      const cfg = registry[name];
      if (!cfg) {
        const error = `not registered — register it with PUT /mcp/servers/${name}`;
        this.#recordFailure(sessionId, name, error);
        return { name, tools: [], error };
      }
      try {
        const conn = await this.#acquire(sessionId, name, cfg, spawn);
        return { name, tools: conn.tools };
      } catch (e) {
        return { name, tools: [], error: messageOf(e) };
      }
    }));
  }

  /**
   * Invoke one tool, connecting on demand.
   *
   * NOTE (port): the old manager REQUIRED an `ensure()` from turn start and failed a
   * call to anything else with "not connected for this session" — a message that
   * described bough's own bookkeeping rather than anything the program could act on.
   * Connecting lazily means a granted server is callable the moment it is granted,
   * and the only failures left are real ones: the server does not start, the tool
   * does not exist, or the tool itself failed.
   *
   * An `isError` result THROWS with the server's own text, so it rejects inside the
   * program like any other host-fn failure.
   */
  async call(
    sessionId: string,
    server: string,
    tool: string,
    args: unknown,
    spawn: SpawnCtx,
  ): Promise<unknown> {
    this.#sweep();
    const conn = await this.#live(sessionId, server, spawn);
    conn.lastUsed = this.#now;
    const known = conn.tools.find((t) => t.name === tool);
    if (!known) {
      const names = conn.tools.map((t) => t.name);
      throw new McpError(
        404,
        `MCP server "${server}" has no tool "${tool}". It advertises: ` +
          (names.length > 0 ? names.join(", ") : "(none)") +
          `. Call mcpStatus() for the live catalog rather than guessing a name.`,
      );
    }
    return mapResult(server, tool, await conn.client.callTool(tool, args));
  }

  /** Drop and re-establish one (session, server) connection. */
  async restart(sessionId: string, server: string, spawn?: SpawnCtx): Promise<ConnStatus> {
    const previous = this.#conns.get(key(sessionId, server));
    const where = spawn ?? previous?.spawn;
    if (!where) {
      throw new McpError(
        400,
        `MCP server "${server}" has no connection for this session to restart, and no ` +
          `workspace to start one in. Connect it first (POST /mcp/servers/${server}/connect).`,
      );
    }
    await this.drop(sessionId, server);
    const cfg = requireServer(server, this.config);
    try {
      return statusOf(await this.#acquire(sessionId, server, cfg, where));
    } catch (e) {
      const failed = this.#failures.get(key(sessionId, server));
      if (failed) return failureStatus(failed);
      throw e;
    }
  }

  /**
   * Live rows for one session (or every session), plus the failures that explain a
   * server with no connection. Never connects and never throws — status is a read.
   */
  statuses(sessionId?: string): ConnStatus[] {
    const rows: ConnStatus[] = [];
    for (const [k, conn] of this.#conns) {
      if (sessionId !== undefined && conn.sessionId !== sessionId) continue;
      rows.push(statusOf(conn));
      this.#failures.delete(k); // a live connection supersedes an old failure
    }
    for (const [k, failure] of this.#failures) {
      if (sessionId !== undefined && failure.sessionId !== sessionId) continue;
      if (this.#conns.has(k)) continue;
      rows.push(failureStatus(failure));
    }
    return rows.sort((a, b) => a.server.localeCompare(b.server));
  }

  /** Close one session's connection to one server. No-op when there is none. */
  async drop(sessionId: string, server: string): Promise<void> {
    const k = key(sessionId, server);
    const conn = this.#conns.get(k);
    this.#failures.delete(k);
    if (!conn) return;
    this.#conns.delete(k);
    await conn.client.close();
  }

  /**
   * Close every session's connection to one server — a registry edit, a removal, or
   * cleared auth. A changed entry must not keep serving from the old definition.
   */
  async dropServer(server: string): Promise<void> {
    const live = [...this.#conns.entries()].filter(([, c]) => c.server === server);
    for (const [k] of live) this.#conns.delete(k);
    for (const [k, f] of [...this.#failures]) if (f.server === server) this.#failures.delete(k);
    await Promise.all(live.map(([, c]) => c.client.close()));
  }

  /** Close everything. Shutdown, and the `finally` of every test that connects. */
  async dropAll(): Promise<void> {
    const live = [...this.#conns.values()];
    this.#conns.clear();
    this.#failures.clear();
    await Promise.all(live.map((c) => c.client.close()));
  }

  // -- internals -----------------------------------------------------------

  /** An alive connection, reconnecting a dead or absent one. */
  async #live(sessionId: string, server: string, spawn: SpawnCtx): Promise<Conn> {
    const existing = this.#conns.get(key(sessionId, server));
    if (existing?.client.alive) return existing;
    if (existing) await this.drop(sessionId, server);
    return await this.#acquire(sessionId, server, requireServer(server, this.config), spawn);
  }

  #acquire(
    sessionId: string,
    server: string,
    cfg: ServerConfig,
    spawn: SpawnCtx,
  ): Promise<Conn> {
    const k = key(sessionId, server);
    const live = this.#conns.get(k);
    if (live && live.client.alive) {
      live.lastUsed = this.#now;
      live.spawn = spawn; // a later turn's workspace wins for the next respawn
      return Promise.resolve(live);
    }
    let connecting = this.#connecting.get(k);
    if (!connecting) {
      connecting = this.#connect(sessionId, server, cfg, spawn)
        .then((conn) => {
          this.#conns.set(k, conn);
          this.#failures.delete(k);
          return conn;
        })
        .catch((e: unknown) => {
          this.#recordFailure(sessionId, server, messageOf(e));
          throw e;
        })
        .finally(() => this.#connecting.delete(k));
      this.#connecting.set(k, connecting);
    }
    return connecting;
  }

  async #connect(
    sessionId: string,
    server: string,
    cfg: ServerConfig,
    spawn: SpawnCtx,
  ): Promise<Conn> {
    const connect = this.#opts.connect ?? defaultConnector;
    const client = await connect({
      name: server,
      server: cfg,
      spawn,
      config: this.config,
      ...(this.#opts.timeouts ? { timeouts: this.#opts.timeouts } : {}),
    });
    try {
      const tools = await client.listTools();
      return { client, tools, spawn, sessionId, server, lastUsed: this.#now };
    } catch (e) {
      // A server that connects and cannot list its tools is not usable, and leaving
      // its child running would leak a process nobody can reach.
      await client.close();
      throw e;
    }
  }

  #recordFailure(sessionId: string, server: string, error: string): void {
    this.#failures.set(key(sessionId, server), { sessionId, server, error, at: this.#now });
  }

  /** Reap dead and idle connections. Opportunistic — no timer, no background work. */
  #sweep(): void {
    const now = this.#now;
    const idleMs = this.#opts.idleMs ?? IDLE_MS;
    for (const [k, conn] of [...this.#conns]) {
      if (conn.client.alive && now - conn.lastUsed <= idleMs) continue;
      this.#conns.delete(k);
      void conn.client.close();
    }
  }
}

// ---------------------------------------------------------------------------
// The default transport
// ---------------------------------------------------------------------------

/**
 * Turn one registry entry into a live connection: a spawned child for a stdio
 * entry, the Streamable HTTP transport for a `url` one (T7.2, `remote.ts`).
 *
 * The two clients satisfy the same `McpConnection`, so this is the only place in
 * the manager that knows which kind an entry is — and a 401 from a remote server
 * arrives as `McpAuthRequiredError`, whose message is the "^p, then a" prompt
 * rather than a fault, straight through `ensure`'s catalog error and into the
 * `failed` status row.
 */
export const defaultConnector: Connector = async ({ name, server, spawn, config, timeouts }) => {
  if (!isStdio(server)) {
    return await McpRemoteClient.connect({
      name,
      url: server.url!,
      // Expanded HERE, not at load: a `${VAR}` or `${keychain:…}` reference is
      // resolved at the moment it is sent, so the secret never enters the registry
      // document, the `GET /mcp/servers` body, or the `/mcp` panel (`config.ts`).
      ...(Object.keys(server.headers).length > 0
        ? { headers: await expandHeaders(server.headers, config ?? {}) }
        : {}),
      ...(timeouts ? { timeouts } : {}),
    });
  }
  return await McpStdioClient.connect({
    name,
    argv: [server.command, ...server.args],
    // The entry's own `cwd` wins; otherwise the session's checkout, so a
    // filesystem-backed server sees the tree the turn is working in.
    cwd: server.cwd ?? spawn.workspace,
    // The child's ENTIRE environment, composed by `config.ts` — a third-party binary
    // never inherits the user's provider keys.
    env: childEnv(server, config),
    ...(timeouts ? { timeouts } : {}),
  });
};

// ---------------------------------------------------------------------------
// The process-wide manager
// ---------------------------------------------------------------------------

/**
 * One manager per process, because a connection is a live subprocess: the turn
 * runner, the host function and the HTTP endpoints all have to reach the SAME child,
 * or `POST /mcp/servers/x/connect` would prove a server works in a process nobody
 * else can see. Tests construct their own `McpManager` with an injected connector
 * and never touch this one.
 */
let active: McpManager | undefined;

export function mcpManager(): McpManager {
  active ??= new McpManager();
  return active;
}

/** Swap the process manager (tests, and the boot wiring). Returns the previous one. */
export function setMcpManager(next: McpManager): McpManager {
  const previous = mcpManager();
  active = next;
  return previous;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function key(sessionId: string, server: string): string {
  return `${sessionId} ${server}`;
}

function statusOf(conn: Conn): ConnStatus {
  const tail = conn.client.stderrTail.trim();
  const alive = conn.client.alive;
  return {
    server: conn.server,
    sessionId: conn.sessionId,
    state: alive ? "connected" : "exited",
    alive,
    toolCount: conn.tools.length,
    tools: conn.tools.map((t) => t.name),
    lastUsed: conn.lastUsed,
    ...(alive ? {} : { error: "the server process exited; the next call restarts it" }),
    ...(tail ? { stderrTail: tail.slice(-500) } : {}),
  };
}

function failureStatus(failure: Failure): ConnStatus {
  return {
    server: failure.server,
    sessionId: failure.sessionId,
    state: "failed",
    alive: false,
    toolCount: 0,
    tools: [],
    lastUsed: failure.at,
    error: failure.error,
  };
}

/** Text of a call result's content blocks. */
function textOf(result: McpCallResult): string {
  return (result.content ?? [])
    .filter((c) => c.type === "text" && typeof c.text === "string")
    .map((c) => c.text)
    .join("\n");
}

/**
 * A tool result as the program sees it: the structured content when the server sent
 * some, otherwise its text. A tool that FAILED throws with the server's own words —
 * the program can catch it, and the sentence names the server and the tool so the
 * next round is not spent asking which one broke.
 */
function mapResult(server: string, tool: string, result: McpCallResult): unknown {
  if (result.isError) {
    throw new McpError(
      502,
      `MCP ${server}:${tool} failed: ${
        textOf(result) || "the server reported an error with no text"
      }`,
    );
  }
  return result.structuredContent ?? textOf(result);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
