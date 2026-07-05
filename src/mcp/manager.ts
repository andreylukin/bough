/**
 * The MCP connection manager: one stdio client per (session, server), spawned under
 * the SAME confinement as a bash child — seatbelt-wrapped (writes → workspace +
 * session snapshot dir, loopback-only network when the proxy runs) with the
 * session's Claw Patrol env, so the server's own egress is proxied and attributed.
 * The child's environment is minimal and explicit: PATH/HOME, the registry entry's
 * declared env (${VAR}-expanded — secrets reach the child only), and the proxy env.
 *
 * Connections are cached across turns and reaped when idle (opportunistic sweep on
 * use — no background timer), on restart/disable, or on a registry change. Every
 * call() is gated through Claw Patrol (mcp/gate.ts) BEFORE it reaches the server.
 *
 * Grant scoping (which servers a turn may call) is the turn runner's job — the
 * manager connects and executes; it does not decide who may ask.
 */
import { wrap } from "../sandbox/seatbelt.ts";
import { clawpatrolEnv } from "../net/gateway.ts";
import { expandEnv, loadRegistry, type ServerConfig } from "./config.ts";
import {
  type McpCallResult,
  type McpConnection,
  McpStdioClient,
  type McpToolInfo,
} from "./client.ts";
import { McpRemoteClient } from "./remote.ts";
import { gateMcpCall } from "./gate.ts";

/** Reap a connection this long after its last use (next manager touch). */
const IDLE_MS = 30 * 60_000;

export interface SpawnCtx {
  /** Session workspace — the child's cwd and the seatbelt rw root. */
  workspace: string;
  /** Present when the turn is sandboxed (see ToolRunCtx.sandbox). */
  sandbox?: { sessionDir: string };
}

/** One server's turn-start outcome: its tools, or why it has none. */
export interface ServerCatalog {
  name: string;
  tools: McpToolInfo[];
  error?: string;
}

export interface ConnStatus {
  server: string;
  sessionId: string;
  alive: boolean;
  toolCount: number;
  lastUsed: number;
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

export class McpManager {
  /** `${sessionId} ${server}` → live connection. */
  #conns = new Map<string, Conn>();
  /** In-flight connects, so concurrent ensures share one spawn. */
  #connecting = new Map<string, Promise<Conn>>();

  /**
   * Connect (or reuse) each named server for the session and return its tool
   * catalog. A server that can't connect yields an `error` entry instead of
   * throwing — the turn proceeds and the prompt section says what's missing.
   */
  async ensure(
    sessionId: string,
    servers: string[],
    spawn: SpawnCtx,
  ): Promise<ServerCatalog[]> {
    this.#sweep();
    const registry = loadRegistry().servers;
    return await Promise.all(servers.map(async (name): Promise<ServerCatalog> => {
      const cfg = registry[name];
      if (!cfg) {
        return { name, tools: [], error: "not in the registry (~/.bough/mcp/servers.json)" };
      }
      try {
        const conn = await this.#acquire(sessionId, name, cfg, spawn);
        return { name, tools: conn.tools };
      } catch (e) {
        return { name, tools: [], error: (e as Error).message };
      }
    }));
  }

  /**
   * Execute one tool call: gate it through Claw Patrol, then invoke the server.
   * Reconnects a dead server once (same spawn params as its last ensure). An
   * `isError` result throws — inside the program it rejects like any host-fn error.
   */
  async call(sessionId: string, server: string, tool: string, args: unknown): Promise<unknown> {
    let conn = this.#conns.get(key(sessionId, server));
    if (!conn) throw new Error(`mcp server "${server}" is not connected for this session`);
    if (!conn.client.alive) {
      conn = await this.#respawn(conn);
    }
    conn.lastUsed = Date.now();
    const info = conn.tools.find((t) => t.name === tool);
    if (!info) {
      const names = conn.tools.map((t) => t.name).join(", ");
      throw new Error(`mcp server "${server}" has no tool "${tool}" (has: ${names})`);
    }
    await gateMcpCall(sessionId, server, tool, args, info.annotations);
    const result = await conn.client.callTool(tool, args);
    return mapResult(server, tool, result);
  }

  /** Drop and re-establish a session's connection to one server. */
  async restart(sessionId: string, server: string): Promise<ConnStatus> {
    const conn = this.#conns.get(key(sessionId, server));
    if (!conn) throw new Error(`mcp server "${server}" is not connected for this session`);
    return status(await this.#respawn(conn));
  }

  /** Live connections (optionally for one session) — the /mcp status surface. */
  statuses(sessionId?: string): ConnStatus[] {
    return [...this.#conns.values()]
      .filter((c) => sessionId === undefined || c.sessionId === sessionId)
      .map(status);
  }

  /** Close a session's connection to one server (disable). No-op when absent. */
  async drop(sessionId: string, server: string): Promise<void> {
    const conn = this.#conns.get(key(sessionId, server));
    if (!conn) return;
    this.#conns.delete(key(sessionId, server));
    await conn.client.close();
  }

  /** Close every session's connection to one server (logout / auth cleared). */
  async dropServer(server: string): Promise<void> {
    const live = [...this.#conns.entries()].filter(([, c]) => c.server === server);
    for (const [k] of live) this.#conns.delete(k);
    await Promise.all(live.map(([, c]) => c.client.close()));
  }

  /** Close everything (registry change, tests, shutdown). */
  async dropAll(): Promise<void> {
    const live = [...this.#conns.values()];
    this.#conns.clear();
    await Promise.all(live.map((c) => c.client.close()));
  }

  #acquire(sessionId: string, server: string, cfg: ServerConfig, spawn: SpawnCtx): Promise<Conn> {
    const k = key(sessionId, server);
    const live = this.#conns.get(k);
    if (live && live.client.alive) {
      live.lastUsed = Date.now();
      live.spawn = spawn; // a later turn's workspace wins for the next respawn
      return Promise.resolve(live);
    }
    let connecting = this.#connecting.get(k);
    if (!connecting) {
      connecting = this.#connect(sessionId, server, cfg, spawn)
        .then((conn) => {
          this.#conns.set(k, conn);
          return conn;
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
    // Remote server: the SDK transport + OAuth provider (remote.ts). No spawn, no
    // seatbelt — the call-layer gate is the border; 401 surfaces as "not authorized".
    if (cfg.url) {
      const client = await McpRemoteClient.connect({ server, url: cfg.url });
      try {
        const tools = await client.listTools();
        return { client, tools, spawn, sessionId, server, lastUsed: Date.now() };
      } catch (e) {
        await client.close();
        throw e;
      }
    }
    // Same egress routing as a bash child: this session's proxy + the MITM CA.
    const netEnv = await clawpatrolEnv(sessionId);
    let argv = [cfg.command!, ...cfg.args];
    if (spawn.sandbox && Deno.build.os === "darwin" && Deno.env.get("BOUGH_NO_SANDBOX") !== "1") {
      argv = wrap(argv, {
        workspace: spawn.workspace,
        allowWrite: [spawn.sandbox.sessionDir],
        confineNetwork: Object.keys(netEnv).length > 0,
      });
    }
    const home = Deno.env.get("HOME");
    const client = await McpStdioClient.connect({
      argv,
      cwd: spawn.workspace,
      env: {
        PATH: Deno.env.get("PATH") ?? "/usr/bin:/bin",
        ...(home ? { HOME: home } : {}),
        ...expandEnv(cfg.env),
        ...netEnv,
      },
    });
    try {
      const tools = await client.listTools();
      return { client, tools, spawn, sessionId, server, lastUsed: Date.now() };
    } catch (e) {
      await client.close();
      throw e;
    }
  }

  async #respawn(conn: Conn): Promise<Conn> {
    await this.drop(conn.sessionId, conn.server);
    const cfg = loadRegistry().servers[conn.server];
    if (!cfg) throw new Error(`mcp server "${conn.server}" is no longer in the registry`);
    return await this.#acquire(conn.sessionId, conn.server, cfg, conn.spawn);
  }

  /** Reap idle connections. Opportunistic (no timer): runs on each ensure(). */
  #sweep(now = Date.now()): void {
    for (const [k, conn] of [...this.#conns]) {
      if (!conn.client.alive || now - conn.lastUsed > IDLE_MS) {
        this.#conns.delete(k);
        void conn.client.close();
      }
    }
  }
}

function key(sessionId: string, server: string): string {
  return `${sessionId} ${server}`;
}

function status(conn: Conn): ConnStatus {
  const tail = conn.client.stderrTail.trim();
  return {
    server: conn.server,
    sessionId: conn.sessionId,
    alive: conn.client.alive,
    toolCount: conn.tools.length,
    lastUsed: conn.lastUsed,
    ...(tail ? { stderrTail: tail.slice(-500) } : {}),
  };
}

/** Text of a call result's content blocks. */
function textOf(result: McpCallResult): string {
  return (result.content ?? [])
    .filter((c) => c.type === "text" && typeof c.text === "string")
    .map((c) => c.text)
    .join("\n");
}

function mapResult(server: string, tool: string, result: McpCallResult): unknown {
  if (result.isError) {
    throw new Error(textOf(result) || `mcp ${server}:${tool} returned an error`);
  }
  return result.structuredContent ?? textOf(result);
}

// The process-wide manager (mirrors net/gateway.ts's active-gateway pattern): the
// turn runner and the HTTP endpoints share connections without threading the
// instance through every signature. Tests construct their own McpManager.
let active: McpManager | undefined;
export function mcpManager(): McpManager {
  active ??= new McpManager();
  return active;
}
